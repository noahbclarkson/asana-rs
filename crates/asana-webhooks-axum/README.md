# asana-webhooks-axum

An [axum](https://docs.rs/axum) receiver for Asana webhooks, built on
[`asana-webhooks`](../asana-webhooks).

The protocol stays in `asana-webhooks`: the handshake, the constant-time signature check over the
body's exact bytes, decoding events, and every answer (its `Receiver`). This crate reads an axum
request's route segment, headers and body (with a size cap) into that receiver and turns its
`Reply` into a response. The receiver types are re-exported from this crate.

```rust
let (queue, mut deliveries) = tokio::sync::mpsc::channel::<Delivery>(1024);
let receiver = Receiver::new(MemoryHandshakeStore::new(), MemorySecrets::new(), queue);

// Targets are `https://example.com/webhooks/asana/{route_key}`.
let app: axum::Router = axum::Router::new().merge(router(&receiver, "/webhooks/asana"));

// Create webhooks against the same handshake store the receiver captures into.
let established = client
    .webhooks()
    .establish(project, "https://example.com/webhooks/asana")
    .store(receiver.handshakes())
    .send()
    .await?;
receiver.secrets().insert(&established.route, established.secret);
```

You supply:

| | What it is | In one process |
|---|---|---|
| `HandshakeStore` | where a handshake's secret is captured for the code creating the webhook | `MemoryHandshakeStore` |
| `SecretLookup` | finds the secret of the webhook a delivery's route belongs to | `MemorySecrets`; across processes, a table keyed by `RouteKey::digest` |
| `Sink` | takes each verified batch | `tokio::sync::mpsc::Sender<Delivery>` |

**Enqueue; do not work.** Asana wants an answer within 10 seconds and retries a delivery that
does not get one, so the sink queues the delivery and returns, and a worker does the processing.
A full queue answers `503`, and Asana retries later.

## What it answers

| Request | Answer |
|---|---|
| a route segment that is not a route key | `404` |
| a body over the limit (256 KiB by default), read no further than the limit | `413` |
| a handshake on a route the creator opened | `200` with `X-Hook-Secret` echoed |
| a handshake on any other route | `404` (or `409`, `503`, as the store says) |
| no signature, a malformed one, or a wrong one | `401` |
| a delivery on a route the lookup does not know | `404` |
| a delivery on a route the lookup says is retired | `410`: Asana deletes the webhook |
| a lookup or a sink that cannot answer now | `503`: Asana retries |
| a verified body that is not an event batch | `400` (`413` for too many events) |
| a heartbeat (no events) | `200`; the sink is not called |
| a delivery the sink took | `200` |

An unknown route is `404`, never `410`. `410 Gone` makes Asana delete the webhook, so a lost row or
a misconfigured deploy must not answer it; return `RouteSecret::Retired` only for a route you know
you no longer want.

Each request runs in an `asana_webhook` tracing span with the route (as the start of its digest),
the kind of call, the number of events and the status. The secret, the signature and the body are
never logged.

## Example

[`examples/receiver.rs`](examples/receiver.rs) is a complete receiver: it serves the router,
establishes a webhook at start-up, and runs a worker that drops the integration's own writes,
stale events and repeats, and re-reads each task before acting.

```text
ASANA_ACCESS_TOKEN=... ASANA_RESOURCE_GID=1201234567890456 \
ASANA_WEBHOOK_BASE_URL=https://hooks.example.com/webhooks/asana \
cargo run -p asana-webhooks-axum --example receiver
```

## Licence

MIT or Apache-2.0, at your option.
