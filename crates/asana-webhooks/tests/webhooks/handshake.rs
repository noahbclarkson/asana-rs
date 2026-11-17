//! The handshake store through every state, route keys, settling, and dedupe.

use std::time::Duration;

use asana_webhooks::{
    Dedupe, HandshakeOutcome, HandshakeStore, ManualClock, MemoryDedupe, MemoryHandshakeStore,
    RouteKey, RouteKeyError, Sighting, StoreError, WebhookSecret, settle,
};
use time::macros::datetime;

use crate::block_on;

fn store() -> (MemoryHandshakeStore, ManualClock) {
    let clock = ManualClock::new(datetime!(2026-03-02 09:00 UTC));
    (MemoryHandshakeStore::with_clock(clock.clock()), clock)
}

fn secret(text: &str) -> WebhookSecret {
    WebhookSecret::from_bytes(text.as_bytes().to_vec())
}

#[test]
fn expected_then_captured_then_established() {
    let (store, clock) = store();
    let route = RouteKey::generate();
    block_on(store.expect(&route, Duration::from_secs(60))).unwrap();
    assert!(matches!(
        block_on(store.expect(&route, Duration::from_secs(60))),
        Err(StoreError::RouteInUse)
    ));

    clock.advance(Duration::from_secs(5));
    let captured = block_on(store.capture(&route, secret("abc"))).unwrap();
    assert_eq!(captured.reply().header_value(), b"abc");
    // Asana retrying the same handshake is answered again; a different secret is not.
    assert!(block_on(store.capture(&route, secret("abc"))).is_ok());
    let conflict = block_on(store.capture(&route, secret("xyz"))).unwrap_err();
    assert!(matches!(conflict, StoreError::AlreadyCaptured));
    assert_eq!(conflict.status_hint(), 409);

    let taken = block_on(store.take(&route)).unwrap();
    let HandshakeOutcome::Established(kept) = settle(taken, &secret("abc")) else {
        panic!("not established");
    };
    assert_eq!(kept.expose(), b"abc");
    // Taking closes the route.
    assert!(block_on(store.take(&route)).unwrap().is_none());
    assert!(matches!(
        block_on(store.capture(&route, secret("abc"))),
        Err(StoreError::NotExpected)
    ));
}

#[test]
fn a_secret_that_differs_from_the_response_is_rejected() {
    let (store, _clock) = store();
    let route = RouteKey::generate();
    block_on(store.expect(&route, Duration::from_secs(60))).unwrap();
    block_on(store.capture(&route, secret("captured"))).unwrap();
    let taken = block_on(store.take(&route)).unwrap();
    assert!(matches!(
        settle(taken, &secret("returned")),
        HandshakeOutcome::Rejected
    ));
    assert!(matches!(
        settle(None, &secret("returned")),
        HandshakeOutcome::NotCaptured
    ));
}

#[test]
fn a_route_nobody_expects_is_refused_distinctly() {
    let (store, _clock) = store();
    let error = block_on(store.capture(&RouteKey::generate(), secret("abc"))).unwrap_err();
    assert!(matches!(error, StoreError::NotExpected));
    assert_eq!(error.status_hint(), 404);
    assert!(
        block_on(store.take(&RouteKey::generate()))
            .unwrap()
            .is_none()
    );
}

#[test]
fn an_expected_route_expires_after_its_ttl() {
    let (store, clock) = store();
    let late_capture = RouteKey::generate();
    let late_take = RouteKey::generate();
    block_on(store.expect(&late_capture, Duration::from_secs(30))).unwrap();
    block_on(store.expect(&late_take, Duration::from_secs(30))).unwrap();
    block_on(store.capture(&late_take, secret("abc"))).unwrap();

    clock.advance(Duration::from_secs(30));
    assert!(matches!(
        block_on(store.capture(&late_capture, secret("abc"))),
        Err(StoreError::Expired)
    ));
    assert!(matches!(
        block_on(store.take(&late_take)),
        Err(StoreError::Expired)
    ));

    // An expired route can be opened again.
    block_on(store.expect(&late_capture, Duration::from_secs(30))).unwrap();
    assert!(block_on(store.take(&late_capture)).unwrap().is_none());
}

#[test]
fn a_route_key_round_trips_through_a_path_segment_and_hides_in_debug() {
    let route = RouteKey::generate();
    let segment = route.to_string();
    assert_eq!(segment.len(), 43);
    assert!(
        segment
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    );
    assert_eq!(RouteKey::parse(&segment), Ok(route.clone()));
    assert_eq!(segment.parse::<RouteKey>(), Ok(route.clone()));
    assert_ne!(RouteKey::generate(), route);
    assert_eq!(route.digest(), route.clone().digest());
    assert_eq!(route.digest().to_string().len(), 64);

    let debug = format!("{route:?}");
    assert!(
        !debug.contains(&segment) && !debug.contains(&segment[..10]),
        "{debug}"
    );

    for bad in [
        "",
        "short",
        &format!("{segment}A"),
        &segment.replace(|_: char| true, "*"),
    ] {
        assert_eq!(RouteKey::parse(bad), Err(RouteKeyError));
    }
    assert_eq!(RouteKeyError.status_hint(), 404);
}

#[test]
fn dedupe_sees_each_key_once_within_its_ttl() {
    let clock = ManualClock::new(datetime!(2026-03-02 09:00 UTC));
    let dedupe = MemoryDedupe::with_clock(clock.clock());
    let event = crate::receive(
        r#"{"events":[{"created_at":"2026-03-02T09:00:00Z","action":"deleted",
           "resource":{"gid":"1201234567890123","resource_type":"task"}}]}"#,
    )
    .events
    .remove(0);
    let ttl = Duration::from_secs(24 * 3600);
    assert_eq!(
        block_on(dedupe.observe(&event.key(), ttl)).unwrap(),
        Sighting::First
    );
    assert_eq!(
        block_on(dedupe.observe(&event.key(), ttl)).unwrap(),
        Sighting::Repeat
    );
    clock.advance(ttl);
    assert_eq!(
        block_on(dedupe.observe(&event.key(), ttl)).unwrap(),
        Sighting::First
    );
}

#[test]
fn dedupe_sweeps_expired_keys() {
    let clock = ManualClock::new(datetime!(2026-03-02 09:00 UTC));
    let dedupe = MemoryDedupe::with_clock(clock.clock());
    for byte in 0..=255_u8 {
        for other in 0..8_u8 {
            let mut bytes = [0_u8; 32];
            bytes[0] = byte;
            bytes[1] = other;
            let key = asana_webhooks::EventKey::from_bytes(bytes);
            block_on(dedupe.observe(&key, Duration::from_secs(1))).unwrap();
        }
    }
    clock.advance(Duration::from_secs(2));
    block_on(dedupe.observe(
        &asana_webhooks::EventKey::from_bytes([9; 32]),
        Duration::from_secs(1),
    ))
    .unwrap();
    assert_eq!(format!("{dedupe:?}"), "MemoryDedupe { keys: 1, .. }");
}
