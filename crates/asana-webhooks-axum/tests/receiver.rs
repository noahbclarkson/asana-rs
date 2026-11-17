//! The router end to end through `tower::ServiceExt::oneshot`: deliveries signed with a known
//! secret, as Asana signs them, against in-memory stores and a channel sink.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use asana_webhooks::{
    HandshakeStore as _, Limits, MemoryHandshakeStore, RouteDigest, RouteKey, StoreError,
    WebhookSecret, sign,
};
use asana_webhooks_axum::{Delivery, Receiver, RouteSecret, SecretLookup, router};
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use tokio::sync::mpsc;
use tower::ServiceExt as _;

/// A delivery shaped like Asana's documented example: a task added to a section (GIDs invented).
const SECTION_ENTERED: &str = r#"{"events":[{
  "user": {"gid": "1201234567890001", "resource_type": "user"},
  "created_at": "2026-03-02T09:15:00.000Z",
  "action": "added",
  "resource": {"gid": "1201234567890123", "resource_type": "task", "resource_subtype": "default_task"},
  "parent": {"gid": "1201234567890456", "resource_type": "section"}
}]}"#;

/// The routes a test knows about.
#[derive(Default)]
struct Secrets {
    routes: Mutex<HashMap<RouteDigest, Option<WebhookSecret>>>,
    failing: bool,
}

impl Secrets {
    fn active(&self, route: &RouteKey, secret: &WebhookSecret) {
        self.routes().insert(route.digest(), Some(secret.clone()));
    }

    fn retired(&self, route: &RouteKey) {
        self.routes().insert(route.digest(), None);
    }

    fn routes(&self) -> std::sync::MutexGuard<'_, HashMap<RouteDigest, Option<WebhookSecret>>> {
        self.routes.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl SecretLookup for Secrets {
    async fn lookup(&self, route: &RouteKey) -> Result<RouteSecret, StoreError> {
        if self.failing {
            return Err(StoreError::backend(std::io::Error::other("database down")));
        }
        Ok(match self.routes().get(&route.digest()) {
            Some(Some(secret)) => RouteSecret::Active(secret.clone()),
            Some(None) => RouteSecret::Retired,
            None => RouteSecret::Unknown,
        })
    }
}

type TestReceiver = Receiver<MemoryHandshakeStore, Secrets, mpsc::Sender<Delivery>>;

struct Harness {
    receiver: TestReceiver,
    deliveries: mpsc::Receiver<Delivery>,
    route: RouteKey,
    secret: WebhookSecret,
}

impl Harness {
    /// A receiver whose one route is established with a known secret.
    fn new() -> Self {
        Self::with(Secrets::default(), 8, Limits::default())
    }

    fn with(secrets: Secrets, queue: usize, limits: Limits) -> Self {
        let (sender, deliveries) = mpsc::channel(queue);
        let receiver = Receiver::new(MemoryHandshakeStore::new(), secrets, sender).limits(limits);
        let route = RouteKey::generate();
        let secret = WebhookSecret::from_bytes(b"0123456789abcdef0123456789abcdef".to_vec());
        receiver.secrets().active(&route, &secret);
        Self {
            receiver,
            deliveries,
            route,
            secret,
        }
    }

    fn app(&self) -> Router {
        router(&self.receiver, "/webhooks/asana")
    }

    fn uri(&self) -> String {
        format!("/webhooks/asana/{}", self.route)
    }

    /// Posts `body` signed with the route's secret.
    async fn deliver(&self, body: &[u8]) -> StatusCode {
        let signature = sign(&self.secret, body).to_hex();
        self.post(
            &self.uri(),
            &[("X-Hook-Signature", &signature)],
            body.to_vec(),
        )
        .await
    }

    async fn post(&self, uri: &str, headers: &[(&str, &str)], body: Vec<u8>) -> StatusCode {
        self.send(uri, headers, Body::from(body)).await.status()
    }

    async fn send(
        &self,
        uri: &str,
        headers: &[(&str, &str)],
        body: Body,
    ) -> axum::response::Response {
        let mut request = Request::builder().method("POST").uri(uri);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let request = request
            .body(body)
            .unwrap_or_else(|error| panic!("a test request is valid: {error}"));
        match self.app().oneshot(request).await {
            Ok(response) => response,
            Err(infallible) => match infallible {},
        }
    }

    fn nothing_delivered(&mut self) -> bool {
        self.deliveries.try_recv().is_err()
    }
}

#[tokio::test]
async fn a_handshake_on_an_expected_route_is_captured_and_echoed() {
    let harness = Harness::new();
    let route = RouteKey::generate();
    harness
        .receiver
        .handshakes()
        .expect(&route, Duration::from_secs(60))
        .await
        .unwrap();

    let response = harness
        .send(
            &format!("/webhooks/asana/{route}"),
            &[("X-Hook-Secret", "fresh-secret-from-asana")],
            Body::empty(),
        )
        .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["x-hook-secret"].as_bytes(),
        b"fresh-secret-from-asana"
    );
    let captured = harness
        .receiver
        .handshakes()
        .take(&route)
        .await
        .unwrap()
        .expect("the secret was captured");
    assert_eq!(captured.expose(), b"fresh-secret-from-asana");
}

#[tokio::test]
async fn a_handshake_nobody_expected_is_refused() {
    let harness = Harness::new();
    let route = RouteKey::generate();
    let status = harness
        .post(
            &format!("/webhooks/asana/{route}"),
            &[("X-Hook-Secret", "fresh-secret-from-asana")],
            Vec::new(),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_signed_delivery_reaches_the_sink() {
    let mut harness = Harness::new();
    assert_eq!(
        harness.deliver(SECTION_ENTERED.as_bytes()).await,
        StatusCode::OK
    );

    let delivery = harness
        .deliveries
        .try_recv()
        .expect("a delivery was queued");
    assert_eq!(delivery.route, harness.route);
    assert_eq!(delivery.batch.events.len(), 1);
    let (task, section) = delivery.batch.events[0]
        .section_entered()
        .expect("a section entry");
    assert_eq!(task.to_string(), "1201234567890123");
    assert_eq!(section.to_string(), "1201234567890456");
}

#[tokio::test]
async fn a_heartbeat_is_answered_without_reaching_the_sink() {
    let mut harness = Harness::new();
    assert_eq!(harness.deliver(b"").await, StatusCode::OK);
    assert_eq!(harness.deliver(br#"{"events":[]}"#).await, StatusCode::OK);
    assert!(harness.nothing_delivered());
}

#[tokio::test]
async fn a_wrong_signature_is_refused() {
    let mut harness = Harness::new();
    let forged = sign(
        &WebhookSecret::from_bytes(b"not-the-secret".to_vec()),
        SECTION_ENTERED.as_bytes(),
    )
    .to_hex();
    let status = harness
        .post(
            &harness.uri(),
            &[("X-Hook-Signature", &forged)],
            SECTION_ENTERED.as_bytes().to_vec(),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(harness.nothing_delivered());
}

#[tokio::test]
async fn a_missing_or_malformed_signature_is_refused() {
    let mut harness = Harness::new();
    let body = SECTION_ENTERED.as_bytes().to_vec();
    assert_eq!(
        harness.post(&harness.uri(), &[], body.clone()).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        harness
            .post(&harness.uri(), &[("X-Hook-Signature", "abc")], body)
            .await,
        StatusCode::UNAUTHORIZED
    );
    assert!(harness.nothing_delivered());
}

#[tokio::test]
async fn a_body_altered_by_one_byte_fails_its_signature() {
    let mut harness = Harness::new();
    let signature = sign(&harness.secret, SECTION_ENTERED.as_bytes()).to_hex();
    let mut altered = SECTION_ENTERED.as_bytes().to_vec();
    let last_digit = altered
        .iter()
        .rposition(u8::is_ascii_digit)
        .expect("the body holds a GID");
    altered[last_digit] = if altered[last_digit] == b'9' {
        b'8'
    } else {
        b'9'
    };

    let status = harness
        .post(&harness.uri(), &[("X-Hook-Signature", &signature)], altered)
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(harness.nothing_delivered());
}

#[tokio::test]
async fn an_unknown_route_is_not_found_and_never_gone() {
    let harness = Harness::new();
    let stranger = RouteKey::generate();
    let signature = sign(&harness.secret, SECTION_ENTERED.as_bytes()).to_hex();
    for uri in [
        format!("/webhooks/asana/{stranger}"),
        "/webhooks/asana/not-a-route-key".to_owned(),
    ] {
        let status = harness
            .post(
                &uri,
                &[("X-Hook-Signature", &signature)],
                SECTION_ENTERED.as_bytes().to_vec(),
            )
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
    }
}

#[tokio::test]
async fn a_retired_route_is_gone() {
    let harness = Harness::new();
    harness.receiver.secrets().retired(&harness.route);
    assert_eq!(
        harness.deliver(SECTION_ENTERED.as_bytes()).await,
        StatusCode::GONE
    );
}

#[tokio::test]
async fn a_failing_lookup_asks_asana_to_retry() {
    let harness = Harness::with(
        Secrets {
            failing: true,
            ..Secrets::default()
        },
        8,
        Limits::default(),
    );
    assert_eq!(
        harness.deliver(SECTION_ENTERED.as_bytes()).await,
        StatusCode::SERVICE_UNAVAILABLE
    );
}

#[tokio::test]
async fn an_oversized_body_is_refused_whether_or_not_it_declares_its_length() {
    let mut limits = Limits::default();
    limits.max_body_bytes = 64;
    let mut harness = Harness::with(Secrets::default(), 8, limits);
    let body = SECTION_ENTERED.as_bytes();
    assert!(body.len() > 64);

    // Declared: refused from the header, before reading (the short body read would pass).
    let status = harness
        .post(
            &harness.uri(),
            &[
                ("Content-Length", "1000"),
                ("X-Hook-Signature", &"0".repeat(64)),
            ],
            b"{}".to_vec(),
        )
        .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);

    // Streamed with no length: refused once the bytes read pass the limit.
    let signature = sign(&harness.secret, body).to_hex();
    let chunks: Vec<Result<Vec<u8>, std::io::Error>> =
        body.chunks(16).map(|chunk| Ok(chunk.to_vec())).collect();
    let response = harness
        .send(
            &harness.uri(),
            &[("X-Hook-Signature", &signature)],
            Body::from_stream(futures_util::stream::iter(chunks)),
        )
        .await;
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert!(harness.nothing_delivered());
}

#[tokio::test]
async fn too_many_events_are_refused() {
    let mut limits = Limits::default();
    limits.max_events = 1;
    let harness = Harness::with(Secrets::default(), 8, limits);
    let event = &SECTION_ENTERED[r#"{"events":["#.len()..SECTION_ENTERED.len() - 2];
    let body = format!(r#"{{"events":[{event},{event}]}}"#);
    assert_eq!(
        harness.deliver(body.as_bytes()).await,
        StatusCode::PAYLOAD_TOO_LARGE
    );
}

#[tokio::test]
async fn a_signed_body_that_is_not_an_event_batch_is_a_bad_request() {
    let mut harness = Harness::new();
    assert_eq!(harness.deliver(b"{not json").await, StatusCode::BAD_REQUEST);
    assert_eq!(
        harness.deliver(br#"{"data":[]}"#).await,
        StatusCode::BAD_REQUEST
    );
    assert!(harness.nothing_delivered());
}

#[tokio::test]
async fn a_full_sink_asks_asana_to_retry() {
    let mut harness = Harness::with(Secrets::default(), 1, Limits::default());
    assert_eq!(
        harness.deliver(SECTION_ENTERED.as_bytes()).await,
        StatusCode::OK
    );
    assert_eq!(
        harness.deliver(SECTION_ENTERED.as_bytes()).await,
        StatusCode::SERVICE_UNAVAILABLE
    );
    // Once the worker catches up, the retry is taken.
    assert!(harness.deliveries.try_recv().is_ok());
    assert_eq!(
        harness.deliver(SECTION_ENTERED.as_bytes()).await,
        StatusCode::OK
    );
}

#[tokio::test]
async fn the_router_nests_under_an_application_with_its_own_state() {
    #[derive(Clone)]
    struct AppState;

    let mut harness = Harness::new();
    let app: Router = Router::<AppState>::new()
        .merge(router(&harness.receiver, "/webhooks/asana"))
        .with_state(AppState);
    let signature = sign(&harness.secret, SECTION_ENTERED.as_bytes()).to_hex();
    let request = Request::builder()
        .method("POST")
        .uri(harness.uri())
        .header("X-Hook-Signature", signature)
        .body(Body::from(SECTION_ENTERED))
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(!harness.nothing_delivered());
}
