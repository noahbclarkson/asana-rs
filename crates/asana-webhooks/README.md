# asana-webhooks

Receive [Asana webhooks](https://developers.asana.com/docs/webhooks-guide): the protocol, with no
HTTP client and no web framework.

- `classify` tells a handshake (`X-Hook-Secret`) from a delivery (`X-Hook-Signature`).
- `HandshakeStore` is where the code creating a webhook and the receiver answering Asana's
  handshake meet, possibly in different processes; `MemoryHandshakeStore` serves one process.
  `settle` compares the captured secret with the one Asana returned, in constant time.
- `verify` checks a delivery's signature over the raw body, in constant time, before anything
  parses it; only the `VerifiedBody` it returns can be decoded.
- `EventBatch` and `Event` are the typed events, lenient where Asana is (unknown resource types
  and actions are kept), with helpers such as `Event::section_entered` and `Event::key` for
  de-duplication through a `Dedupe` store.
- `Filter` builds a webhook's filters, and `validate_filters` checks Asana's rules for them.
- `Receiver` puts the parts together the way every receiver must, framework-neutral: give it a
  `HandshakeStore`, a `SecretLookup` (`MemorySecrets` in one process) and a `Sink` (a
  `tokio::sync::mpsc::Sender<Delivery>` is one), hand it the route segment, headers and raw body,
  and write back the `Reply`. The axum and actix-web adapters are this and nothing more.

Without `Receiver`, the heart of a receiver looks like this, from the crate's doc test, where
`store` is the handshake store, `secrets` finds an established webhook's secret by its route, and
`queue` is where batches wait for a worker:

```rust
match classify(headers, body) {
    Err(refused) => Answer::status(refused.status_hint()),
    Ok(Inbound::Handshake { secret }) => match block_on(store.capture(&route, secret)) {
        Ok(captured) => {
            let reply = captured.into_reply();
            Answer {
                status: reply.status(),
                header: Some((reply.header_name(), reply.header_value().to_vec())),
            }
        }
        Err(refused) => Answer::status(refused.status_hint()),
    },
    Ok(Inbound::Delivery { signature, body }) => {
        let Some(secret) = secrets(&route) else {
            return Answer::status(404);
        };
        // Verify the raw bytes first; only then parse them.
        let verified = match verify(&secret, &signature, body) {
            Ok(verified) => verified,
            Err(refused) => return Answer::status(refused.status_hint()),
        };
        match verified.decode() {
            Ok(batch) if batch.is_heartbeat() => Answer::status(200),
            Ok(batch) => {
                queue.push(batch); // the work happens later, not inside Asana's 10 seconds
                Answer::status(200)
            }
            Err(refused) => Answer::status(refused.status_hint()),
        }
    }
    // `Inbound` may gain kinds of request; one this code does not know is refused.
    Ok(_) => Answer::status(400),
}
```

The doc test drives the store's future with a small `block_on` so it needs no runtime; a server
awaits it.

Asana wants a delivery answered within 10 seconds, retries for 24 hours, and deletes a webhook
after 24 hours without a successful delivery (heartbeats included): queue the batch, answer
`200`, and do the work afterwards. Ready-made mounts of `Receiver` are
[`asana-webhooks-axum`](https://crates.io/crates/asana-webhooks-axum) and
[`asana-webhooks-actix`](https://crates.io/crates/asana-webhooks-actix); establishing webhooks,
checking their health and turning events into work is in
[`asana-client`](https://crates.io/crates/asana-client)'s `webhooks` module.

This is an unofficial crate, not affiliated with Asana. It is part of
[asana-rs](https://github.com/noahbclarkson/asana-rs), whose `docs/webhooks.md` covers running
webhooks in production. Licensed under MIT or Apache-2.0.
