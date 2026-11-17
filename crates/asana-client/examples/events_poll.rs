//! Polls a project's events stream, saving the sync token after each event so a restart carries
//! on where it stopped. This is the catch-up path beside a webhook, or a stand-in for one.
//!
//! ```text
//! ASANA_ACCESS_TOKEN=...   a personal access token
//! ASANA_PROJECT_GID=...    the project to watch
//! ASANA_SYNC_FILE=...      where to keep the sync token; default ./asana-sync-token
//! cargo run -p asana-client --example events_poll
//! ```

use std::env;
use std::time::Duration;

use asana_client::openapi::ids::ProjectGid;
use asana_client::resources::events::EventsItem;
use asana_client::{Auth, Client, Error, SyncToken};
use futures_util::StreamExt;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let project: ProjectGid = env::var("ASANA_PROJECT_GID")?.parse()?;
    let file = env::var("ASANA_SYNC_FILE").unwrap_or_else(|_| "asana-sync-token".to_owned());
    let client = Client::builder()
        .auth(Auth::personal_access_token(env::var("ASANA_ACCESS_TOKEN")?))
        .build()?;

    loop {
        // Without a saved token the first item is `CaughtUp` with a fresh one: events start
        // after it, and nothing before it is replayed.
        let saved = std::fs::read_to_string(&file)
            .ok()
            .map(|text| SyncToken::new(text.trim()));
        let mut items = client
            .events()
            .poll(&project, saved, Duration::from_secs(10));
        while let Some(item) = items.next().await {
            match item {
                Ok(EventsItem::Event { event, sync }) => {
                    // The same `Event` type a webhook delivers, with the same helpers.
                    if let Some((task, section)) = event.section_entered() {
                        println!("task {task} entered section {section}");
                    } else {
                        println!("{:?} on {:?}", event.action, event.resource.gid);
                    }
                    // Save after handling: a restart may repeat an event, never skip one.
                    std::fs::write(&file, sync.as_str())?;
                }
                Ok(other) => std::fs::write(&file, other.sync().as_str())?,
                Err(Error::SyncTokenExpired { sync }) => {
                    // Events were lost: read the project again in full, then go on from here.
                    println!("the sync token expired; re-read the project");
                    std::fs::write(&file, sync.as_str())?;
                }
                // Rate limits and server failures were already retried by the client.
                Err(other) => return Err(other.into()),
            }
        }
    }
}
