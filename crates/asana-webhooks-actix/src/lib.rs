#![forbid(unsafe_code)]

//! An [actix-web](https://docs.rs/actix-web) receiver for Asana webhooks.
//!
//! The protocol (the handshake, the signature check over the raw bytes, decoding events) and every
//! status rule live in [`asana_webhooks`], in its framework-neutral [`Receiver`]; this crate only
//! turns an actix-web request into the route key, headers and bytes it works on, and its [`Reply`]
//! into a response. The receiver's types are re-exported here. You supply three things:
//!
//! - a [`HandshakeStore`](asana_webhooks::HandshakeStore), where a handshake's secret is captured
//!   for the code that is creating the webhook ([`MemoryHandshakeStore`] in one process, a database
//!   table across several);
//! - a [`SecretLookup`], which finds the secret of the webhook a delivery's route belongs to
//!   ([`MemorySecrets`] in one process);
//! - a [`Sink`], which takes each verified batch. **Enqueue; do not work**: Asana wants an answer
//!   within 10 seconds and retries otherwise, so a sink records or queues the delivery and returns
//!   at once, and a worker does the processing. A `tokio::sync::mpsc::Sender<Delivery>` is a sink
//!   as it is.
//!
//! [`resource`] mounts `POST {path}/{route_key}` (and [`configure`] does the same through
//! `App::configure`); [`Receiver::receive`] documents every answer. Unknown routes are answered
//! `404`, never `410`: `410 Gone` makes Asana delete the webhook, so it is answered only for a
//! route the lookup says is [`Retired`](RouteSecret::Retired).
//!
//! # Example
//!
//! ```
//! use actix_web::{App, test};
//! use asana_webhooks::MemoryHandshakeStore;
//! use asana_webhooks_actix::{Delivery, MemorySecrets, Receiver, resource};
//! use tokio::sync::mpsc;
//!
//! # actix_web::rt::System::new().block_on(async {
//! let (queue, mut deliveries) = mpsc::channel::<Delivery>(1024);
//! let receiver = Receiver::new(MemoryHandshakeStore::new(), MemorySecrets::new(), queue);
//!
//! // Targets are `https://example.com/webhooks/asana/{route_key}`. In a server, build the
//! // receiver once, outside `HttpServer::new`, and clone it into each worker's `App`.
//! let app = test::init_service(App::new().service(resource(&receiver, "/webhooks/asana"))).await;
//!
//! // A call whose path holds no route key is answered like any unknown path.
//! let call = test::TestRequest::post()
//!     .uri("/webhooks/asana/not-a-route-key")
//!     .insert_header(("X-Hook-Secret", "0123abcd"))
//!     .to_request();
//! assert_eq!(test::call_service(&app, call).await.status(), 404);
//!
//! // A worker, apart from the request, does the work.
//! actix_web::rt::spawn(async move {
//!     while let Some(delivery) = deliveries.recv().await {
//!         for event in &delivery.batch.events {
//!             let _ = event.section_entered(); // re-read the task before acting on it
//!         }
//!     }
//! });
//! # });
//! ```
//!
//! `examples/receiver.rs` is a complete receiver: it serves under `HttpServer`, establishes a
//! webhook at start-up, and runs a worker that drops the integration's own writes and repeats, and
//! re-reads each task before acting.
//!
//! # The body
//!
//! The signature covers the body's exact bytes, so [`handle`] reads them as
//! [`web::Bytes`](actix_web::web::Bytes), never through `web::Json`. The resource carries its own
//! [`PayloadConfig`](actix_web::web::PayloadConfig) of
//! [`Limits::max_body_bytes`](asana_webhooks::Limits) (256 KiB unless changed with
//! [`Receiver::limits`]), and actix looks for the innermost one, so an app-wide `PayloadConfig` or
//! `JsonConfig` neither shrinks nor widens it. A body over the cap is answered `413` as it streams
//! in, never buffered whole.
//!
//! One thing outside this crate's reach: an application that enables actix-web's `compress-*`
//! features has a body sent with `Content-Encoding` decompressed by the extractor. Asana sends
//! deliveries uncompressed, so this does not arise with Asana itself.
//!
//! # actix's workers
//!
//! actix runs each worker thread on its own single-threaded runtime and does not need handlers to
//! be `Send`. The bounds here (`Send + Sync` collaborators whose futures are `Send`) are the
//! protocol crate's, shared with every framework, and no more: the [`Receiver`] reaches the
//! handler as [`web::Data`](actix_web::web::Data) on the resource, so the clone each worker's
//! `App` holds shares one set of stores and one queue.
//!
//! [`MemoryHandshakeStore`]: asana_webhooks::MemoryHandshakeStore

mod handler;

pub use asana_webhooks::{
    Busy, Delivery, MemorySecrets, Receiver, Reply, RouteSecret, SecretLookup, Sink,
};
pub use handler::{configure, handle, resource};
