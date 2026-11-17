//! The whole path against `asana-fake`: the client establishes a webhook, the fake handshakes
//! into the axum router, then signs and delivers the events a task move causes.

use asana_client::{Auth, Client};
use asana_fake::FakeAsana;
use asana_webhooks::{EventAction, Filter, MemoryHandshakeStore, MemorySecrets};
use asana_webhooks_axum::{Delivery, Receiver, router};
use axum::body::{Body, Bytes};
use axum::http::{Request, Response, StatusCode};
use tokio::sync::mpsc;
use tower::ServiceExt as _;

type AppReceiver = Receiver<MemoryHandshakeStore, MemorySecrets, mpsc::Sender<Delivery>>;

/// Serves the fake's calls with the router, as a server would.
fn serve_with_router(fake: &FakeAsana, receiver: &AppReceiver) {
    let app: axum::Router = router(receiver, "/webhooks/asana");
    fake.receiver(move |request: Request<Bytes>| {
        let app = app.clone();
        async move {
            let response = match app.oneshot(request.map(Body::from)).await {
                Ok(response) => response,
                Err(infallible) => match infallible {},
            };
            let (parts, body) = response.into_parts();
            let bytes = axum::body::to_bytes(body, usize::MAX)
                .await
                .unwrap_or_default();
            Response::from_parts(parts, bytes)
        }
    });
}

#[tokio::test]
async fn a_task_moved_in_asana_reaches_the_sink_through_the_router() {
    let fake = FakeAsana::new();
    let acme = fake.workspace("Acme");
    let ada = acme.user("Ada Lovelace", "ada@example.com");
    let project = acme.project("Onboarding");
    let [todo, done] = project.sections(["To do", "Done"]);
    let task = project.task("Welcome Ada").in_section(&todo).gid();
    let client = Client::builder()
        .auth(Auth::personal_access_token("0/fake"))
        .transport(fake.transport())
        .build()
        .unwrap();

    let (sink, mut deliveries) = mpsc::channel(16);
    let receiver = Receiver::new(MemoryHandshakeStore::new(), MemorySecrets::new(), sink);
    serve_with_router(&fake, &receiver);

    // The fake calls the router with the handshake while `establish` waits on the create.
    let established = client
        .webhooks()
        .establish(project.gid(), "https://hooks.example.com/webhooks/asana")
        .filter(Filter::task().action(EventAction::Added))
        .store(receiver.handshakes())
        .send()
        .await
        .unwrap();
    let webhook = established.webhook.gid.clone().unwrap();
    receiver
        .secrets()
        .insert(&established.route, established.secret);

    // A heartbeat is answered without reaching the sink.
    let heartbeat = fake.heartbeat(&webhook).await.unwrap();
    assert_eq!(heartbeat.status, StatusCode::OK);
    assert!(deliveries.try_recv().is_err());

    fake.as_user(&ada).move_task(&task, &done);
    let delivered = fake.deliver().await;
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0].status, StatusCode::OK);

    let delivery = deliveries.try_recv().unwrap();
    assert_eq!(delivery.route, established.route);
    let entered: Vec<_> = delivery
        .batch
        .events
        .iter()
        .filter_map(asana_webhooks::Event::section_entered)
        .collect();
    assert_eq!(entered, vec![(task, done)]);
}

#[tokio::test]
async fn a_retired_route_answers_gone_and_the_fake_deletes_the_webhook() {
    let fake = FakeAsana::new();
    let acme = fake.workspace("Acme");
    let ada = acme.user("Ada Lovelace", "ada@example.com");
    let project = acme.project("Onboarding");
    let [todo, done] = project.sections(["To do", "Done"]);
    let task = project.task("Welcome Ada").in_section(&todo).gid();
    let client = Client::builder()
        .auth(Auth::personal_access_token("0/fake"))
        .transport(fake.transport())
        .build()
        .unwrap();

    let (sink, mut deliveries) = mpsc::channel(16);
    let receiver = Receiver::new(MemoryHandshakeStore::new(), MemorySecrets::new(), sink);
    serve_with_router(&fake, &receiver);
    let established = client
        .webhooks()
        .establish(project.gid(), "https://hooks.example.com/webhooks/asana")
        .store(receiver.handshakes())
        .send()
        .await
        .unwrap();
    receiver.secrets().retire(&established.route);

    fake.as_user(&ada).move_task(&task, &done);
    let delivered = fake.deliver().await;
    assert_eq!(delivered[0].status, StatusCode::GONE);
    assert!(deliveries.try_recv().is_err());

    let webhook = established.webhook.gid.clone().unwrap();
    let gone = client.webhooks().get(&webhook).await.unwrap_err();
    assert!(gone.is_not_found());
}
