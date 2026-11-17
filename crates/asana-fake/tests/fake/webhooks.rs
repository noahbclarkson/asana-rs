//! Webhooks: the handshake through the client's `establish`, signed deliveries verified by a
//! receiver built on `asana-webhooks`, filters, heartbeats, failures and auto-delivery.

use std::sync::{Arc, Mutex};

use asana_fake::{FakeAsana, FakeError};
use asana_webhooks::{
    EventAction, EventBatch, Filter, HandshakeStore, Inbound, MemoryHandshakeStore, RouteKey,
    WebhookSecret, classify, verify,
};
use bytes::Bytes;
use http::StatusCode;

use crate::{World, client};

/// A receiver as an application would write it: handshakes go into a store keyed by the route in
/// the path, deliveries are verified with the established secret before they are decoded.
#[derive(Clone, Default)]
struct Receiver {
    store: Arc<MemoryHandshakeStore>,
    secret: Arc<Mutex<Option<WebhookSecret>>>,
    batches: Arc<Mutex<Vec<EventBatch>>>,
}

impl Receiver {
    async fn receive(self, request: http::Request<Bytes>) -> http::Response<Bytes> {
        let headers: Vec<(String, String)> = request
            .headers()
            .iter()
            .map(|(name, value)| (name.as_str().to_owned(), value.to_str().unwrap().to_owned()))
            .collect();
        let status = match classify(&headers, request.body()) {
            Ok(Inbound::Handshake { secret }) => {
                let segment = request.uri().path().rsplit('/').next().unwrap();
                let route = RouteKey::parse(segment).unwrap();
                let reply = self
                    .store
                    .capture(&route, secret)
                    .await
                    .unwrap()
                    .into_reply();
                return http::Response::builder()
                    .status(reply.status())
                    .header(reply.header_name(), reply.header_value())
                    .body(Bytes::new())
                    .unwrap();
            }
            Ok(Inbound::Delivery { signature, body }) => {
                let secret = self.secret.lock().unwrap().clone().unwrap();
                match verify(&secret, &signature, body) {
                    Ok(verified) => {
                        self.batches
                            .lock()
                            .unwrap()
                            .push(verified.decode().unwrap());
                        StatusCode::OK
                    }
                    Err(_) => StatusCode::UNAUTHORIZED,
                }
            }
            Ok(_) | Err(_) => StatusCode::BAD_REQUEST,
        };
        http::Response::builder()
            .status(status)
            .body(Bytes::new())
            .unwrap()
    }

    fn install(&self, fake: &FakeAsana) {
        let receiver = self.clone();
        fake.receiver(move |request| receiver.clone().receive(request));
    }
}

#[tokio::test]
async fn an_established_webhook_receives_a_signed_batch_for_a_section_move() {
    let world = World::new();
    let client = client(&world.fake);
    let receiver = Receiver::default();
    receiver.install(&world.fake);

    let established = client
        .webhooks()
        .establish(world.project.clone(), "https://hooks.example.com/asana")
        .filter(Filter::task().action(EventAction::Added))
        .store(&*receiver.store)
        .send()
        .await
        .unwrap();
    *receiver.secret.lock().unwrap() = Some(established.secret.clone());
    let webhook = established.webhook.gid.clone().unwrap();

    world
        .fake
        .as_user(&world.ada)
        .move_task(&world.task, &world.sent);
    let deliveries = world.fake.deliver().await;

    assert_eq!(deliveries.len(), 1);
    assert_eq!(deliveries[0].webhook, webhook);
    assert_eq!(deliveries[0].status, StatusCode::OK);
    // The filter keeps the task's `added` to its new section and drops the `removed` from the
    // old one and the story's `added`.
    assert_eq!(deliveries[0].events, 1);
    let batches = receiver.batches.lock().unwrap().clone();
    let event = &batches[0].events[0];
    assert_eq!(
        event.section_entered(),
        Some((world.task.clone(), world.sent.clone()))
    );
    assert!(event.is_by(&world.ada));

    let heartbeat = world.fake.heartbeat(&webhook).await.unwrap();
    assert_eq!(heartbeat.status, StatusCode::OK);
    assert!(receiver.batches.lock().unwrap()[1].is_heartbeat());

    let health = client.webhooks().health(&webhook).await.unwrap();
    assert!(!health.is_failing());
}

/// A receiver that echoes every handshake and keeps every delivery's raw request.
fn echoing(fake: &FakeAsana, status: StatusCode) -> Arc<Mutex<Vec<http::Request<Bytes>>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let kept = Arc::clone(&seen);
    fake.receiver(move |request: http::Request<Bytes>| {
        let kept = Arc::clone(&kept);
        async move {
            if let Some(secret) = request.headers().get("x-hook-secret").cloned() {
                return http::Response::builder()
                    .status(StatusCode::OK)
                    .header("X-Hook-Secret", secret)
                    .body(Bytes::new())
                    .unwrap();
            }
            kept.lock().unwrap().push(request);
            http::Response::builder()
                .status(status)
                .body(Bytes::new())
                .unwrap()
        }
    });
    seen
}

#[tokio::test]
async fn api_changes_reach_an_unfiltered_task_webhook_signed_with_its_secret() {
    let world = World::new();
    let client = client(&world.fake);
    let seen = echoing(&world.fake, StatusCode::OK);
    let webhook = world
        .fake
        .create_webhook(&world.task, "https://hooks.example.com/asana/task")
        .await
        .unwrap();

    client
        .tasks()
        .update(world.task.clone())
        .completed(true)
        .send()
        .await
        .unwrap();
    client
        .stories()
        .comment(world.task.clone(), "Done")
        .send()
        .await
        .unwrap();
    let deliveries = world.fake.deliver().await;

    assert_eq!(deliveries.len(), 1);
    assert_eq!(deliveries[0].events, 2);
    let request = seen.lock().unwrap().remove(0);
    assert_eq!(request.uri(), "https://hooks.example.com/asana/task");
    let signature = asana_webhooks::Signature::from_hex(
        request.headers()["x-hook-signature"].to_str().unwrap(),
    )
    .unwrap();
    let secret = world.fake.webhook_secret(&webhook).unwrap();
    let batch = verify(&secret, &signature, request.body())
        .unwrap()
        .decode()
        .unwrap();
    assert_eq!(
        batch.events[0].task_changed(),
        Some((world.task.clone(), "completed"))
    );
    assert!(batch.events[1].story_added().is_some());
}

#[tokio::test]
async fn a_receiver_that_does_not_echo_the_secret_gets_no_webhook() {
    let world = World::new();
    world.fake.receiver(|_request| async {
        http::Response::builder()
            .status(StatusCode::OK)
            .body(Bytes::new())
            .unwrap()
    });

    let refused = world
        .fake
        .create_webhook(&world.project, "https://hooks.example.com/asana")
        .await
        .unwrap_err();
    assert!(matches!(
        refused,
        FakeError::Refused {
            status: StatusCode::BAD_REQUEST,
            ..
        }
    ));

    let store = MemoryHandshakeStore::new();
    let error = client(&world.fake)
        .webhooks()
        .establish(world.project.clone(), "https://hooks.example.com/asana")
        .store(&store)
        .send()
        .await;
    assert!(error.is_err());

    let local = world
        .fake
        .create_webhook(&world.project, "https://localhost/asana")
        .await
        .unwrap_err();
    assert!(matches!(
        local,
        FakeError::Refused {
            status: StatusCode::FORBIDDEN,
            ..
        }
    ));
}

#[tokio::test]
async fn a_failed_delivery_is_kept_for_the_next_and_410_deletes_the_webhook() {
    let world = World::new();
    let client = client(&world.fake);
    let _ = echoing(&world.fake, StatusCode::SERVICE_UNAVAILABLE);
    let webhook = world
        .fake
        .create_webhook(&world.project, "https://hooks.example.com/asana")
        .await
        .unwrap();
    world
        .fake
        .as_user(&world.ada)
        .rename_task(&world.task, "Welcome pack, revised");

    let failed = world.fake.deliver().await;
    assert_eq!(failed[0].status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(world.fake.pending_events(&webhook).len(), 1);
    let health = client.webhooks().health(&webhook).await.unwrap();
    assert!(health.is_failing());

    let _ = echoing(&world.fake, StatusCode::GONE);
    let gone = world.fake.deliver().await;
    assert_eq!(gone[0].status, StatusCode::GONE);
    assert!(
        client
            .webhooks()
            .get(&webhook)
            .await
            .unwrap_err()
            .is_not_found()
    );
}

#[tokio::test]
async fn auto_delivery_sends_events_before_the_request_returns() {
    let world = World::new();
    let client = client(&world.fake);
    let seen = echoing(&world.fake, StatusCode::OK);
    world
        .fake
        .create_webhook(&world.project, "https://hooks.example.com/asana")
        .await
        .unwrap();
    world.fake.set_auto_deliver(true);

    client
        .sections()
        .add_task(world.sending.clone(), world.task.clone())
        .send()
        .await
        .unwrap();

    let delivered = seen.lock().unwrap().len();
    assert_eq!(delivered, 1);
}
