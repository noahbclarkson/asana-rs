//! A whole receiver, framework-neutral: the consumer's collaborators and every status rule.
//!
//! [`classify`](crate::classify), [`verify`](crate::verify) and the [`HandshakeStore`] are the
//! protocol's parts; [`Receiver`] puts them together the way every receiver must, so a framework
//! adapter (`asana-webhooks-axum`, `asana-webhooks-actix`) only reads the path segment, the headers
//! and the raw body, calls [`Receiver::receive`], and writes the [`Reply`] back. Besides the
//! handshake store it needs a [`SecretLookup`], which finds a route's stored secret, and a
//! [`Sink`], which takes each verified batch off its hands.
//!
//! | The call | The answer |
//! |---|---|
//! | the path segment is not a [`RouteKey`] | `404` |
//! | a body over [`Limits::max_body_bytes`] | `413` |
//! | both Asana headers, or an empty secret | `400` |
//! | a handshake the store captured | `200`, `X-Hook-Secret` echoed |
//! | a handshake the store refused | its [`StoreError::status_hint`]: `404`, `409` or `503` |
//! | neither header, or a malformed or wrong signature | `401` |
//! | a delivery on a route the lookup does not know | `404`, which Asana retries |
//! | a delivery on a route the consumer [retired](RouteSecret::Retired) | `410`: Asana deletes the webhook |
//! | the lookup fails | `503`, which Asana retries |
//! | a verified body that is not an event batch | `400` (`413` for more than [`Limits::max_events`]) |
//! | a heartbeat | `200`, without reaching the sink |
//! | a batch the sink took | `200` |
//! | a batch the sink is too [`Busy`] for | `503`, which Asana retries |
//!
//! **Why `404` and not `410` for an unknown route.** `410 Gone` makes Asana delete the webhook at
//! once, for good. A route can look unknown for reasons that pass (a secrets table restored from
//! an older backup, a deploy reading the wrong database), and `404` makes Asana retry with back-off
//! for up to 24 hours, so a fix in that time loses nothing. Answer `410` only for a route the
//! consumer decided it no longer wants.
//!
//! ```
//! use asana_webhooks::{Delivery, MemoryHandshakeStore, MemorySecrets, Receiver, RouteKey};
//! use tokio::sync::mpsc;
//! # fn block_on<F: std::future::Future>(future: F) -> F::Output {
//! #     let mut future = std::pin::pin!(future);
//! #     let mut context = std::task::Context::from_waker(std::task::Waker::noop());
//! #     loop {
//! #         if let std::task::Poll::Ready(output) = future.as_mut().poll(&mut context) {
//! #             return output;
//! #         }
//! #     }
//! # }
//!
//! let (queue, _deliveries) = mpsc::channel::<Delivery>(1024);
//! let receiver = Receiver::new(MemoryHandshakeStore::new(), MemorySecrets::new(), queue);
//!
//! // A delivery on a route nobody established: 404, so Asana retries rather than deleting.
//! let route = RouteKey::generate().to_string();
//! let headers = [("X-Hook-Signature", "00".repeat(32))];
//! let reply = block_on(receiver.receive(&route, &headers, b"{\"events\":[]}"));
//! assert_eq!(reply.status(), 404);
//! assert!(reply.echo().is_none());
//! ```

use std::collections::HashMap;
use std::fmt;
use std::future::{Future, ready};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tokio::sync::mpsc;
use tracing::Instrument as _;
use tracing::field::Empty;

use crate::events::{DecodeError, EventBatch, Limits};
use crate::handshake::{HandshakeStore, RouteDigest, RouteKey, StoreError};
use crate::headers::HeaderLookup;
use crate::inbound::{Inbound, InboundError, classify_with};
use crate::signing::{HandshakeReply, WebhookSecret, verify};

/// Finds the secret that signs a route's deliveries.
///
/// Implement it over wherever the secrets of established webhooks are kept (a database table
/// keyed by [`RouteKey::digest`], with the secrets encrypted at rest); [`MemorySecrets`] serves one
/// process. It is asked once per delivery, so keep it fast.
pub trait SecretLookup: Send + Sync {
    /// What is known about `route`.
    ///
    /// An error answers `503 Service Unavailable`, so Asana retries the delivery later.
    fn lookup(
        &self,
        route: &RouteKey,
    ) -> impl Future<Output = Result<RouteSecret, StoreError>> + Send;
}

/// What a [`SecretLookup`] knows about a route.
#[non_exhaustive]
pub enum RouteSecret {
    /// A live webhook: its deliveries are verified with this secret.
    Active(WebhookSecret),
    /// A webhook the consumer has given up on. Its deliveries are answered `410 Gone`, which makes
    /// Asana delete the webhook.
    Retired,
    /// Nothing is known about the route. Its deliveries are answered `404 Not Found`, which Asana
    /// treats as a failure to retry rather than as a request to delete the webhook: a store that
    /// lost a row, or a deploy that briefly points at the wrong database, must never make Asana
    /// delete a live webhook. Answer [`Retired`](Self::Retired) only for a route you know you no
    /// longer want.
    Unknown,
}

impl fmt::Debug for RouteSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Active(_) => "Active([REDACTED])",
            Self::Retired => "Retired",
            Self::Unknown => "Unknown",
        })
    }
}

/// Established webhooks' secrets, by route, in this process's memory.
///
/// Right only when the process that establishes webhooks is the one that receives them, and lost
/// on restart; across instances or restarts, keep the secrets (encrypted) in a database table
/// keyed by [`RouteKey::digest`] and implement [`SecretLookup`] over it. Routes are kept as their
/// digest, never as the key itself.
#[derive(Default)]
pub struct MemorySecrets {
    routes: Mutex<HashMap<RouteDigest, Option<WebhookSecret>>>,
}

impl MemorySecrets {
    /// An empty table: every route is [`RouteSecret::Unknown`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Makes `route` live, its deliveries verified with `secret`.
    pub fn insert(&self, route: &RouteKey, secret: WebhookSecret) {
        self.routes().insert(route.digest(), Some(secret));
    }

    /// Marks `route` [retired](RouteSecret::Retired): its deliveries are answered `410 Gone`, and
    /// Asana deletes the webhook.
    pub fn retire(&self, route: &RouteKey) {
        self.routes().insert(route.digest(), None);
    }

    /// Forgets `route`: its deliveries are answered `404`, which Asana retries.
    pub fn remove(&self, route: &RouteKey) {
        self.routes().remove(&route.digest());
    }

    fn routes(&self) -> MutexGuard<'_, HashMap<RouteDigest, Option<WebhookSecret>>> {
        self.routes.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl fmt::Debug for MemorySecrets {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MemorySecrets")
            .field("routes", &self.routes().len())
            .finish_non_exhaustive()
    }
}

impl SecretLookup for MemorySecrets {
    fn lookup(
        &self,
        route: &RouteKey,
    ) -> impl Future<Output = Result<RouteSecret, StoreError>> + Send {
        let found = match self.routes().get(&route.digest()) {
            Some(Some(secret)) => RouteSecret::Active(secret.clone()),
            Some(None) => RouteSecret::Retired,
            None => RouteSecret::Unknown,
        };
        ready(Ok(found))
    }
}

/// A verified batch of events and the route it arrived on: what a [`Sink`] is handed.
///
/// Heartbeats (deliveries with no events) are answered without reaching the sink.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Delivery {
    /// The route the delivery arrived on, which says which webhook it belongs to.
    pub route: RouteKey,
    /// The decoded events, including any that could not be read
    /// ([`EventBatch::unreadable`]).
    pub batch: EventBatch,
}

/// Where verified deliveries go.
///
/// **Enqueue; do not work.** Asana wants an answer within 10 seconds and retries a delivery that
/// does not get one, so `accept` should record or queue the delivery and return at once; a worker
/// elsewhere does the processing. A `tokio::sync::mpsc::Sender<Delivery>` is a sink already: it
/// queues with `try_send` and reports a full or closed channel as [`Busy`].
pub trait Sink: Send + Sync {
    /// Takes `delivery`, or says it cannot right now.
    ///
    /// [`Busy`] answers `503 Service Unavailable`, so Asana retries the delivery later.
    fn accept(&self, delivery: Delivery) -> impl Future<Output = Result<(), Busy>> + Send;
}

impl Sink for mpsc::Sender<Delivery> {
    fn accept(&self, delivery: Delivery) -> impl Future<Output = Result<(), Busy>> + Send {
        // `try_send`, never `send`: waiting for room could outlast Asana's 10 seconds.
        ready(self.try_send(delivery).map_err(|_| Busy))
    }
}

/// A [`Sink`] cannot take a delivery now (its queue is full, or its worker has stopped).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, thiserror::Error)]
#[error("the webhook sink cannot take the delivery now")]
pub struct Busy;

/// What to answer a call: a status with an empty body and, for a captured handshake, the secret
/// to echo.
#[derive(Clone, Debug)]
pub struct Reply {
    status: u16,
    echo: Option<HandshakeReply>,
}

impl Reply {
    fn status_only(status: u16) -> Self {
        Self { status, echo: None }
    }

    /// The HTTP status to answer.
    #[must_use]
    pub fn status(&self) -> u16 {
        self.status
    }

    /// For a captured handshake, the header to set: [`HandshakeReply::header_name`] with
    /// [`HandshakeReply::header_value`]. Mark the value sensitive if the framework allows it.
    #[must_use]
    pub fn echo(&self) -> Option<&HandshakeReply> {
        self.echo.as_ref()
    }
}

/// The webhook receiver: the consumer's stores and sink, and the size limits.
///
/// Cheap to clone (one [`Arc`] and two numbers), so it serves as a framework's shared state
/// directly; every clone shares the same stores and sink.
pub struct Receiver<H, L, S> {
    shared: Arc<Shared<H, L, S>>,
    limits: Limits,
}

struct Shared<H, L, S> {
    handshakes: H,
    secrets: L,
    sink: S,
}

impl<H, L, S> Clone for Receiver<H, L, S> {
    fn clone(&self) -> Self {
        Self {
            shared: Arc::clone(&self.shared),
            limits: self.limits.clone(),
        }
    }
}

impl<H, L, S> fmt::Debug for Receiver<H, L, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Receiver")
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

impl<H, L, S> Receiver<H, L, S>
where
    H: HandshakeStore,
    L: SecretLookup,
    S: Sink,
{
    /// A receiver capturing handshakes into `handshakes`, verifying deliveries with the secrets
    /// `secrets` finds, and handing verified batches to `sink`, with the default [`Limits`]
    /// (256 KiB bodies, 10,000 events).
    #[must_use]
    pub fn new(handshakes: H, secrets: L, sink: S) -> Self {
        Self {
            shared: Arc::new(Shared {
                handshakes,
                secrets,
                sink,
            }),
            limits: Limits::default(),
        }
    }

    /// The same receiver with other limits: a body longer than `limits.max_body_bytes` is answered
    /// `413`, and so is a delivery of more than `limits.max_events`. An adapter should also stop
    /// reading a body at that size rather than buffer more only for it to be refused here.
    #[must_use]
    pub fn limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    /// The handshake store, for the code that creates webhooks: hand it to
    /// `establish(..).store(receiver.handshakes())` so the creator and this receiver meet in it.
    #[must_use]
    pub fn handshakes(&self) -> &H {
        &self.shared.handshakes
    }

    /// The secret lookup, for the code that records a newly established webhook's secret.
    #[must_use]
    pub fn secrets(&self) -> &L {
        &self.shared.secrets
    }

    /// The limits in force.
    #[must_use]
    pub fn current_limits(&self) -> &Limits {
        &self.limits
    }

    /// Answers one call: `route_segment` is the path segment holding the route key, `headers` the
    /// request's headers and `body` its raw bytes, exactly as received. The module's table lists
    /// every answer.
    ///
    /// Each call runs in an `asana_webhook` span carrying the route (as its `Debug`, the start of
    /// its digest, never the key itself), the kind of call, the number of events and the status.
    /// Neither the secret, the signature nor the body is ever logged.
    pub async fn receive(
        &self,
        route_segment: &str,
        headers: &(impl HeaderLookup + Sync + ?Sized),
        body: &[u8],
    ) -> Reply {
        let span = tracing::info_span!(
            "asana_webhook",
            route = Empty,
            kind = Empty,
            events = Empty,
            status = Empty,
        );
        async move {
            let reply = self.answer(route_segment, headers, body).await;
            let status = reply.status;
            tracing::Span::current().record("status", status);
            if status >= 500 || status == 401 {
                tracing::warn!(status, "refused an Asana webhook call");
            } else {
                tracing::debug!(status, "answered an Asana webhook call");
            }
            reply
        }
        .instrument(span)
        .await
    }

    async fn answer(
        &self,
        route_segment: &str,
        headers: &(impl HeaderLookup + Sync + ?Sized),
        body: &[u8],
    ) -> Reply {
        let Ok(route) = RouteKey::parse(route_segment) else {
            return Reply::status_only(404);
        };
        let span = tracing::Span::current();
        span.record("route", tracing::field::debug(&route));
        let inbound = match classify_with(headers, body, &self.limits) {
            Ok(inbound) => inbound,
            Err(refused) => {
                if let InboundError::BodyTooLarge { len, limit } = refused {
                    tracing::error!(
                        len,
                        limit,
                        "a delivery over the body limit was refused; if it is Asana's, it will be \
                         refused on every retry and the webhook deleted: raise max_body_bytes"
                    );
                }
                return Reply::status_only(inbound_status(&refused));
            }
        };
        match inbound {
            Inbound::Handshake { secret } => {
                span.record("kind", "handshake");
                match self.shared.handshakes.capture(&route, secret).await {
                    Ok(captured) => Reply {
                        status: 200,
                        echo: Some(captured.into_reply()),
                    },
                    Err(refused) => {
                        if matches!(refused, StoreError::Backend(_)) {
                            tracing::warn!(error = %refused, "the handshake store failed");
                        }
                        Reply::status_only(refused.status_hint())
                    }
                }
            }
            Inbound::Delivery { signature, body } => {
                span.record("kind", "delivery");
                let secret = match self.shared.secrets.lookup(&route).await {
                    Ok(RouteSecret::Active(secret)) => secret,
                    Ok(RouteSecret::Retired) => return Reply::status_only(410),
                    Ok(RouteSecret::Unknown) => return Reply::status_only(404),
                    Err(error) => {
                        tracing::warn!(%error, "the secret lookup failed");
                        return Reply::status_only(503);
                    }
                };
                // The signature covers the exact bytes: check it before anything reads them.
                let verified = match verify(&secret, &signature, body) {
                    Ok(verified) => verified,
                    Err(refused) => return Reply::status_only(refused.status_hint()),
                };
                if body.len() > self.limits.max_body_bytes / 2 {
                    tracing::warn!(
                        len = body.len(),
                        limit = self.limits.max_body_bytes,
                        "a verified delivery is over half the body limit; a larger one would be \
                         refused for good"
                    );
                }
                let batch = match verified.decode_with(&self.limits) {
                    Ok(batch) => batch,
                    Err(refused) => {
                        if let DecodeError::TooManyEvents { count, limit } = refused {
                            tracing::error!(
                                count,
                                limit,
                                "a verified delivery over the event limit was refused; Asana will \
                                 retry it and then delete the webhook: raise max_events"
                            );
                        }
                        return Reply::status_only(refused.status_hint());
                    }
                };
                span.record("events", batch.events.len() + batch.unreadable.len());
                if !batch.unreadable.is_empty() {
                    tracing::warn!(
                        unreadable = batch.unreadable.len(),
                        "some events in the delivery could not be read"
                    );
                }
                if batch.is_heartbeat() {
                    return Reply::status_only(200);
                }
                match self.shared.sink.accept(Delivery { route, batch }).await {
                    Ok(()) => Reply::status_only(200),
                    Err(Busy) => Reply::status_only(503),
                }
            }
        }
    }
}

/// The status for a request that is neither a handshake nor a delivery.
///
/// A missing or malformed signature is `401`, like a wrong one: whoever sent it is not proven to
/// be Asana. Everything else follows [`InboundError::status_hint`] (`413` oversized, `400`
/// otherwise).
fn inbound_status(refused: &InboundError) -> u16 {
    match refused {
        InboundError::NeitherHeader | InboundError::MalformedSignature => 401,
        other => other.status_hint(),
    }
}

#[cfg(test)]
mod tests;
