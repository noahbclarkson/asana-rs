//! The actix receiver end to end through `actix_web::test`, with deliveries signed under a known
//! secret. Payloads are shaped like Asana's documented ones; every GID is invented.

use std::time::Duration;

use actix_web::App;
use actix_web::http::StatusCode;
use actix_web::test::{self, TestRequest};
use actix_web::web::{self, JsonConfig, PayloadConfig};
use asana_webhooks::Limits;
use asana_webhooks::{HandshakeStore, MemoryHandshakeStore, RouteKey, WebhookSecret, sign};
use asana_webhooks_actix::{Delivery, MemorySecrets, Receiver, configure, resource};
use tokio::sync::mpsc;

const BASE: &str = "/webhooks/asana";

/// A task entering a section, as Asana delivers it.
const SECTION_MOVE: &str = r#"{"events":[{
    "user": {"gid": "1201234567890001", "resource_type": "user"},
    "created_at": "2026-03-02T09:15:00.000Z",
    "action": "added",
    "resource": {"gid": "1201234567890123", "resource_type": "task", "resource_subtype": "default_task"},
    "parent": {"gid": "1201234567890456", "resource_type": "section"}
}]}"#;

type Webhooks = Receiver<MemoryHandshakeStore, MemorySecrets, mpsc::Sender<Delivery>>;

/// A receiver with one live route, its secret, and the queue its sink fills.
struct Rig {
    route: RouteKey,
    secret: WebhookSecret,
    work: mpsc::Receiver<Delivery>,
    webhooks: Webhooks,
}

impl Rig {
    fn new() -> Self {
        Self::with_queue(16)
    }

    fn with_queue(capacity: usize) -> Self {
        let (queue, work) = mpsc::channel(capacity);
        let webhooks = Receiver::new(MemoryHandshakeStore::new(), MemorySecrets::new(), queue);
        let route = RouteKey::generate();
        let secret = WebhookSecret::from_bytes(b"0123456789abcdef0123456789abcdef".to_vec());
        webhooks.secrets().insert(&route, secret.clone());
        Self {
            route,
            secret,
            work,
            webhooks,
        }
    }

    fn uri(&self) -> String {
        format!("{BASE}/{}", self.route)
    }

    /// A delivery of `body` signed with the route's secret.
    fn signed(&self, body: &str) -> TestRequest {
        TestRequest::post()
            .uri(&self.uri())
            .insert_header((
                "X-Hook-Signature",
                sign(&self.secret, body.as_bytes()).to_hex(),
            ))
            .set_payload(body.to_owned())
    }
}

/// The receiver mounted at [`BASE`] in an otherwise empty app.
macro_rules! app {
    ($webhooks:expr) => {
        test::init_service(App::new().service(resource(&$webhooks, BASE))).await
    };
}

#[actix_web::test]
async fn a_handshake_is_captured_and_its_secret_echoed() {
    let rig = Rig::new();
    let route = RouteKey::generate();
    rig.webhooks
        .handshakes()
        .expect(&route, Duration::from_secs(60))
        .await
        .unwrap();
    let app = app!(rig.webhooks);

    let response = test::call_service(
        &app,
        TestRequest::post()
            .uri(&format!("{BASE}/{route}"))
            .insert_header(("X-Hook-Secret", "b537207f20cbfa02"))
            .to_request(),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get("x-hook-secret").unwrap().as_bytes(),
        b"b537207f20cbfa02"
    );
    let captured = rig
        .webhooks
        .handshakes()
        .take(&route)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(captured.expose(), b"b537207f20cbfa02");
}

#[actix_web::test]
async fn a_handshake_nobody_expects_is_refused() {
    let rig = Rig::new();
    let app = app!(rig.webhooks);
    let response = test::call_service(
        &app,
        TestRequest::post()
            .uri(&format!("{BASE}/{}", RouteKey::generate()))
            .insert_header(("X-Hook-Secret", "b537207f20cbfa02"))
            .to_request(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert!(response.headers().get("x-hook-secret").is_none());
}

#[actix_web::test]
async fn a_signed_delivery_reaches_the_sink() {
    let mut rig = Rig::new();
    let app = app!(rig.webhooks);

    let response = test::call_service(&app, rig.signed(SECTION_MOVE).to_request()).await;

    assert_eq!(response.status(), StatusCode::OK);
    let delivery = rig.work.try_recv().unwrap();
    assert_eq!(delivery.route, rig.route);
    let (task, section) = delivery.batch.events[0].section_entered().unwrap();
    assert_eq!(task.to_string(), "1201234567890123");
    assert_eq!(section.to_string(), "1201234567890456");
}

#[actix_web::test]
async fn a_heartbeat_is_answered_without_reaching_the_sink() {
    let mut rig = Rig::new();
    let app = app!(rig.webhooks);
    for body in ["", r#"{"events":[]}"#] {
        let response = test::call_service(&app, rig.signed(body).to_request()).await;
        assert_eq!(response.status(), StatusCode::OK, "body {body:?}");
    }
    assert!(rig.work.try_recv().is_err());
}

#[actix_web::test]
async fn a_wrong_signature_is_refused() {
    let mut rig = Rig::new();
    let app = app!(rig.webhooks);
    let other = WebhookSecret::from_bytes(b"not the route's secret".to_vec());
    let response = test::call_service(
        &app,
        TestRequest::post()
            .uri(&rig.uri())
            .insert_header((
                "X-Hook-Signature",
                sign(&other, SECTION_MOVE.as_bytes()).to_hex(),
            ))
            .set_payload(SECTION_MOVE)
            .to_request(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert!(rig.work.try_recv().is_err());
}

#[actix_web::test]
async fn a_missing_or_malformed_signature_is_refused() {
    let mut rig = Rig::new();
    let app = app!(rig.webhooks);
    let unsigned = TestRequest::post()
        .uri(&rig.uri())
        .set_payload(SECTION_MOVE)
        .to_request();
    assert_eq!(
        test::call_service(&app, unsigned).await.status(),
        StatusCode::UNAUTHORIZED
    );
    let malformed = TestRequest::post()
        .uri(&rig.uri())
        .insert_header(("X-Hook-Signature", "not hex"))
        .set_payload(SECTION_MOVE)
        .to_request();
    assert_eq!(
        test::call_service(&app, malformed).await.status(),
        StatusCode::UNAUTHORIZED
    );
    assert!(rig.work.try_recv().is_err());
}

#[actix_web::test]
async fn a_body_altered_by_one_byte_is_refused() {
    let mut rig = Rig::new();
    let app = app!(rig.webhooks);
    let signature = sign(&rig.secret, SECTION_MOVE.as_bytes()).to_hex();
    // The section's GID ends in 7 instead of 6: one byte.
    let altered = SECTION_MOVE.replace("1201234567890456", "1201234567890457");
    assert_eq!(altered.len(), SECTION_MOVE.len());
    let response = test::call_service(
        &app,
        TestRequest::post()
            .uri(&rig.uri())
            .insert_header(("X-Hook-Signature", signature))
            .set_payload(altered)
            .to_request(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert!(rig.work.try_recv().is_err());
}

#[actix_web::test]
async fn an_unknown_route_is_not_found_and_a_retired_one_gone() {
    let rig = Rig::new();
    let app = app!(rig.webhooks);

    // A well-formed route key nobody stored.
    let stranger = RouteKey::generate();
    let response = test::call_service(
        &app,
        TestRequest::post()
            .uri(&format!("{BASE}/{stranger}"))
            .insert_header(("X-Hook-Signature", sign(&rig.secret, b"").to_hex()))
            .to_request(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    // A path segment that is not a route key at all.
    let response = test::call_service(
        &app,
        TestRequest::post()
            .uri(&format!("{BASE}/nope"))
            .insert_header(("X-Hook-Signature", sign(&rig.secret, b"").to_hex()))
            .to_request(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    // A route the consumer retired: 410 makes Asana delete the webhook.
    rig.webhooks.secrets().retire(&rig.route);
    let response = test::call_service(&app, rig.signed("").to_request()).await;
    assert_eq!(response.status(), StatusCode::GONE);
}

#[actix_web::test]
async fn an_oversized_body_is_refused() {
    let (queue, mut work) = mpsc::channel::<Delivery>(4);
    let mut limits = Limits::default();
    limits.max_body_bytes = 64;
    let webhooks =
        Receiver::new(MemoryHandshakeStore::new(), MemorySecrets::new(), queue).limits(limits);
    let route = RouteKey::generate();
    let secret = WebhookSecret::from_bytes(b"0123456789abcdef".to_vec());
    webhooks.secrets().insert(&route, secret.clone());
    let app = test::init_service(App::new().service(resource(&webhooks, BASE))).await;

    let body = format!(r#"{{"events":[],"padding":"{}"}}"#, "x".repeat(64));
    let response = test::call_service(
        &app,
        TestRequest::post()
            .uri(&format!("{BASE}/{route}"))
            .insert_header(("X-Hook-Signature", sign(&secret, body.as_bytes()).to_hex()))
            .set_payload(body)
            .to_request(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert!(work.try_recv().is_err());

    // The default cap is Asana's 256 KiB, not actix's own default: a body just under it passes.
    let rig = Rig::new();
    let app = app!(rig.webhooks);
    let near_cap = format!(
        r#"{{"events":[],"padding":"{}"}}"#,
        "x".repeat(Limits::default().max_body_bytes - 64)
    );
    let response = test::call_service(&app, rig.signed(&near_cap).to_request()).await;
    assert_eq!(response.status(), StatusCode::OK);
    let over_cap = "x".repeat(Limits::default().max_body_bytes + 1);
    let response = test::call_service(&app, rig.signed(&over_cap).to_request()).await;
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

#[actix_web::test]
async fn a_verified_body_that_is_not_events_is_a_bad_request() {
    let mut rig = Rig::new();
    let app = app!(rig.webhooks);
    for body in ["{not json", r#"{"data":[]}"#] {
        let response = test::call_service(&app, rig.signed(body).to_request()).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "body {body:?}");
    }
    assert!(rig.work.try_recv().is_err());
}

#[actix_web::test]
async fn a_full_sink_answers_503_so_asana_retries() {
    let mut rig = Rig::with_queue(1);
    let app = app!(rig.webhooks);

    let first = test::call_service(&app, rig.signed(SECTION_MOVE).to_request()).await;
    assert_eq!(first.status(), StatusCode::OK);
    let second = test::call_service(&app, rig.signed(SECTION_MOVE).to_request()).await;
    assert_eq!(second.status(), StatusCode::SERVICE_UNAVAILABLE);

    // Once the worker catches up, Asana's retry is taken.
    rig.work.try_recv().unwrap();
    let retry = test::call_service(&app, rig.signed(SECTION_MOVE).to_request()).await;
    assert_eq!(retry.status(), StatusCode::OK);
}

#[actix_web::test]
async fn app_wide_body_configs_do_not_reach_the_receiver() {
    let mut rig = Rig::new();
    let app = test::init_service(
        App::new()
            .app_data(PayloadConfig::new(8))
            .app_data(web::Data::new(PayloadConfig::new(8)))
            .app_data(JsonConfig::default().limit(8))
            .service(resource(&rig.webhooks, BASE)),
    )
    .await;

    let response = test::call_service(&app, rig.signed(SECTION_MOVE).to_request()).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(rig.work.try_recv().unwrap().batch.events.len(), 1);
}

#[actix_web::test]
async fn configure_mounts_it_inside_a_scope() {
    let mut rig = Rig::new();
    let app = test::init_service(
        App::new().service(web::scope("/hooks").configure(configure(&rig.webhooks, "/asana/"))),
    )
    .await;

    let response = test::call_service(
        &app,
        TestRequest::post()
            .uri(&format!("/hooks/asana/{}", rig.route))
            .insert_header((
                "x-hook-signature",
                sign(&rig.secret, SECTION_MOVE.as_bytes()).to_hex(),
            ))
            .set_payload(SECTION_MOVE)
            .to_request(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(rig.work.try_recv().is_ok());

    // Asana only ever posts; anything else is actix's 405.
    let response = test::call_service(
        &app,
        TestRequest::get()
            .uri(&format!("/hooks/asana/{}", rig.route))
            .to_request(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
}
