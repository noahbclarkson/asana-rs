//! The fake as an [`asana_client::Transport`]: each request is parsed, recorded, checked against
//! queued faults and the token, routed to its handler, and answered in Asana's shapes.

use std::fmt;
use std::time::Duration;

use asana_client::{Transport, TransportError};
use bytes::Bytes;
use futures_util::future::BoxFuture;
use http::StatusCode;
use http::header::{AUTHORIZATION, CONTENT_TYPE};
use serde_json::Value;

use crate::FakeAsana;
use crate::answer::{Answer, Call, Refusal};
use crate::faults::{FakeTimeout, Fault};
use crate::multipart::{FormPart, boundary, parse};
use crate::ops::attachments::download;
use crate::recorded::RecordedRequest;
use crate::render::FILES_HOST;
use crate::router::{Action, Matched, find};
use crate::select::Selection;
use crate::state::{Id, State};

/// The [`Transport`] a client uses to reach a [`FakeAsana`]; from [`FakeAsana::transport`].
#[derive(Clone)]
pub struct FakeTransport {
    fake: FakeAsana,
}

impl fmt::Debug for FakeTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FakeTransport").finish_non_exhaustive()
    }
}

impl FakeTransport {
    pub(crate) fn new(fake: FakeAsana) -> Self {
        Self { fake }
    }
}

impl Transport for FakeTransport {
    fn send(
        &self,
        request: http::Request<Bytes>,
    ) -> BoxFuture<'_, Result<http::Response<Bytes>, TransportError>> {
        Box::pin(self.fake.handle(request))
    }
}

/// The answer to an operation in the specification that the fake does not serve.
pub(crate) fn unserved(operation: &str) -> Refusal {
    Refusal::new(
        StatusCode::NOT_IMPLEMENTED,
        format!("asana-fake does not serve {operation}"),
    )
}

/// Asana's answer to a path no operation has: `404`.
pub(crate) fn no_route() -> Refusal {
    Refusal::new(StatusCode::NOT_FOUND, "No matching route for request")
}

/// A request body, read according to its `Content-Type`.
enum Body {
    None,
    Json(Value),
    Form(Vec<FormPart>),
    Malformed(Refusal),
}

fn read_body(headers: &http::HeaderMap, body: &Bytes) -> Body {
    if body.is_empty() {
        return Body::None;
    }
    let content_type = headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if let Some(boundary) = boundary(content_type) {
        return parse(body, &boundary).map_or_else(
            || Body::Malformed(Refusal::bad_request("Could not parse the multipart form")),
            Body::Form,
        );
    }
    match serde_json::from_slice(body) {
        Ok(value) => Body::Json(value),
        Err(_) => Body::Malformed(Refusal::bad_request(
            "Could not parse request data, invalid JSON",
        )),
    }
}

impl FakeAsana {
    /// Answers one request.
    pub(crate) async fn handle(
        &self,
        request: http::Request<Bytes>,
    ) -> Result<http::Response<Bytes>, TransportError> {
        let (parts, body) = request.into_parts();
        let Ok(url) = url::Url::parse(&parts.uri.to_string()) else {
            return Ok(respond(
                Refusal::bad_request("The request URL is not absolute").into_answer(),
            ));
        };
        if url.host_str() == Some(FILES_HOST) {
            return Ok(download(&self.lock(), url.path()));
        }
        let path = url
            .path()
            .strip_prefix("/api/1.0")
            .unwrap_or(url.path())
            .to_owned();
        let query: Vec<(String, String)> = url.query_pairs().into_owned().collect();
        let body = read_body(&parts.headers, &body);
        let matched = find(parts.method.as_str(), &path);
        let operation = matched.operation();

        let fault = {
            let mut state = self.lock();
            state.requests.push(RecordedRequest {
                method: parts.method.clone(),
                path: path.clone(),
                query: query.clone(),
                body: match &body {
                    Body::Json(value) => Some(value.clone()),
                    _ => None,
                },
                operation,
            });
            take_fault(&mut state, operation)
        };
        let mut answer_lost = false;
        match fault {
            Some(Fault::TimeoutAfterApplying) => answer_lost = true,
            Some(Fault::RateLimited { retry_after }) => {
                return Ok(respond(rate_limited(retry_after)));
            }
            Some(Fault::Status { status, message }) => {
                return Ok(respond(Refusal::new(status, message).into_answer()));
            }
            Some(Fault::Timeout) => return Err(TransportError::timeout(FakeTimeout)),
            Some(Fault::Slow(delay)) => tokio::time::sleep(delay).await,
            None => {}
        }

        let user = match self.authenticate(&parts.headers) {
            Ok(user) => user,
            Err(refusal) => return Ok(respond(refusal.into_answer())),
        };
        let (json, form) = match body {
            Body::None => (None, Vec::new()),
            Body::Json(value) => (Some(value), Vec::new()),
            Body::Form(form) => (None, form),
            Body::Malformed(refusal) => return Ok(respond(refusal.into_answer())),
        };
        let answer = match matched {
            Matched::Served(route, params) => {
                if let Err(refusal) = route.check_fields(&query) {
                    return Ok(respond(refusal.into_answer()));
                }
                let call = Call {
                    params,
                    selection: Selection::parse(
                        query
                            .iter()
                            .find(|(key, _)| key == "opt_fields")
                            .map(|(_, value)| value.as_str()),
                    ),
                    query,
                    body: json,
                    form,
                    path,
                };
                match route.action {
                    Action::Sync(handler) => {
                        let mut state = self.lock();
                        state.actor = Some(user);
                        let reply = handler(&mut state, &call);
                        state.actor = None;
                        reply.unwrap_or_else(Refusal::into_answer)
                    }
                    Action::CreateWebhook => self.create_webhook_call(&call).await,
                }
            }
            Matched::Unserved(operation) => unserved(operation).into_answer(),
            Matched::Nothing => no_route().into_answer(),
        };
        let auto_deliver = self.lock().auto_deliver;
        if auto_deliver {
            self.deliver().await;
        }
        if answer_lost {
            return Err(TransportError::timeout(FakeTimeout));
        }
        Ok(respond(answer))
    }

    /// The user the request's token belongs to, or `401`.
    fn authenticate(&self, headers: &http::HeaderMap) -> Result<Id, Refusal> {
        let token = headers
            .get(AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .filter(|token| !token.is_empty());
        let not_authorized = || Refusal::new(StatusCode::UNAUTHORIZED, "Not Authorized");
        let token = token.ok_or_else(not_authorized)?;
        let mut state = self.lock();
        if state.tokens.is_empty() {
            return Ok(state.me());
        }
        state.tokens.get(token).cloned().ok_or_else(not_authorized)
    }
}

/// Takes the first queued fault for `operation` (or for any request).
fn take_fault(state: &mut State, operation: Option<&'static str>) -> Option<Fault> {
    let index = state.faults.iter().position(|queued| {
        queued
            .operation
            .is_none_or(|wanted| Some(wanted) == operation)
    })?;
    state.faults.remove(index).map(|queued| queued.fault)
}

/// `429` with `Retry-After` in whole seconds, rounded up.
fn rate_limited(retry_after: Duration) -> Answer {
    let seconds = retry_after.as_secs() + u64::from(retry_after.subsec_nanos() > 0);
    let mut refusal = Refusal::new(
        StatusCode::TOO_MANY_REQUESTS,
        "You have made too many requests recently. Please, be chill.",
    );
    refusal.headers.push(("retry-after", seconds.to_string()));
    refusal.into_answer()
}

/// Writes an answer as an HTTP response.
fn respond(answer: Answer) -> http::Response<Bytes> {
    let body = serde_json::to_vec(&answer.body).unwrap_or_default();
    let mut response = http::Response::new(Bytes::from(body));
    *response.status_mut() = answer.status;
    response.headers_mut().insert(
        CONTENT_TYPE,
        http::HeaderValue::from_static("application/json; charset=UTF-8"),
    );
    for (name, value) in answer.headers {
        if let (Ok(name), Ok(value)) = (
            http::HeaderName::from_bytes(name.as_bytes()),
            http::HeaderValue::from_str(&value),
        ) {
            response.headers_mut().insert(name, value);
        }
    }
    response
}
