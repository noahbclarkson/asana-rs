//! The actix receiver against the in-memory Asana, through the actix mapping rather than around
//! it: the client establishes a webhook (the fake performs the handshake through the actix
//! service), a person moves a task, and the signed delivery reaches the sink. Names are invented.

use actix_web::test::{self, TestRequest};
use actix_web::{App, rt};
use asana_client::{Auth, Client};
use asana_fake::FakeAsana;
use asana_webhooks::MemoryHandshakeStore;
use asana_webhooks_actix::{Delivery, MemorySecrets, Receiver, resource};
use bytes::Bytes;
use tokio::sync::{mpsc, oneshot};

type Call = (http::Request<Bytes>, oneshot::Sender<http::Response<Bytes>>);

#[actix_web::test]
async fn a_task_moved_in_the_fake_reaches_the_actix_receivers_sink() {
    let fake = FakeAsana::new();
    let acme = fake.workspace("Acme");
    let project = acme.project("Onboarding");
    let ada = acme.user("Ada Lovelace", "ada@example.com");
    let [todo, done] = project.sections(["To do", "Done"]);
    let task = project.task("Welcome Ada").in_section(&todo).gid();

    let (queue, mut work) = mpsc::channel::<Delivery>(16);
    let receiver = Receiver::new(MemoryHandshakeStore::new(), MemorySecrets::new(), queue);
    let app = test::init_service(App::new().service(resource(&receiver, "/webhooks/asana"))).await;

    // actix's test service is not `Send`, and the fake's receiver must be: the fake's calls are
    // forwarded over a channel to a task on this thread, which calls the service.
    let (calls, mut incoming) = mpsc::unbounded_channel::<Call>();
    rt::spawn(async move {
        while let Some((request, answer)) = incoming.recv().await {
            let mut call = TestRequest::post().uri(request.uri().path());
            for (name, value) in request.headers() {
                call = call.insert_header((name.as_str(), value.as_bytes()));
            }
            let response =
                test::call_service(&app, call.set_payload(request.into_body()).to_request()).await;
            let mut reply = http::Response::builder().status(response.status().as_u16());
            if let Some(echo) = response.headers().get("x-hook-secret") {
                reply = reply.header("x-hook-secret", echo.as_bytes());
            }
            let _ = answer.send(reply.body(test::read_body(response).await).unwrap());
        }
    });
    fake.receiver(move |request| {
        let calls = calls.clone();
        async move {
            let (answer, reply) = oneshot::channel();
            calls.send((request, answer)).unwrap();
            reply.await.unwrap()
        }
    });

    let client = Client::builder()
        .auth(Auth::personal_access_token("fake"))
        .transport(fake.transport())
        .build()
        .unwrap();
    let established = client
        .webhooks()
        .establish(project.gid(), "https://hooks.example.com/webhooks/asana")
        .store(receiver.handshakes())
        .send()
        .await
        .unwrap();
    receiver
        .secrets()
        .insert(&established.route, established.secret.clone());
    let webhook = established.webhook.gid.clone().unwrap();

    let heartbeat = fake.heartbeat(&webhook).await.unwrap();
    assert_eq!(heartbeat.status.as_u16(), 200);
    assert!(work.try_recv().is_err());

    fake.as_user(&ada).move_task(&task, &done);
    let delivered = fake.deliver().await;
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0].status.as_u16(), 200);
    let delivery = work.try_recv().unwrap();
    assert_eq!(delivery.route, established.route);
    assert!(
        delivery
            .batch
            .events
            .iter()
            .any(|event| event.section_entered() == Some((task.clone(), done.clone())))
    );

    // Retiring the route answers 410, and the fake deletes the webhook as Asana would.
    receiver.secrets().retire(&established.route);
    fake.as_user(&ada).move_task(&task, &todo);
    let delivered = fake.deliver().await;
    assert_eq!(delivered[0].status.as_u16(), 410);
    assert!(fake.webhook_secret(&webhook).is_none());
}
