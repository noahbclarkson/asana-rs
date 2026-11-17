# asana-webhooks-actix

An [actix-web](https://actix.rs) receiver for Asana webhooks, built on
[`asana-webhooks`](../asana-webhooks).

The protocol (the handshake, the constant-time signature check over the raw bytes, decoding the
events) and every status rule are in `asana-webhooks`, in its framework-neutral `Receiver`, whose
types this crate re-exports. This crate mounts it at `POST {path}/{route_key}` (`resource`, or
`configure` for `App::configure`), reads the body as raw bytes under a cap of its own, and writes
the answer back.

```rust
use actix_web::{App, HttpServer};
use asana_webhooks::MemoryHandshakeStore;
use asana_webhooks_actix::{Delivery, MemorySecrets, Receiver, resource};
use tokio::sync::mpsc;

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    let (queue, mut deliveries) = mpsc::channel::<Delivery>(1024);
    let receiver = Receiver::new(MemoryHandshakeStore::new(), MemorySecrets::new(), queue);

    // The work happens here, never inside the request: Asana allows ten seconds.
    actix_web::rt::spawn(async move {
        while let Some(delivery) = deliveries.recv().await {
            println!("{} events", delivery.batch.events.len());
        }
    });

    let served = receiver.clone();
    HttpServer::new(move || App::new().service(resource(&served, "/webhooks/asana")))
        .bind("127.0.0.1:8080")?
        .run()
        .await
}
```

You supply three things: a `HandshakeStore` (where the handshake's secret meets the code creating
the webhook), a `SecretLookup` (each route's stored secret) and a `Sink` (where verified batches
go). The in-memory ones serve a single process; across instances, implement the two stores over a
database. A `tokio::sync::mpsc::Sender<Delivery>` is a sink as it is.

## What it answers

| The call | The answer |
|---|---|
| a path segment that is not a route key | `404` |
| a body over the cap (256 KiB by default) | `413`, read no further |
| a handshake on a route that is expected | `200`, `X-Hook-Secret` echoed |
| a missing, malformed or wrong signature | `401` |
| a delivery on a route nobody knows | `404`, which Asana retries |
| a delivery on a route you retired | `410`, which makes Asana delete the webhook |
| a verified body that is not an event batch | `400` |
| a heartbeat | `200`; the sink is not called |
| a batch the sink took | `200` |
| a full sink, or a store that failed | `503`, which Asana retries |

An unknown route is `404` and not `410` on purpose: `410` deletes the webhook at once and for
good, and a route can look unknown for reasons that pass (a restored backup, a deploy reading the
wrong database). Say `RouteSecret::Retired` for a route you mean to be rid of.

## The body

The handler reads `web::Bytes`, never `web::Json`: the signature covers the exact bytes. The
resource carries its own `PayloadConfig`, which actix finds before any app-wide one, so a small
app-wide `PayloadConfig` or `JsonConfig` does not break the receiver.

## A complete server

[`examples/receiver.rs`](examples/receiver.rs) is a full receiver to read: the stores, the queue,
a worker that drops the integration's own changes, stale events and repeats with the client's
`Resolver` and re-reads each task before acting, and the webhook established at start-up once the
server is answering. Run it with `ASANA_ACCESS_TOKEN`, `ASANA_PROJECT_GID` and `PUBLIC_URL` set:

```text
cargo run -p asana-webhooks-actix --example receiver
```

## Licence

MIT or Apache-2.0, at your option.
