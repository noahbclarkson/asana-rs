//! The one path every operation takes: build the request, wait for the limits, send, retry what
//! the policy allows, map the status, and decode the body.

use std::time::Duration;

use asana_openapi::endpoint::{Endpoint, Envelope, Method, NextPage, Paginated, RequestBody};
use bytes::Bytes;
use http::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, USER_AGENT};
use http::{HeaderMap, HeaderValue, StatusCode};
use secrecy::ExposeSecret;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use tokio::time::Instant;
use tracing::Instrument;
use tracing::field::Empty;

use crate::auth::{AccessToken, AuthError};
use crate::client::Client;
use crate::error::{DecodeError, Error};
use crate::limits::Admission;
use crate::multipart;
use crate::page::Page;
use crate::retry::{retry_after, retryable_status, safe_to_repeat};
use crate::transport::RequestTimeout;
use crate::transport::TransportError;

/// A successful answer, before its body is decoded.
struct Answer {
    body: Bytes,
}

/// Where a call stands between attempts.
struct Attempts {
    /// Whether sending the request twice is safe.
    repeatable: bool,
    /// Whether the request is a `DELETE`.
    deletes: bool,
    /// How many times it has been sent.
    made: u32,
    /// Whether a rejected OAuth token has already been replaced once.
    refreshed: bool,
    /// Whether an earlier attempt may have been carried out without its answer arriving (a
    /// `5xx`, or a transport failure after the request may have reached Asana).
    maybe_applied: bool,
}

/// What happens after an attempt.
enum Step {
    /// The call is over.
    Done(Result<Answer, Error>),
    /// Send again after this wait.
    Again(Duration),
}

/// A request's parts that stay the same on every attempt.
struct Prepared {
    operation: &'static str,
    method: http::Method,
    url: String,
    content_type: Option<HeaderValue>,
    body: Bytes,
}

impl Client {
    /// Sends one operation and returns its result.
    ///
    /// A request is sent again only as the [`RetryPolicy`](crate::RetryPolicy) and the operation
    /// allow: a request that creates something is never sent again once Asana may have carried it
    /// out ([`Error::outcome`] is then [`Outcome::Unknown`](crate::Outcome::Unknown)). A `DELETE`
    /// sent again after such an attempt and answered `404` counts as deleted.
    ///
    /// # Errors
    ///
    /// Any [`Error`]: what Asana answered, a body that could not be read, or a request that never
    /// got an answer, after the retries the [`RetryPolicy`](crate::RetryPolicy) allows.
    pub async fn send<E: Endpoint>(&self, endpoint: E) -> Result<E::Output, Error> {
        let answer = self.execute(&endpoint, None, None).await?;
        decode_output::<E>(&answer.body)
    }

    /// Sends one operation that counts against the limits as `admission` rather than as one
    /// request: a batch, which Asana counts action by action.
    pub(crate) async fn send_admitting<E: Endpoint>(
        &self,
        endpoint: E,
        admission: Admission,
    ) -> Result<E::Output, Error> {
        let answer = self.execute(&endpoint, None, Some(admission)).await?;
        decode_output::<E>(&answer.body)
    }

    /// Sends one operation, asking the transport to allow `timeout` in all for each attempt in
    /// place of its own default. For the few operations that stay open on purpose, such as
    /// creating a webhook while Asana calls the receiver.
    ///
    /// # Errors
    ///
    /// As [`Client::send`].
    pub async fn send_with_timeout<E: Endpoint>(
        &self,
        endpoint: E,
        timeout: Duration,
    ) -> Result<E::Output, Error> {
        let answer = self.execute(&endpoint, Some(timeout), None).await?;
        decode_output::<E>(&answer.body)
    }

    /// Sends one page of a paged list and returns the page with where the next one starts.
    ///
    /// The page size and offset are whatever the descriptor holds.
    ///
    /// # Errors
    ///
    /// As [`Client::send`].
    pub async fn send_page<E: Paginated>(&self, endpoint: E) -> Result<Page<E::Item>, Error> {
        self.page_of(&endpoint).await
    }

    pub(crate) async fn page_of<E: Paginated>(&self, endpoint: &E) -> Result<Page<E::Item>, Error> {
        let answer = self.execute(endpoint, None, None).await?;
        let page: PageEnvelope<E::Item> = decode_json(E::OPERATION_ID, &answer.body)?;
        Ok(Page::new(page.data, page.next_page.map(|next| next.offset)))
    }

    /// Runs the pipeline for one operation inside its tracing span.
    async fn execute<E: Endpoint>(
        &self,
        endpoint: &E,
        timeout: Option<Duration>,
        admission: Option<Admission>,
    ) -> Result<Answer, Error> {
        let span = tracing::debug_span!(
            "asana.request",
            operation = E::OPERATION_ID,
            method = E::METHOD.as_str(),
            route = E::ROUTE,
            status = Empty,
            attempt = Empty,
            wait_ms = Empty,
        );
        let admission = admission.unwrap_or_else(|| Admission::of(E::METHOD, E::OPERATION_ID));
        self.attempt_until_done(endpoint, timeout, admission, &span)
            .instrument(span.clone())
            .await
    }

    async fn attempt_until_done<E: Endpoint>(
        &self,
        endpoint: &E,
        timeout: Option<Duration>,
        admission: Admission,
        span: &tracing::Span,
    ) -> Result<Answer, Error> {
        let inner = &self.inner;
        let prepared = self.prepare(endpoint)?;
        let mut state = Attempts {
            repeatable: safe_to_repeat(E::METHOD, E::IDEMPOTENT),
            deletes: E::METHOD == Method::Delete,
            made: 0,
            refreshed: false,
            maybe_applied: false,
        };
        let mut waited = Duration::ZERO;
        let outcome = loop {
            let queued_at = Instant::now();
            let admitted = inner.governor.admit(admission, inner.retry.max_wait).await;
            waited += queued_at.elapsed();
            let permit = match admitted {
                Ok(permit) => permit,
                Err(retry_after) => {
                    break Err(Error::RateLimited {
                        retry_after,
                        attempts: state.made,
                    });
                }
            };
            let token = match inner.auth.token().await {
                Ok(token) => token,
                Err(error) => break Err(Error::Auth(error)),
            };
            let mut request = match self.request(&prepared, &token) {
                Ok(request) => request,
                Err(error) => break Err(error),
            };
            if let Some(timeout) = timeout {
                request.extensions_mut().insert(RequestTimeout(timeout));
            }
            state.made += 1;
            span.record("attempt", state.made);
            let result = inner.transport.send(request).await;
            drop(permit);
            let step = match result {
                Ok(response) => {
                    span.record("status", response.status().as_u16());
                    self.judge(response, &token, &mut state).await
                }
                Err(error) => self.judge_failure(error, &mut state),
            };
            match step {
                Step::Done(outcome) => break outcome,
                Step::Again(wait) => {
                    if !wait.is_zero() {
                        tokio::time::sleep(wait).await;
                        waited += wait;
                    }
                }
            }
        };
        span.record("wait_ms", millis(waited));
        outcome
    }

    /// What to do after a response: finish, or send again after a wait. A `429`'s wait is the
    /// shared cooldown, which the next admission waits out, so it is not returned here.
    ///
    /// A `DELETE` answered `404` after an earlier attempt that may have been carried out is a
    /// success: the object is gone, most likely deleted by that attempt.
    async fn judge(
        &self,
        response: http::Response<Bytes>,
        token: &AccessToken,
        state: &mut Attempts,
    ) -> Step {
        let inner = &self.inner;
        let status = response.status();
        self.record_changes(response.headers());
        if status.is_success() {
            return Step::Done(Ok(Answer {
                body: response.into_body(),
            }));
        }
        if status == StatusCode::TOO_MANY_REQUESTS {
            let wait =
                retry_after(response.headers()).unwrap_or_else(|| inner.retry.backoff(state.made));
            inner.governor.cool_down(wait);
            if inner.retry.allows_another(state.made) && wait <= inner.retry.max_wait {
                tracing::debug!(
                    attempt = state.made,
                    wait_ms = millis(wait),
                    "rate limited; waiting as Asana asked"
                );
                return Step::Again(Duration::ZERO);
            }
            return Step::Done(Err(Error::RateLimited {
                retry_after: wait,
                attempts: state.made,
            }));
        }
        if status == StatusCode::UNAUTHORIZED
            && !state.refreshed
            && let Some(source) = inner.auth.refreshable()
        {
            source.rejected(token).await;
            state.refreshed = true;
            tracing::debug!(
                attempt = state.made,
                "token rejected; retrying once with a fresh one"
            );
            return Step::Again(Duration::ZERO);
        }
        if status == StatusCode::NOT_FOUND && state.deletes && state.maybe_applied {
            tracing::debug!(
                attempt = state.made,
                "a repeated delete found nothing to delete; an earlier attempt deleted it"
            );
            return Step::Done(Ok(Answer { body: Bytes::new() }));
        }
        if retryable_status(status) && state.repeatable && inner.retry.allows_another(state.made) {
            state.maybe_applied = true;
            let wait = inner.retry.backoff(state.made);
            tracing::debug!(
                attempt = state.made,
                wait_ms = millis(wait),
                "retrying after a server failure"
            );
            return Step::Again(wait);
        }
        Step::Done(Err(Error::from_status(
            status,
            response.body(),
            state.made,
            state.repeatable,
        )))
    }

    /// What to do when no response came back.
    fn judge_failure(&self, error: TransportError, state: &mut Attempts) -> Step {
        let retry = &self.inner.retry;
        let may_retry = error.is_transient()
            && (state.repeatable || !error.may_have_reached_asana())
            && retry.allows_another(state.made);
        if !may_retry {
            return Step::Done(Err(Error::Transport {
                source: error,
                attempts: state.made,
                safe_to_repeat: state.repeatable,
            }));
        }
        state.maybe_applied |= error.may_have_reached_asana();
        let wait = retry.backoff(state.made);
        tracing::debug!(attempt = state.made, wait_ms = millis(wait), kind = %error.kind(), "retrying after a transport failure");
        Step::Again(wait)
    }

    /// Builds the URL and body once, before the first attempt.
    fn prepare<E: Endpoint>(&self, endpoint: &E) -> Result<Prepared, Error> {
        let path = endpoint.path();
        let separator = if path.starts_with('/') { "" } else { "/" };
        let mut url = url::Url::parse(&format!("{}{separator}{path}", self.inner.base_url))
            .map_err(|_| Error::InvalidRequest {
                operation: E::OPERATION_ID,
                reason: "its path does not make a valid URL",
            })?;
        let query = endpoint.query();
        if !query.is_empty() {
            let mut pairs = url.query_pairs_mut();
            for (name, value) in &query {
                pairs.append_pair(name, value);
            }
        }
        let body = endpoint.body().map_err(|source| Error::Encode {
            operation: E::OPERATION_ID,
            source,
        })?;
        let (content_type, body) = match body {
            RequestBody::Json(value) => {
                let bytes = serde_json::to_vec(&value).map_err(|source| Error::Encode {
                    operation: E::OPERATION_ID,
                    source,
                })?;
                (
                    Some(HeaderValue::from_static("application/json")),
                    Bytes::from(bytes),
                )
            }
            RequestBody::Multipart(parts) => {
                let form = multipart::encode(&parts).ok_or(Error::InvalidRequest {
                    operation: E::OPERATION_ID,
                    reason: "its form holds a part of a kind this client cannot write",
                })?;
                let content_type = HeaderValue::try_from(form.content_type).map_err(|_| {
                    Error::InvalidRequest {
                        operation: E::OPERATION_ID,
                        reason: "its multipart boundary is not a valid header",
                    }
                })?;
                (Some(content_type), form.body)
            }
            // `RequestBody` is non-exhaustive; a body this client does not know is sent empty.
            _ => (None, Bytes::new()),
        };
        Ok(Prepared {
            operation: E::OPERATION_ID,
            method: http_method(E::METHOD).ok_or(Error::InvalidRequest {
                operation: E::OPERATION_ID,
                reason: "its method is not a valid HTTP method",
            })?,
            url: url.into(),
            content_type,
            body,
        })
    }

    /// One attempt's request, with this attempt's token.
    fn request(
        &self,
        prepared: &Prepared,
        token: &AccessToken,
    ) -> Result<http::Request<Bytes>, Error> {
        let mut authorization =
            HeaderValue::try_from(format!("Bearer {}", token.secret().expose_secret()))
                .map_err(|_| Error::Auth(AuthError::Malformed))?;
        authorization.set_sensitive(true);
        let mut request = http::Request::builder()
            .method(prepared.method.clone())
            .uri(prepared.url.as_str())
            .body(prepared.body.clone())
            .map_err(|_| Error::InvalidRequest {
                operation: prepared.operation,
                reason: "its URL is not a valid request target",
            })?;
        let headers: &mut HeaderMap = request.headers_mut();
        headers.insert(AUTHORIZATION, authorization);
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        headers.insert(USER_AGENT, self.inner.user_agent.clone());
        if let Some(content_type) = &prepared.content_type {
            headers.insert(CONTENT_TYPE, content_type.clone());
        }
        if let Some(enable) = &self.inner.enable {
            headers.insert("asana-enable", enable.clone());
        }
        if let Some(disable) = &self.inner.disable {
            headers.insert("asana-disable", disable.clone());
        }
        Ok(request)
    }
}

/// Turns one answer to an operation into its result or its error, as [`Client::send`] does.
///
/// For a response that arrived some other way than [`Client::send`]: each action's answer
/// inside a batch response, or a recorded body in a test. A success status decodes the body by
/// the operation's [`Envelope`]; any other status becomes the matching [`Error`], counted as one
/// attempt.
///
/// # Errors
///
/// The [`Error`] for a non-success status, or [`Error::Decode`] when the body is not the
/// operation's shape.
pub fn decode_response<E: Endpoint>(status: StatusCode, body: &[u8]) -> Result<E::Output, Error> {
    if status.is_success() {
        decode_output::<E>(body)
    } else {
        Err(Error::from_status(
            status,
            body,
            1,
            safe_to_repeat(E::METHOD, E::IDEMPOTENT),
        ))
    }
}

/// Decodes a successful body by the operation's envelope. An empty body, `{}`, `null` or
/// `{"data": {}}` becomes the operation's empty output.
fn decode_output<E: Endpoint>(body: &[u8]) -> Result<E::Output, Error> {
    let decoded = match E::ENVELOPE {
        Envelope::Data => {
            decode_json::<DataEnvelope<E::Output>>(E::OPERATION_ID, body).map(|data| data.data)
        }
        // `Whole`, and any envelope this client does not know: the body as it is.
        _ => decode_json::<E::Output>(E::OPERATION_ID, body),
    };
    match decoded {
        Ok(output) => Ok(output),
        Err(error) => empty_output(body, E::ENVELOPE).ok_or(error),
    }
}

/// The output of an operation that answers with nothing, when the body says nothing.
fn empty_output<T: DeserializeOwned>(body: &[u8], envelope: Envelope) -> Option<T> {
    let inner = if body.iter().all(u8::is_ascii_whitespace) {
        serde_json::Value::Null
    } else {
        let value: serde_json::Value = serde_json::from_slice(body).ok()?;
        match envelope {
            Envelope::Data if value.as_object().is_some_and(serde_json::Map::is_empty) => value,
            Envelope::Data => value.get("data")?.clone(),
            _ => value,
        }
    };
    let empty = inner.is_null() || inner.as_object().is_some_and(serde_json::Map::is_empty);
    if !empty {
        return None;
    }
    T::deserialize(serde_json::Value::Object(serde_json::Map::new()))
        .ok()
        .or_else(|| T::deserialize(serde_json::Value::Null).ok())
}

/// Decodes JSON, reporting where it failed and never the value there.
fn decode_json<T: DeserializeOwned>(operation: &'static str, body: &[u8]) -> Result<T, Error> {
    let mut deserializer = serde_json::Deserializer::from_slice(body);
    serde_path_to_error::deserialize(&mut deserializer).map_err(|error| Error::Decode {
        operation,
        path: error.path().to_string(),
        source: DecodeError::from_json(error.inner()),
    })
}

#[derive(Deserialize)]
struct DataEnvelope<T> {
    data: T,
}

#[derive(Deserialize)]
struct PageEnvelope<T> {
    data: Vec<T>,
    #[serde(default)]
    next_page: Option<NextPage>,
}

/// The `http` crate's method for `method`, or `None` for a name it cannot carry.
fn http_method(method: Method) -> Option<http::Method> {
    Some(match method {
        Method::Get => http::Method::GET,
        Method::Post => http::Method::POST,
        Method::Put => http::Method::PUT,
        Method::Delete => http::Method::DELETE,
        other => return http::Method::from_bytes(other.as_str().as_bytes()).ok(),
    })
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}
