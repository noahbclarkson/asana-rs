//! A complete Asana webhook receiver on actix-web, written to be read.
//!
//! It watches one project and reacts when a task is moved into a section. The parts, in the order
//! a request meets them:
//!
//! 1. **The stores.** The handshake store is where the code establishing a webhook and the
//!    receiver answering Asana's handshake meet; the secrets table is where each webhook's secret
//!    is kept for checking its deliveries. Both are in memory here, which is right only when one
//!    process both establishes and receives; across instances they are database tables.
//! 2. **The queue.** The receiver checks the signature, decodes the batch, puts it on a bounded
//!    channel and answers `200`. Asana allows ten seconds per delivery, so nothing slow happens in
//!    the request; a full channel answers `503` and Asana retries.
//! 3. **The worker.** It drains the channel, drops our own changes, stale events and repeats with
//!    the client's `Resolver`, folds the rest into what they mean, and re-reads each task before
//!    acting, because an event is compact and may be minutes old.
//! 4. **Start-up.** The server must be answering before the webhook is established: Asana calls the
//!    receiver with the handshake while the create request is still open.
//!
//! Run it with a public `https` address that reaches this process (Asana refuses `localhost`
//! targets, so use a tunnel in development):
//!
//! ```text
//! ASANA_ACCESS_TOKEN=...                   # a personal access token
//! ASANA_PROJECT_GID=1201234567890456       # the project to watch
//! PUBLIC_URL=https://hooks.example.com     # where Asana can reach BIND_ADDRESS
//! BIND_ADDRESS=127.0.0.1:8080              # optional
//! cargo run -p asana-webhooks-actix --example receiver
//! ```

use std::time::Duration;

use actix_web::{App, HttpServer, rt};
use asana_client::openapi::fields;
use asana_client::openapi::ids::{ProjectGid, UserGid};
use asana_client::webhooks::{Coalesced, Resolver, confirm_in_section, reread};
use asana_client::{Auth, Client};
use asana_webhooks::{EventAction, Filter, MemoryDedupe, MemoryHandshakeStore};
use asana_webhooks_actix::{Delivery, MemorySecrets, Receiver, resource};
use time::OffsetDateTime;
use tokio::sync::mpsc;

/// The path the receiver is mounted at; the target given to Asana is `PUBLIC_URL` plus this, and
/// each webhook's route key is appended to it.
const PATH: &str = "/webhooks/asana";

/// How many verified batches may wait for the worker before the receiver answers `503`.
const QUEUE: usize = 1_024;

type Error = Box<dyn std::error::Error>;

#[actix_web::main]
async fn main() -> Result<(), Error> {
    let token = std::env::var("ASANA_ACCESS_TOKEN")?;
    let project: ProjectGid = std::env::var("ASANA_PROJECT_GID")?.parse()?;
    let public_url = std::env::var("PUBLIC_URL")?;
    let bind = std::env::var("BIND_ADDRESS").unwrap_or_else(|_| "127.0.0.1:8080".to_owned());

    let client = Client::builder()
        .auth(Auth::personal_access_token(token))
        .build()?;
    // Our own writes come back as events too; the worker ignores them so it cannot loop.
    let me = client
        .users()
        .me()
        .send()
        .await?
        .gid
        .ok_or("Asana returned no GID for the token's user")?;

    // 1 and 2. The stores and the queue, held by the receiver. The receiver is cheap to clone and
    //    every clone shares them: the server's workers get one each, and the code establishing the
    //    webhook below reaches the same stores through `handshakes()` and `secrets()`.
    let (queue, work) = mpsc::channel::<Delivery>(QUEUE);
    let receiver = Receiver::new(MemoryHandshakeStore::new(), MemorySecrets::new(), queue);

    // 3. The worker, on this thread's runtime: nothing in it needs to be `Send`.
    rt::spawn(worker(client.clone(), me, work));

    // 4. The server first: the handshake arrives while `establish` is waiting.
    //    `HttpServer::new` runs its factory once per worker thread; each `App` gets a clone of the
    //    same receiver, so every thread shares the stores and the queue.
    let served = receiver.clone();
    let server = HttpServer::new(move || App::new().service(resource(&served, PATH)))
        .bind(&bind)?
        .run();
    let running = rt::spawn(server);

    // Then the webhook. `establish` makes a fresh route key, opens it in the handshake store,
    // creates the webhook with the target `{PUBLIC_URL}{PATH}/{route_key}`, and checks that the
    // secret the receiver captured is the one Asana returned.
    let established = client
        .webhooks()
        .establish(
            project.clone(),
            format!("{}{PATH}", public_url.trim_end_matches('/')),
        )
        // A task entering a section is an `added` event on the task, with the section as parent.
        .filter(Filter::task().action(EventAction::Added))
        .store(receiver.handshakes())
        .send()
        .await?;
    // From now on deliveries on this route are checked with this secret. Asana's first heartbeat
    // can arrive a moment before this line; it is answered `404` and Asana simply retries it.
    // A real service keeps the GID, the route's digest and the secret (encrypted) in its database,
    // and checks `client.webhooks().health(..)` on a schedule.
    receiver
        .secrets()
        .insert(&established.route, established.secret.clone());
    let gid = established.webhook.gid.as_ref().map(ToString::to_string);
    println!(
        "watching project {project} through webhook {}",
        gid.unwrap_or_default()
    );

    running.await??;
    Ok(())
}

/// Turns verified batches into work, one at a time, outside Asana's ten seconds.
async fn worker(client: Client, me: UserGid, mut work: mpsc::Receiver<Delivery>) {
    // Asana retries a delivery it thinks failed, so the same event can come twice; the dedupe
    // store remembers what was seen for a day. Across instances, a shared store instead.
    let dedupe = MemoryDedupe::new();
    let resolver = Resolver::builder()
        .ignore_user(me)
        .max_age(Duration::from_secs(60 * 60))
        .dedupe(&dedupe)
        .build();

    while let Some(Delivery { route, batch, .. }) = work.recv().await {
        if !batch.unreadable.is_empty() {
            eprintln!("{} events could not be read", batch.unreadable.len());
        }
        let fresh = match resolver.accept(&batch, OffsetDateTime::now_utc()).await {
            Ok(fresh) => fresh,
            Err(error) => {
                eprintln!("dedupe store failed ({error}); processing the batch anyway");
                batch.events
            }
        };
        // One entry per task, the newest event winning: a drag through three sections is one
        // `SectionEntered` for where it ended up.
        for change in resolver.coalesce(fresh) {
            let Coalesced::SectionEntered { task, section, .. } = change else {
                continue;
            };
            // Re-read before acting: the task may have moved again since the event.
            match reread(&client, &task, fields![Task: name, memberships.section]).await {
                Ok(now) if confirm_in_section(&now, &section) => {
                    let name = now.name.as_deref().unwrap_or("(unnamed)");
                    println!("task {task} ({name}) entered section {section} via {route:?}");
                }
                Ok(_) => println!("task {task} has left section {section} again; nothing to do"),
                Err(error) => eprintln!("could not re-read task {task}: {error}"),
            }
        }
    }
}
