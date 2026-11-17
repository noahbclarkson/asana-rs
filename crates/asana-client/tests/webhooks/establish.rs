//! Establishing a webhook: the handshake captured mid-call, every way it can fail, and the
//! clean-up after a failure that follows Asana creating the webhook.

use std::sync::Arc;

use std::time::Duration;

use asana_client::openapi::ids::{Gid as _, ProjectGid, WebhookGid, WorkspaceGid};
use asana_client::webhooks::{Cleanup, WebhookError};
use asana_client::{RequestTimeout, RetryPolicy};
use asana_webhooks::{
    EventAction, Filter, HandshakeStore, MemoryHandshakeStore, RouteKey, WebhookSecret,
};
use futures_util::FutureExt;
use serde_json::json;

use crate::support::{Scripted, client, client_with, reply};
use crate::{PROJECT, WEBHOOK, body_json};

const SECRET: &str = "4b8e3d0c2a1f9e7d6c5b4a39281706f5e4d3c2b1a0918273645546372819";

fn project() -> ProjectGid {
    PROJECT.parse().unwrap()
}

/// The 201 Asana answers a create with: the webhook, and the secret beside it.
fn created(target: &str, secret: Option<&str>) -> http::Response<bytes::Bytes> {
    let mut body = json!({
        "data": {
            "gid": WEBHOOK,
            "resource_type": "webhook",
            "active": true,
            "resource": {"gid": PROJECT, "resource_type": "project", "name": "Acme launch"},
            "target": target,
        }
    });
    if let Some(secret) = secret {
        body["X-Hook-Secret"] = json!(secret);
    }
    reply(201, &body.to_string())
}

/// The route key the create's target ends with.
fn route_of(request: &http::Request<bytes::Bytes>) -> (String, RouteKey) {
    let target = body_json(request)["data"]["target"]
        .as_str()
        .unwrap()
        .to_owned();
    let url = url::Url::parse(&target).unwrap();
    let segment = url.path_segments().unwrap().next_back().unwrap().to_owned();
    (target, RouteKey::parse(&segment).unwrap())
}

/// A transport that plays Asana: on the create, the receiver captures `captured` (if any) on the
/// route in the target, and Asana answers with `returned`; deletes answer `delete_status`.
fn asana(
    store: Arc<MemoryHandshakeStore>,
    captured: Option<&'static str>,
    returned: Option<&'static str>,
    delete_status: u16,
) -> Arc<Scripted> {
    Scripted::new(move |request, _| {
        if request.method() == http::Method::DELETE {
            return Ok(reply(delete_status, r#"{"data": {}}"#));
        }
        let (target, route) = route_of(request);
        if let Some(secret) = captured {
            // Asana's handshake reaches the receiver while the create is open.
            let secret = WebhookSecret::from_header(secret).unwrap();
            store
                .capture(&route, secret)
                .now_or_never()
                .unwrap()
                .unwrap();
        }
        Ok(created(&target, returned))
    })
}

#[tokio::test]
async fn establishes_when_the_receiver_captures_the_handshake_mid_call() {
    let store = Arc::new(MemoryHandshakeStore::new());
    let transport = asana(store.clone(), Some(SECRET), Some(SECRET), 200);
    let client = client(transport.clone());
    let filters = [
        Filter::task().action(EventAction::Added),
        Filter::task()
            .action(EventAction::Changed)
            .fields(["completed", "due_on"]),
    ];

    let established = client
        .webhooks()
        .establish(project(), "https://hooks.example.com/asana/")
        .filter(filters[0].clone())
        .filters([filters[1].clone()])
        .store(&*store)
        .send()
        .await
        .unwrap();

    assert_eq!(established.secret.expose(), SECRET.as_bytes());
    assert_eq!(
        established.webhook.gid,
        Some(WEBHOOK.parse::<WebhookGid>().unwrap())
    );
    assert_eq!(transport.count(), 1, "nothing is deleted");

    let request = transport.request(0);
    assert_eq!(request.method(), http::Method::POST);
    assert!(request.uri().path().ends_with("/webhooks"));
    let (target, route) = route_of(&request);
    assert_eq!(route, established.route);
    assert_eq!(
        target,
        format!("https://hooks.example.com/asana/{}", established.route)
    );
    let body = body_json(&request);
    assert_eq!(body["data"]["resource"], PROJECT);
    // The filters go out exactly as `asana_webhooks::Filter` serialises them.
    assert_eq!(
        body["data"]["filters"],
        serde_json::to_value(filters).unwrap()
    );
    // The route is closed once settled.
    assert!(
        store
            .take(&route)
            .now_or_never()
            .unwrap()
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn a_create_without_filters_sends_none() {
    let store = Arc::new(MemoryHandshakeStore::new());
    let transport = asana(store.clone(), Some(SECRET), Some(SECRET), 200);
    let client = client(transport.clone());

    client
        .webhooks()
        .establish(project(), "https://hooks.example.com/asana")
        .store(&*store)
        .send()
        .await
        .unwrap();

    let body = body_json(&transport.request(0));
    assert!(body["data"].get("filters").is_none());
}

#[tokio::test]
async fn a_refused_handshake_is_not_captured_and_leaves_nothing() {
    let store = Arc::new(MemoryHandshakeStore::new());
    let transport = Scripted::replies(vec![Ok(reply(
        400,
        r#"{"errors": [{"message": "The remote server did not respond with the handshake secret."}]}"#,
    ))]);
    let client = client(transport.clone());

    let error = client
        .webhooks()
        .establish(project(), "https://hooks.example.com/asana")
        .store(&*store)
        .send()
        .await
        .unwrap_err();

    match &error {
        WebhookError::NotCaptured {
            refusal: Some(refusal),
            cleanup: Cleanup::NothingCreated,
        } => assert_eq!(refusal.status(), Some(http::StatusCode::BAD_REQUEST)),
        other => panic!("expected NotCaptured, got {other:?}"),
    }
    assert!(error.cleanup().unwrap().is_clean());
    assert_eq!(transport.count(), 1);
}

#[tokio::test]
async fn a_create_nobody_captured_is_deleted() {
    let store = Arc::new(MemoryHandshakeStore::new());
    let transport = asana(store.clone(), None, Some(SECRET), 200);
    let client = client(transport.clone());

    let error = client
        .webhooks()
        .establish(project(), "https://hooks.example.com/asana")
        .store(&*store)
        .send()
        .await
        .unwrap_err();

    assert!(
        matches!(
            &error,
            WebhookError::NotCaptured { refusal: None, cleanup: Cleanup::Deleted(gid) }
                if gid.as_str() == WEBHOOK
        ),
        "{error:?}"
    );
    assert_delete(&transport, 1);
}

#[tokio::test]
async fn a_different_secret_is_rejected_and_the_webhook_deleted() {
    let store = Arc::new(MemoryHandshakeStore::new());
    let transport = asana(
        store.clone(),
        Some("not-the-same-secret"),
        Some(SECRET),
        200,
    );
    let client = client(transport.clone());

    let error = client
        .webhooks()
        .establish(project(), "https://hooks.example.com/asana")
        .store(&*store)
        .send()
        .await
        .unwrap_err();

    assert!(
        matches!(&error, WebhookError::Rejected { cleanup: Cleanup::Deleted(gid) } if gid.as_str() == WEBHOOK),
        "{error:?}"
    );
    assert!(!error.to_string().contains(SECRET));
    assert_delete(&transport, 1);
}

#[tokio::test]
async fn a_failed_clean_up_names_the_webhook_left_behind() {
    let store = Arc::new(MemoryHandshakeStore::new());
    let transport = asana(
        store.clone(),
        Some("not-the-same-secret"),
        Some(SECRET),
        403,
    );
    let client = client_with(
        transport.clone(),
        RetryPolicy::never(),
        asana_client::Limits::default(),
    );

    let error = client
        .webhooks()
        .establish(project(), "https://hooks.example.com/asana")
        .store(&*store)
        .send()
        .await
        .unwrap_err();

    let cleanup = error.cleanup().unwrap();
    assert!(!cleanup.is_clean());
    assert_eq!(cleanup.remaining().unwrap().as_str(), WEBHOOK);
    assert!(error.to_string().contains(WEBHOOK), "{error}");
}

#[tokio::test]
async fn an_answer_without_a_secret_deletes_the_webhook() {
    let store = Arc::new(MemoryHandshakeStore::new());
    let transport = asana(store.clone(), Some(SECRET), None, 200);
    let client = client(transport.clone());

    let error = client
        .webhooks()
        .establish(project(), "https://hooks.example.com/asana")
        .store(&*store)
        .send()
        .await
        .unwrap_err();

    assert!(
        matches!(
            &error,
            WebhookError::MissingSecret {
                cleanup: Cleanup::Deleted(_)
            }
        ),
        "{error:?}"
    );
    assert_delete(&transport, 1);
}

#[tokio::test]
async fn a_localhost_target_refused_with_403_says_why() {
    let store = Arc::new(MemoryHandshakeStore::new());
    let transport = Scripted::replies(vec![Ok(reply(
        403,
        r#"{"errors": [{"message": "Forbidden"}]}"#,
    ))]);
    let client = client(transport.clone());

    let error = client
        .webhooks()
        .establish(project(), "https://localhost:8443/asana")
        .store(&*store)
        .send()
        .await
        .unwrap_err();

    assert!(matches!(error, WebhookError::LocalTarget(_)), "{error:?}");
    assert!(error.to_string().contains("localhost"));
}

#[tokio::test]
async fn a_403_for_a_public_target_is_a_refused_create() {
    let store = Arc::new(MemoryHandshakeStore::new());
    let transport = Scripted::replies(vec![Ok(reply(
        403,
        r#"{"errors": [{"message": "Forbidden"}]}"#,
    ))]);
    let client = client(transport);

    let error = client
        .webhooks()
        .establish(project(), "https://hooks.example.com/asana")
        .store(&*store)
        .send()
        .await
        .unwrap_err();

    assert!(
        matches!(
            error,
            WebhookError::Create {
                handshake_seen: false,
                ..
            }
        ),
        "{error:?}"
    );
}

#[tokio::test]
async fn unusable_targets_and_filters_send_nothing() {
    let store = Arc::new(MemoryHandshakeStore::new());
    let transport = Scripted::replies(vec![Ok(reply(500, "{}"))]);
    let client = client(transport.clone());
    let webhooks = client.webhooks();

    for base in [
        "http://hooks.example.com/asana",
        "https://hooks.example.com/asana?state=1",
        "https://hooks.example.com/asana#part",
        "https://user:pass@hooks.example.com/asana",
        "/asana",
    ] {
        let error = webhooks
            .establish(project(), base)
            .store(&*store)
            .send()
            .await
            .unwrap_err();
        assert!(
            matches!(error, WebhookError::InvalidTarget(_)),
            "{base}: {error:?}"
        );
    }

    // A full target must carry its route, or the receiver could not find the handshake.
    let error = webhooks
        .establish_at(
            project(),
            "https://hooks.example.com/asana?state=1",
            RouteKey::generate(),
        )
        .store(&*store)
        .send()
        .await
        .unwrap_err();
    assert!(matches!(error, WebhookError::InvalidTarget(_)), "{error:?}");

    // A workspace webhook needs filters.
    let workspace: WorkspaceGid = "1201234567890000".parse().unwrap();
    let error = webhooks
        .establish(workspace, "https://hooks.example.com/asana")
        .store(&*store)
        .send()
        .await
        .unwrap_err();
    assert!(
        matches!(error, WebhookError::InvalidFilters(_)),
        "{error:?}"
    );

    assert_eq!(transport.count(), 0);
}

#[tokio::test]
async fn a_full_target_carrying_its_route_is_sent_as_given() {
    let store = Arc::new(MemoryHandshakeStore::new());
    let transport = asana(store.clone(), Some(SECRET), Some(SECRET), 200);
    let client = client(transport.clone());
    let route = RouteKey::generate();
    let target = format!("https://hooks.example.com/asana/{route}?tenant=acme");

    let established = client
        .webhooks()
        .establish_at(project(), target.clone(), route.clone())
        .store(&*store)
        .send()
        .await
        .unwrap();

    assert_eq!(established.route, route);
    let body = body_json(&transport.request(0));
    assert_eq!(body["data"]["target"], target);
}

/// The request at `index` deletes the webhook.
fn assert_delete(transport: &Scripted, index: usize) {
    assert_eq!(transport.count(), index + 1);
    let request = transport.request(index);
    assert_eq!(request.method(), http::Method::DELETE);
    assert!(
        request
            .uri()
            .path()
            .ends_with(&format!("/webhooks/{WEBHOOK}")),
        "{}",
        request.uri()
    );
}

#[tokio::test]
async fn the_create_is_sent_with_a_timeout_long_enough_for_the_handshake() {
    let store = Arc::new(MemoryHandshakeStore::new());
    let transport = asana(store.clone(), Some(SECRET), Some(SECRET), 200);
    let client = client(transport.clone());
    let webhooks = client.webhooks();

    webhooks
        .establish(project(), "https://hooks.example.com/asana")
        .store(&*store)
        .send()
        .await
        .unwrap();
    webhooks
        .establish(project(), "https://hooks.example.com/asana")
        .timeout(Duration::from_secs(45))
        .store(&*store)
        .send()
        .await
        .unwrap();

    let timeout = |index| {
        transport
            .request(index)
            .extensions()
            .get::<RequestTimeout>()
            .copied()
    };
    assert_eq!(timeout(0), Some(RequestTimeout(Duration::from_secs(90))));
    assert_eq!(timeout(1), Some(RequestTimeout(Duration::from_secs(45))));
}

#[tokio::test]
async fn a_create_whose_answer_is_lost_after_the_handshake_hands_back_the_secret() {
    for lost in [
        Err(asana_client::TransportError::timeout("slow")),
        Ok(reply(503, "{}")),
    ] {
        let store = Arc::new(MemoryHandshakeStore::new());
        let lost = std::sync::Mutex::new(Some(lost));
        let transport = Scripted::new({
            let store = store.clone();
            move |request, _| {
                let (_, route) = route_of(request);
                // The receiver answers the handshake, then the create's answer is lost.
                let secret = WebhookSecret::from_header(SECRET).unwrap();
                store
                    .capture(&route, secret)
                    .now_or_never()
                    .unwrap()
                    .unwrap();
                lost.lock().unwrap().take().unwrap()
            }
        });
        let client = client(transport.clone());

        let error = client
            .webhooks()
            .establish(project(), "https://hooks.example.com/asana")
            .store(&*store)
            .send()
            .await
            .unwrap_err();

        let WebhookError::OutcomeUnknown { route, secret, .. } = &error else {
            panic!("expected OutcomeUnknown, got {error:?}");
        };
        assert_eq!(secret.expose(), SECRET.as_bytes());
        let (target, sent_route) = route_of(&transport.request(0));
        assert_eq!(route, &sent_route);
        assert!(target.ends_with(&route.to_string()));
        // Sent once, nothing deleted, and the route is closed.
        assert_eq!(transport.count(), 1);
        assert!(store.take(route).now_or_never().unwrap().unwrap().is_none());
        // Neither Display nor Debug shows the secret or the route.
        let text = format!("{error} {error:?}");
        assert!(!text.contains(SECRET), "{text}");
        assert!(!text.contains(&route.to_string()), "{text}");
    }
}
