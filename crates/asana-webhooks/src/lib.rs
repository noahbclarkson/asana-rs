#![forbid(unsafe_code)]

//! Receiving Asana webhooks, with no HTTP client and no web framework.
//!
//! Asana calls a webhook's target twice over: once with a handshake that must be echoed while the
//! creating request is still open, and from then on with signed batches of compact events. This
//! crate is that protocol as plain functions over headers and bytes: tell the two kinds of call
//! apart, answer the handshake, verify a delivery's signature over its raw bytes, and decode the
//! events into types.
//!
//! # Receiving, end to end
//!
//! 1. **Classify** the request with [`classify`], from its headers (through [`HeaderLookup`], which
//!    a slice of pairs implements) and its raw body. A handshake carries `X-Hook-Secret`; a
//!    delivery carries `X-Hook-Signature`; anything else is refused with an [`InboundError`].
//! 2. **A handshake**: store the secret where the webhook's creator will find it
//!    ([`HandshakeStore::capture`], keyed by the [`RouteKey`] in the path), then answer with the
//!    [`HandshakeReply`] it returns: `200` with the secret echoed in `X-Hook-Secret`. The creator's
//!    `POST /webhooks` is waiting on this answer.
//! 3. **A delivery**: [`verify`] the signature with the route's stored secret **before parsing
//!    anything**. The signature covers the body's exact bytes, so it must be checked on the bytes as
//!    received; and verifying first means a forged request never reaches the JSON parser. Only a
//!    [`VerifiedBody`] can be [decoded](VerifiedBody::decode).
//! 4. **Heartbeats**: a delivery with no events ([`EventBatch::is_heartbeat`]) arrives right after
//!    the handshake and every eight hours. Answer it `200` like any other: after 24 hours without a
//!    successful delivery Asana deletes the webhook.
//! 5. **Answer within 10 seconds.** Asana retries a delivery that is not answered `2xx` in time,
//!    with back-off, for up to 24 hours. So record or queue the events and answer; do the work
//!    afterwards. Answering `410 Gone` asks Asana to delete the webhook.
//! 6. **Re-read before acting.** Events are compact and can arrive minutes late; use
//!    [`Event::age`] to drop stale ones, [`Event::key`] with a [`Dedupe`] to skip redeliveries, and
//!    re-read the resource from the API before acting on it.
//!
//! ```
//! use asana_webhooks::{
//!     EventBatch, HandshakeStore, Inbound, MemoryHandshakeStore, RouteKey, WebhookSecret,
//!     classify, sign, verify,
//! };
//! use std::time::Duration;
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
//! /// What the receiver answers: a status, and a header to set.
//! struct Answer {
//!     status: u16,
//!     header: Option<(&'static str, Vec<u8>)>,
//! }
//!
//! impl Answer {
//!     fn status(status: u16) -> Self {
//!         Self { status, header: None }
//!     }
//! }
//!
//! /// The receiver, for a request to `/webhooks/asana/{route}`.
//! fn receive(
//!     store: &MemoryHandshakeStore,
//!     secrets: &dyn Fn(&RouteKey) -> Option<WebhookSecret>,
//!     route: &str,
//!     headers: &[(&str, &str)],
//!     body: &[u8],
//!     queue: &mut Vec<EventBatch>,
//! ) -> Answer {
//!     let Ok(route) = RouteKey::parse(route) else {
//!         return Answer::status(404);
//!     };
//!     match classify(headers, body) {
//!         Err(refused) => Answer::status(refused.status_hint()),
//!         Ok(Inbound::Handshake { secret }) => match block_on(store.capture(&route, secret)) {
//!             Ok(captured) => {
//!                 let reply = captured.into_reply();
//!                 Answer {
//!                     status: reply.status(),
//!                     header: Some((reply.header_name(), reply.header_value().to_vec())),
//!                 }
//!             }
//!             Err(refused) => Answer::status(refused.status_hint()),
//!         },
//!         Ok(Inbound::Delivery { signature, body }) => {
//!             let Some(secret) = secrets(&route) else {
//!                 return Answer::status(404);
//!             };
//!             // Verify the raw bytes first; only then parse them.
//!             let verified = match verify(&secret, &signature, body) {
//!                 Ok(verified) => verified,
//!                 Err(refused) => return Answer::status(refused.status_hint()),
//!             };
//!             match verified.decode() {
//!                 Ok(batch) if batch.is_heartbeat() => Answer::status(200),
//!                 Ok(batch) => {
//!                     queue.push(batch); // the work happens later, not inside Asana's 10 seconds
//!                     Answer::status(200)
//!                 }
//!                 Err(refused) => Answer::status(refused.status_hint()),
//!             }
//!         }
//!         // `Inbound` may gain kinds of request; one this code does not know is refused.
//!         Ok(_) => Answer::status(400),
//!     }
//! }
//!
//! // The creator opens a route and puts it in the target URL it sends to `POST /webhooks`.
//! let store = MemoryHandshakeStore::new();
//! let route = RouteKey::generate();
//! block_on(store.expect(&route, Duration::from_secs(60))).unwrap();
//! let path_segment = route.to_string();
//!
//! // Asana's handshake arrives while the create request is open.
//! let mut queue = Vec::new();
//! let no_secrets = |_: &RouteKey| None;
//! let answer = receive(
//!     &store, &no_secrets, &path_segment,
//!     &[("X-Hook-Secret", "0123abcd")], b"", &mut queue,
//! );
//! assert_eq!(answer.status, 200);
//! assert_eq!(answer.header, Some(("X-Hook-Secret", b"0123abcd".to_vec())));
//!
//! // The create returns 201 with the same secret; the creator takes the captured one and keeps it.
//! let secret = block_on(store.take(&route)).unwrap().unwrap();
//! let returned = WebhookSecret::from_bytes(b"0123abcd".to_vec());
//! let asana_webhooks::HandshakeOutcome::Established(secret) =
//!     asana_webhooks::settle(Some(secret), &returned)
//! else {
//!     panic!("the secrets differ: delete the webhook");
//! };
//!
//! // Later, a signed delivery: a task entered a section.
//! let body = br#"{"events":[{
//!     "user": {"gid": "1201234567890001", "resource_type": "user"},
//!     "created_at": "2026-03-02T09:15:00.000Z",
//!     "action": "added",
//!     "resource": {"gid": "1201234567890123", "resource_type": "task", "resource_subtype": "default_task"},
//!     "parent": {"gid": "1201234567890456", "resource_type": "section"}
//! }]}"#;
//! let signature = sign(&secret, body).to_hex();
//! let stored = secret.clone();
//! let secrets = move |_: &RouteKey| Some(stored.clone());
//! let answer = receive(
//!     &store, &secrets, &path_segment,
//!     &[("X-Hook-Signature", &signature)], body, &mut queue,
//! );
//! assert_eq!(answer.status, 200);
//!
//! let (task, section) = queue[0].events[0].section_entered().unwrap();
//! assert_eq!((task.to_string().as_str(), section.to_string().as_str()),
//!            ("1201234567890123", "1201234567890456"));
//!
//! // A forged delivery is refused before its body is read.
//! let forged = receive(
//!     &store, &secrets, &path_segment,
//!     &[("X-Hook-Signature", &"0".repeat(64))], body, &mut queue,
//! );
//! assert_eq!(forged.status, 401);
//! ```
//!
//! # A whole receiver
//!
//! [`Receiver`] is the steps above put together, with every status rule in one place, for a
//! framework adapter to wrap: it takes the consumer's [`HandshakeStore`], a [`SecretLookup`] and a
//! [`Sink`] (a `tokio::sync::mpsc::Sender<Delivery>` is one), and [`Receiver::receive`] turns a
//! path segment, headers and raw body into a [`Reply`]. `asana-webhooks-axum` and
//! `asana-webhooks-actix` are that wrapping for their frameworks.
//!
//! # Creating webhooks
//!
//! [`Filter`] builds the `filters` sent with `POST /webhooks`, and [`validate_filters`] checks them
//! against Asana's rules for the kind of resource being watched. Sending the request is the
//! client's job; this crate gives it the [`HandshakeStore`] protocol and [`settle`] to finish with.

mod clock;
mod dedupe;
mod events;
mod filter;
mod handshake;
mod headers;
mod inbound;
mod receive;
mod signing;

pub use clock::{Clock, ManualClock, system_clock};
pub use dedupe::{Dedupe, MemoryDedupe, Sighting};
pub use events::{
    Change, ChangeValue, DecodeError, Event, EventAction, EventBatch, EventKey, EventUser, Limits,
    ResourceGid, ResourceRef, ResourceType, UnreadableEvent, UnreadableReason,
};
pub use filter::{Filter, FilterError, is_higher_level, validate_filters};
pub use handshake::{
    Captured, HandshakeOutcome, HandshakeStore, MemoryHandshakeStore, RouteDigest, RouteKey,
    RouteKeyError, StoreError, settle,
};
pub use headers::{FnHeaders, HOOK_SECRET, HOOK_SIGNATURE, HeaderLookup};
pub use inbound::{Inbound, InboundError, classify, classify_with};
pub use receive::{
    Busy, Delivery, MemorySecrets, Receiver, Reply, RouteSecret, SecretLookup, Sink,
};
pub use signing::{
    HandshakeReply, Signature, VerifiedBody, VerifyError, WebhookSecret, handshake_reply, sign,
    verify,
};
