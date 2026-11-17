//! The webhook side of the fake: the handshake when a webhook is created, signed deliveries of
//! queued events, and heartbeats, all sent to the receiver a test registers.

use std::sync::Arc;

use asana_openapi::ids::WebhookGid;
use asana_webhooks::{HOOK_SECRET, HOOK_SIGNATURE, WebhookSecret, sign};
use bytes::Bytes;
use futures_util::future::BoxFuture;
use http::StatusCode;
use serde_json::{Value, json};

use crate::FakeAsana;
use crate::answer::{Answer, Call, Refusal};
use crate::ops::webhooks::{finish, prepare, read_create};
use crate::state::Id;

/// The receiver deliveries go to: an async function from request to response.
pub(crate) type Receiver =
    Arc<dyn Fn(http::Request<Bytes>) -> BoxFuture<'static, http::Response<Bytes>> + Send + Sync>;

/// What one delivery the fake made to the receiver held, and how the receiver answered.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Delivered {
    /// The webhook delivered for.
    pub webhook: WebhookGid,
    /// How many events the batch held (none for a heartbeat).
    pub events: usize,
    /// What the receiver answered.
    pub status: StatusCode,
}

/// Why the fake refused something asked of it directly (not through the API).
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum FakeError {
    /// Asana would have refused the request with this status and message.
    #[error("Asana would refuse this with {status}: {message}")]
    Refused {
        /// The status Asana would answer.
        status: StatusCode,
        /// Asana's error message.
        message: String,
    },
}

impl From<Refusal> for FakeError {
    fn from(refusal: Refusal) -> Self {
        Self::Refused {
            status: refusal.status,
            message: refusal.message,
        }
    }
}

/// The message Asana answers when the receiver did not echo the secret.
const HANDSHAKE_FAILED: &str = "The remote server did not respond with the handshake secret.";

impl FakeAsana {
    /// Creates a webhook as `POST /webhooks` does, performing the handshake against the
    /// receiver: it must answer `200` or `204` echoing `X-Hook-Secret`. Returns the webhook and its
    /// secret.
    pub(crate) async fn establish(
        &self,
        resource: &str,
        target: &str,
        filters: Vec<Value>,
    ) -> Result<(Id, String), Refusal> {
        let (pending, receiver) = {
            let mut state = self.lock();
            let pending = prepare(&mut state, resource, target, filters)?;
            (pending, state.receiver.clone())
        };
        let receiver = receiver.ok_or_else(|| Refusal::bad_request(HANDSHAKE_FAILED))?;
        let request = http::Request::post(pending.target.as_str())
            .header(HOOK_SECRET, pending.secret.as_str())
            .body(Bytes::new())
            .map_err(|_| Refusal::bad_request("target: Not a valid URL"))?;
        let response = receiver(request).await;
        let echoed = response
            .headers()
            .get(HOOK_SECRET)
            .and_then(|value| value.to_str().ok())
            == Some(pending.secret.as_str());
        let accepted = matches!(response.status(), StatusCode::OK | StatusCode::NO_CONTENT);
        if !(echoed && accepted) {
            return Err(Refusal::bad_request(HANDSHAKE_FAILED));
        }
        let secret = pending.secret.clone();
        let gid = finish(&mut self.lock(), pending);
        Ok((gid, secret))
    }

    /// `createWebhook` through the API: `201` with the webhook and `X-Hook-Secret` beside it.
    pub(crate) async fn create_webhook_call(&self, call: &Call) -> Answer {
        let (resource, target, filters) = match read_create(call) {
            Ok(request) => request,
            Err(refusal) => return refusal.into_answer(),
        };
        match self.establish(&resource, &target, filters).await {
            Ok((gid, secret)) => {
                let data = call.selection.one(&self.lock(), &gid);
                Answer::with_status(
                    StatusCode::CREATED,
                    json!({ "data": data, "X-Hook-Secret": secret }),
                )
            }
            Err(refusal) => refusal.into_answer(),
        }
    }

    /// Sends every webhook's queued events to the receiver, one signed batch per webhook, and
    /// returns what it sent.
    ///
    /// A `2xx` answer records a success. `410 Gone` deletes the webhook, as Asana does. Any other
    /// answer records a failure and keeps the events queued for the next call, as Asana retries.
    /// With no receiver registered nothing is sent and the events stay queued.
    pub async fn deliver(&self) -> Vec<Delivered> {
        let (receiver, batches) = {
            let mut state = self.lock();
            let Some(receiver) = state.receiver.clone() else {
                return Vec::new();
            };
            let batches: Vec<(Id, String, String, Vec<Value>)> = state
                .webhooks
                .iter_mut()
                .filter(|(_, webhook)| !webhook.pending.is_empty())
                .map(|(gid, webhook)| {
                    (
                        gid.clone(),
                        webhook.target.clone(),
                        webhook.secret.clone(),
                        webhook.pending.drain(..).collect(),
                    )
                })
                .collect();
            (receiver, batches)
        };
        let mut deliveries = Vec::with_capacity(batches.len());
        for (gid, target, secret, events) in batches {
            let count = events.len();
            let status = post(&receiver, &target, &secret, &events).await;
            self.settle(&gid, status, events);
            deliveries.push(Delivered {
                webhook: crate::typed(&gid),
                events: count,
                status,
            });
        }
        deliveries
    }

    /// Sends a heartbeat (a signed batch with no events) for `webhook`. `None` when the webhook
    /// does not exist or no receiver is registered.
    pub async fn heartbeat(&self, webhook: &WebhookGid) -> Option<Delivered> {
        let gid = webhook.to_string();
        let (receiver, target, secret) = {
            let state = self.lock();
            let record = state.webhooks.get(&gid)?;
            (
                state.receiver.clone()?,
                record.target.clone(),
                record.secret.clone(),
            )
        };
        let status = post(&receiver, &target, &secret, &[]).await;
        self.settle(&gid, status, Vec::new());
        Some(Delivered {
            webhook: webhook.clone(),
            events: 0,
            status,
        })
    }

    /// Records how a delivery went.
    fn settle(&self, gid: &Id, status: StatusCode, events: Vec<Value>) {
        let mut state = self.lock();
        let now = state.now();
        if status == StatusCode::GONE {
            state.webhooks.remove(gid);
            return;
        }
        let Some(webhook) = state.webhooks.get_mut(gid) else {
            return;
        };
        if status.is_success() {
            webhook.last_success_at = Some(now);
            webhook.delivery_retry_count = 0;
        } else {
            webhook.last_failure_at = Some(now);
            webhook.last_failure_content =
                Some(format!("Received status code {}", status.as_u16()));
            webhook.delivery_retry_count += 1;
            for event in events.into_iter().rev() {
                webhook.pending.push_front(event);
            }
        }
    }
}

/// Posts one signed batch to the receiver and returns its status.
async fn post(receiver: &Receiver, target: &str, secret: &str, events: &[Value]) -> StatusCode {
    let body = serde_json::to_vec(&json!({ "events": events })).unwrap_or_default();
    let signature = sign(
        &WebhookSecret::from_bytes(secret.as_bytes().to_vec()),
        &body,
    )
    .to_hex();
    let request = http::Request::post(target)
        .header(HOOK_SIGNATURE, signature)
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(Bytes::from(body));
    match request {
        Ok(request) => receiver(request).await.status(),
        Err(_) => StatusCode::BAD_GATEWAY,
    }
}
