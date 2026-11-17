#![forbid(unsafe_code)]

//! An [axum](https://docs.rs/axum) receiver for Asana webhooks.
//!
//! The protocol (the handshake, the signature check over the raw bytes, decoding events) and every
//! answer a receiver gives live in [`asana_webhooks`], in its [`Receiver`]; this crate only reads
//! an axum request's route segment, headers and body (with a size cap) into it, and its [`Reply`]
//! back into a response. The receiver types are re-exported here. You supply three things:
//!
//! - a [`HandshakeStore`](asana_webhooks::HandshakeStore), where a handshake's secret is captured
//!   for the code that is creating the webhook ([`MemoryHandshakeStore`] in one process, a database
//!   table across several);
//! - a [`SecretLookup`], which finds the secret of the webhook a delivery's route belongs to;
//! - a [`Sink`], which takes each verified batch. **Enqueue; do not work**: Asana wants an answer
//!   within 10 seconds and retries otherwise, so a sink records or queues the delivery and returns
//!   at once, and a worker does the processing. A `tokio::sync::mpsc::Sender<Delivery>` is a sink
//!   as it is.
//!
//! [`router`] mounts `POST {path}/{route_key}`, and [`Receiver`] documents every answer. Unknown
//! routes are answered `404`, never `410`: `410 Gone` makes Asana delete the webhook, so it is
//! answered only for a route the lookup says is [`Retired`](RouteSecret::Retired).
//!
//! # Example
//!
//! ```
//! use asana_webhooks::MemoryHandshakeStore;
//! use asana_webhooks_axum::{Delivery, MemorySecrets, Receiver};
//! use tokio::sync::mpsc;
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() {
//! let (queue, mut deliveries) = mpsc::channel::<Delivery>(1024);
//! let receiver = Receiver::new(MemoryHandshakeStore::new(), MemorySecrets::new(), queue);
//!
//! // Targets are `https://example.com/webhooks/asana/{route_key}`.
//! let app: axum::Router =
//!     axum::Router::new().merge(asana_webhooks_axum::router(&receiver, "/webhooks/asana"));
//!
//! // Once a webhook is established, record its secret under its route:
//! // `receiver.secrets().insert(&established.route, established.secret)`.
//!
//! // A worker, apart from the request, does the work.
//! tokio::spawn(async move {
//!     while let Some(delivery) = deliveries.recv().await {
//!         for event in &delivery.batch.events {
//!             let _ = event.section_entered(); // re-read the task before acting on it
//!         }
//!     }
//! });
//! # let _ = app;
//! # }
//! ```
//!
//! `examples/receiver.rs` is a complete receiver: it establishes a webhook at start-up, serves
//! it, and runs a worker that drops the integration's own writes and repeats, and re-reads each
//! task before acting.
//!
//! [`MemoryHandshakeStore`]: asana_webhooks::MemoryHandshakeStore

mod handler;

pub use asana_webhooks::{
    Busy, Delivery, MemorySecrets, Receiver, Reply, RouteSecret, SecretLookup, Sink,
};
pub use handler::{handle, router};
