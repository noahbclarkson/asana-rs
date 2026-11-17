//! Watches one section by listing it every few seconds, for work that must start within seconds
//! of a task arriving rather than after Asana's webhook latency. A shared `SeenTasks` keeps the
//! watcher and a webhook resolver from both acting on the same arrival.
//!
//! ```text
//! ASANA_ACCESS_TOKEN=...   a personal access token
//! ASANA_SECTION_GID=...    the section to watch
//! cargo run -p asana-client --example section_watcher
//! ```

use std::env;
use std::time::Duration;

use asana_client::openapi::ids::SectionGid;
use asana_client::webhooks::{Resolver, SectionWatcher, SeenTasks};
use asana_client::{Auth, Client, fields};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let section: SectionGid = env::var("ASANA_SECTION_GID")?.parse()?;
    let client = Client::builder()
        .auth(Auth::personal_access_token(env::var("ASANA_ACCESS_TOKEN")?))
        .build()?;

    // Remember arrivals for longer than a webhook event can lag behind the watcher.
    let seen = SeenTasks::new(Duration::from_secs(60 * 60));
    // The webhook side: its resolver shares the set, so a `SectionEntered` the watcher already
    // reported is not reported again when the event arrives.
    let _resolver = Resolver::builder().seen(seen.clone()).build();

    let mut watcher = SectionWatcher::new(section, seen).fields(fields![Task: name]);
    let mut every = tokio::time::interval(Duration::from_secs(5));
    loop {
        every.tick().await;
        // One request a poll (12 a minute at this rate), counted against the token's limits.
        // The first poll records what is already there and reports nothing.
        match watcher.poll(&client).await {
            Ok(arrivals) => {
                for task in arrivals.tasks {
                    println!("arrived: {:?} {:?}", task.gid, task.name);
                }
                if !arrivals.complete {
                    println!("over 100 tasks in the section: later ones are the webhook's");
                }
            }
            // The watcher keeps its last good listing; the next poll compares against it.
            Err(error) => eprintln!("poll failed: {error}"),
        }
    }
}
