//! The webhook protocol end to end, on payloads shaped like Asana's documented ones (GIDs invented).
//!
//! One test binary with a module per area, so the crate's tests link once.

mod events;
mod filter;
mod handshake;
mod inbound;

use std::future::Future;
use std::pin::pin;
use std::task::{Context, Poll, Waker};

use asana_webhooks::{EventBatch, WebhookSecret, sign, verify};

/// Runs a future that never waits on anything (every store here answers at once).
pub fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = pin!(future);
    let mut context = Context::from_waker(Waker::noop());
    loop {
        if let Poll::Ready(output) = future.as_mut().poll(&mut context) {
            return output;
        }
    }
}

/// The secret from Asana's own example of a `201` response to a webhook creation.
pub fn documented_secret() -> WebhookSecret {
    WebhookSecret::from_bytes(
        b"b537207f20cbfa02357cf448134da559e8bd39d61597dcd5631b8012eae53e81".to_vec(),
    )
}

/// Signs and verifies `body`, then decodes it, as a receiver would.
pub fn receive(body: &str) -> EventBatch {
    let secret = documented_secret();
    let signature = sign(&secret, body.as_bytes());
    match verify(&secret, &signature, body.as_bytes()).map(|verified| verified.decode()) {
        Ok(Ok(batch)) => batch,
        Ok(Err(error)) => panic!("decode failed: {error}"),
        Err(error) => panic!("verify failed: {error}"),
    }
}
