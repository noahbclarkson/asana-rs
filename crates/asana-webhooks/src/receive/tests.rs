//! Every status rule of [`Receiver::receive`], with deliveries signed under a known secret.
//! Payloads are shaped like Asana's documented ones; every GID is invented.

use std::future::{Future, ready};
use std::pin::pin;
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use tokio::sync::mpsc;

use super::*;
use crate::handshake::MemoryHandshakeStore;
use crate::signing::sign;

/// A task entering a section, as Asana delivers it.
const SECTION_MOVE: &str = r#"{"events":[{
    "user": {"gid": "1201234567890001", "resource_type": "user"},
    "created_at": "2026-03-02T09:15:00.000Z",
    "action": "added",
    "resource": {"gid": "1201234567890123", "resource_type": "task"},
    "parent": {"gid": "1201234567890456", "resource_type": "section"}
}]}"#;

/// Runs a future that never waits on anything (every store and sink here answers at once).
fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = pin!(future);
    let mut context = Context::from_waker(Waker::noop());
    loop {
        if let Poll::Ready(output) = future.as_mut().poll(&mut context) {
            return output;
        }
    }
}

type Memory = Receiver<MemoryHandshakeStore, MemorySecrets, mpsc::Sender<Delivery>>;

/// A receiver with one live route, its secret, and the queue its sink fills.
struct Rig {
    receiver: Memory,
    route: RouteKey,
    secret: WebhookSecret,
    work: mpsc::Receiver<Delivery>,
}

impl Rig {
    fn new(capacity: usize) -> Self {
        let (queue, work) = mpsc::channel(capacity);
        let receiver = Receiver::new(MemoryHandshakeStore::new(), MemorySecrets::new(), queue);
        let route = RouteKey::generate();
        let secret = WebhookSecret::from_bytes(b"0123456789abcdef0123456789abcdef".to_vec());
        receiver.secrets().insert(&route, secret.clone());
        Self {
            receiver,
            route,
            secret,
            work,
        }
    }

    /// A delivery of `body` on the rig's route, signed with its secret.
    fn deliver(&self, body: &str) -> Reply {
        let signature = sign(&self.secret, body.as_bytes()).to_hex();
        self.call(&[("X-Hook-Signature", signature.as_str())], body)
    }

    fn call(&self, headers: &[(&str, &str)], body: &str) -> Reply {
        block_on(
            self.receiver
                .receive(&self.route.to_string(), headers, body.as_bytes()),
        )
    }
}

#[test]
fn a_handshake_is_captured_and_echoed() {
    let rig = Rig::new(4);
    let route = RouteKey::generate();
    block_on(
        rig.receiver
            .handshakes()
            .expect(&route, Duration::from_secs(60)),
    )
    .unwrap();

    let reply = block_on(rig.receiver.receive(
        &route.to_string(),
        &[("X-Hook-Secret", "b537207f20cbfa02")],
        b"",
    ));

    assert_eq!(reply.status(), 200);
    let echo = reply.echo().unwrap();
    assert_eq!(echo.header_name(), "X-Hook-Secret");
    assert_eq!(echo.header_value(), b"b537207f20cbfa02");
    let captured = block_on(rig.receiver.handshakes().take(&route))
        .unwrap()
        .unwrap();
    assert_eq!(captured.expose(), b"b537207f20cbfa02");
}

#[test]
fn a_handshake_nobody_expects_is_refused_without_an_echo() {
    let rig = Rig::new(4);
    let reply = rig.call(&[("X-Hook-Secret", "b537207f20cbfa02")], "");
    assert_eq!(reply.status(), 404);
    assert!(reply.echo().is_none());
}

#[test]
fn a_signed_delivery_reaches_the_sink() {
    let mut rig = Rig::new(4);
    let reply = rig.deliver(SECTION_MOVE);
    assert_eq!(reply.status(), 200);
    assert!(reply.echo().is_none());
    let delivery = rig.work.try_recv().unwrap();
    assert_eq!(delivery.route, rig.route);
    let (task, section) = delivery.batch.events[0].section_entered().unwrap();
    assert_eq!(task.to_string(), "1201234567890123");
    assert_eq!(section.to_string(), "1201234567890456");
}

#[test]
fn a_heartbeat_is_answered_without_the_sink() {
    let mut rig = Rig::new(4);
    for body in ["", r#"{"events":[]}"#] {
        assert_eq!(rig.deliver(body).status(), 200, "body {body:?}");
    }
    assert!(rig.work.try_recv().is_err());
}

#[test]
fn a_batch_of_only_unreadable_events_still_reaches_the_sink() {
    let mut rig = Rig::new(4);
    assert_eq!(
        rig.deliver(r#"{"events":[{"action":"added"}]}"#).status(),
        200
    );
    let delivery = rig.work.try_recv().unwrap();
    assert!(delivery.batch.events.is_empty());
    assert_eq!(delivery.batch.unreadable.len(), 1);
}

#[test]
fn signatures_that_are_missing_malformed_or_wrong_are_401() {
    let mut rig = Rig::new(4);
    let other = WebhookSecret::from_bytes(b"not the route's secret".to_vec());
    let wrong = sign(&other, SECTION_MOVE.as_bytes()).to_hex();
    let right = sign(&rig.secret, SECTION_MOVE.as_bytes()).to_hex();
    let altered = SECTION_MOVE.replace("1201234567890456", "1201234567890457");

    assert_eq!(rig.call(&[], SECTION_MOVE).status(), 401);
    assert_eq!(
        rig.call(&[("X-Hook-Signature", "zz")], SECTION_MOVE)
            .status(),
        401
    );
    assert_eq!(
        rig.call(&[("X-Hook-Signature", wrong.as_str())], SECTION_MOVE)
            .status(),
        401
    );
    assert_eq!(
        rig.call(&[("X-Hook-Signature", right.as_str())], &altered)
            .status(),
        401
    );
    assert!(rig.work.try_recv().is_err());
}

#[test]
fn both_headers_or_an_empty_secret_are_400() {
    let rig = Rig::new(4);
    let signature = sign(&rig.secret, b"").to_hex();
    let both = [
        ("X-Hook-Secret", "abc"),
        ("X-Hook-Signature", signature.as_str()),
    ];
    assert_eq!(rig.call(&both, "").status(), 400);
    assert_eq!(rig.call(&[("X-Hook-Secret", "")], "").status(), 400);
}

#[test]
fn routes_unknown_malformed_and_retired() {
    let rig = Rig::new(4);
    let signature = sign(&rig.secret, b"").to_hex();
    let headers = [("X-Hook-Signature", signature.as_str())];

    let stranger = RouteKey::generate().to_string();
    let reply = block_on(rig.receiver.receive(&stranger, &headers, b""));
    assert_eq!(reply.status(), 404);

    let reply = block_on(rig.receiver.receive("not-a-route-key", &headers, b""));
    assert_eq!(reply.status(), 404);

    rig.receiver.secrets().retire(&rig.route);
    assert_eq!(rig.deliver("").status(), 410);

    rig.receiver.secrets().remove(&rig.route);
    assert_eq!(rig.deliver("").status(), 404);
}

/// A lookup whose storage is down.
struct Down;

#[derive(Debug, thiserror::Error)]
#[error("the database is unreachable")]
struct Unreachable;

impl SecretLookup for Down {
    fn lookup(
        &self,
        _route: &RouteKey,
    ) -> impl Future<Output = Result<RouteSecret, StoreError>> + Send {
        ready(Err(StoreError::backend(Unreachable)))
    }
}

#[test]
fn a_failing_lookup_is_503_so_asana_retries() {
    let (queue, _work) = mpsc::channel::<Delivery>(1);
    let receiver = Receiver::new(MemoryHandshakeStore::new(), Down, queue);
    let secret = WebhookSecret::from_bytes(b"s".to_vec());
    let signature = sign(&secret, b"").to_hex();
    let reply = block_on(receiver.receive(
        &RouteKey::generate().to_string(),
        &[("X-Hook-Signature", signature.as_str())],
        b"",
    ));
    assert_eq!(reply.status(), 503);
}

#[test]
fn bodies_too_large_or_too_many_events_are_413() {
    let (queue, mut work) = mpsc::channel::<Delivery>(4);
    let limits = Limits {
        max_body_bytes: 64,
        max_events: 1,
    };
    let receiver =
        Receiver::new(MemoryHandshakeStore::new(), MemorySecrets::new(), queue).limits(limits);
    let route = RouteKey::generate();
    let secret = WebhookSecret::from_bytes(b"s".to_vec());
    receiver.secrets().insert(&route, secret.clone());
    let send = |body: &str| {
        let signature = sign(&secret, body.as_bytes()).to_hex();
        block_on(receiver.receive(
            &route.to_string(),
            &[("X-Hook-Signature", signature.as_str())],
            body.as_bytes(),
        ))
        .status()
    };

    assert_eq!(send(&"x".repeat(65)), 413);
    assert_eq!(send(r#"{"events":[{},{}]}"#), 413);
    assert_eq!(receiver.current_limits().max_events, 1);
    assert!(work.try_recv().is_err());
}

#[test]
fn a_verified_body_that_is_not_events_is_400() {
    let mut rig = Rig::new(4);
    assert_eq!(rig.deliver("{not json").status(), 400);
    assert_eq!(rig.deliver(r#"{"data":[]}"#).status(), 400);
    assert!(rig.work.try_recv().is_err());
}

#[test]
fn a_full_or_closed_sink_is_503() {
    let mut rig = Rig::new(1);
    assert_eq!(rig.deliver(SECTION_MOVE).status(), 200);
    assert_eq!(rig.deliver(SECTION_MOVE).status(), 503);
    rig.work.try_recv().unwrap();
    assert_eq!(rig.deliver(SECTION_MOVE).status(), 200);

    rig.work.close();
    while rig.work.try_recv().is_ok() {}
    assert_eq!(rig.deliver(SECTION_MOVE).status(), 503);
}

#[test]
fn clones_share_the_stores_and_debug_shows_no_secret() {
    let rig = Rig::new(4);
    let clone = rig.receiver.clone();
    let route = RouteKey::generate();
    clone
        .secrets()
        .insert(&route, WebhookSecret::from_bytes(b"hidden-value".to_vec()));
    let found = block_on(rig.receiver.secrets().lookup(&route)).unwrap();
    assert_eq!(format!("{found:?}"), "Active([REDACTED])");
    assert!(!format!("{:?}", rig.receiver.secrets()).contains("hidden"));
    assert!(!format!("{:?}", rig.receiver).contains("hidden"));
}
