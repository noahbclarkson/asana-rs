//! A complete Asana webhook receiver on axum, written to be read.
//!
//! What it does, in order:
//!
//! 1. Builds the receiver's three collaborators: a handshake store, a secret lookup and a channel
//!    the handler queues deliveries on.
//! 2. Serves `POST /webhooks/asana/{route_key}`.
//! 3. Establishes a webhook on one Asana resource (a project, say) whose target is
//!    `{ASANA_WEBHOOK_BASE_URL}/{route_key}`. Asana calls the receiver with the handshake while
//!    that request is open, so the server must already be listening.
//! 4. Runs a worker that takes deliveries off the channel, drops the integration's own writes,
//!    stale events and repeats, groups the rest by task, and re-reads each task before acting.
//!
//! Run it with a public `https` URL that forwards to port 3000 (a tunnel, in development):
//!
//! ```text
//! ASANA_ACCESS_TOKEN=... \
//! ASANA_RESOURCE_GID=1201234567890456 \
//! ASANA_WEBHOOK_BASE_URL=https://hooks.example.com/webhooks/asana \
//! cargo run -p asana-webhooks-axum --example receiver
//! ```
//!
//! Everything here is in memory, so it is right for one process only. Across several instances,
//! the handshake store and the secrets live in a database every instance reads, and the queue is
//! a durable one.

use std::error::Error;
use std::time::Duration;

use asana_client::fields;
use asana_client::openapi::ids::{TaskGid, UserGid};
use asana_client::webhooks::{Coalesced, Resolver, confirm_in_section, reread};
use asana_client::{Auth, Client};
use asana_webhooks::{EventAction, Filter, MemoryDedupe, MemoryHandshakeStore, ResourceGid};
use asana_webhooks_axum::{Delivery, MemorySecrets, Receiver, router};
use time::OffsetDateTime;
use tokio::sync::mpsc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let token = std::env::var("ASANA_ACCESS_TOKEN")?;
    let resource: ResourceGid = ResourceGid::new("project", &std::env::var("ASANA_RESOURCE_GID")?)?;
    let base_url = std::env::var("ASANA_WEBHOOK_BASE_URL")?;

    let client = Client::builder()
        .auth(Auth::personal_access_token(token))
        .build()?;

    // 1. The receiver. The handshake store is where Asana's handshake meets the code creating the
    //    webhook. The secrets are looked up by route for every delivery; a production lookup is a
    //    table keyed by `RouteKey::digest`, secrets encrypted at rest, and an unknown route is
    //    answered 404, never 410 (which would make Asana delete the webhook). The channel is the
    //    sink: the handler only queues, so it answers Asana in milliseconds whatever the worker is
    //    doing, and a full queue answers 503 so Asana retries.
    let (queue, deliveries) = mpsc::channel::<Delivery>(1024);
    let receiver = Receiver::new(MemoryHandshakeStore::new(), MemorySecrets::new(), queue);

    // 2. Serve before establishing: the handshake arrives during the create request.
    let app = router(&receiver, "/webhooks/asana");
    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });

    // 3. Establish. `establish` makes a fresh route key, opens it in the same handshake store the
    //    handler captures into, sends `POST /webhooks`, and checks the captured secret against the
    //    one Asana returns. Keep the GID, the secret and the route; Asana never sends the secret
    //    again.
    let established = client
        .webhooks()
        .establish(resource, base_url)
        .filter(Filter::task().action(EventAction::Added))
        .filter(Filter::task().action(EventAction::Changed))
        .store(receiver.handshakes())
        .send()
        .await?;
    receiver
        .secrets()
        .insert(&established.route, established.secret.clone());
    println!("webhook established: {:?}", established.webhook.gid);

    // Events the integration causes itself come back as webhook events too; ignore them.
    let me: UserGid = client
        .users()
        .me()
        .send()
        .await?
        .gid
        .ok_or("Asana returned no GID for the token's user")?;

    // 4. The worker, apart from the request handler.
    let worker = tokio::spawn(work(client, me, deliveries));

    tokio::signal::ctrl_c().await?;
    // A real service deletes the webhook it no longer wants, or keeps it for the next start.
    server.abort();
    worker.abort();
    Ok(())
}

/// Takes deliveries off the queue and acts on them, one at a time.
async fn work(client: Client, me: UserGid, mut deliveries: mpsc::Receiver<Delivery>) {
    // Remembers event keys for a day, the longest Asana retries a delivery.
    let dedupe = MemoryDedupe::new();
    let resolver = Resolver::builder()
        .ignore_user(me)
        .max_age(Duration::from_secs(60 * 60))
        .dedupe(&dedupe)
        .build();

    while let Some(delivery) = deliveries.recv().await {
        for unreadable in &delivery.batch.unreadable {
            eprintln!(
                "event {} could not be read: {}",
                unreadable.index, unreadable.reason
            );
        }
        let fresh = match resolver
            .accept(&delivery.batch, OffsetDateTime::now_utc())
            .await
        {
            Ok(fresh) => fresh,
            Err(error) => {
                eprintln!("the dedupe store failed: {error}");
                continue;
            }
        };
        // The newest event per task and section, per task and project, and per task's fields.
        for change in resolver.coalesce(fresh) {
            if let Err(error) = act(&client, change).await {
                eprintln!("acting on a change failed: {error}");
            }
        }
    }
}

/// Acts on one change, reading the task as it is now first: events are compact and can be
/// minutes old, and the task may have moved again since.
async fn act(client: &Client, change: Coalesced) -> Result<(), asana_client::Error> {
    match change {
        Coalesced::SectionEntered { task, section, .. } => {
            let now = reread(client, &task, fields![Task: name, memberships.section]).await?;
            if confirm_in_section(&now, &section) {
                println!("{} is in section {section}: act on it", name(&task, &now));
            }
        }
        Coalesced::TaskChanged { task, fields, .. } => {
            let now = reread(client, &task, fields![Task: name, completed]).await?;
            println!("{} changed: {}", name(&task, &now), fields.join(", "));
        }
        other => println!("not acted on: {other:?}"),
    }
    Ok(())
}

fn name(gid: &TaskGid, task: &asana_client::openapi::models::Task) -> String {
    task.name.clone().unwrap_or_else(|| gid.to_string())
}
