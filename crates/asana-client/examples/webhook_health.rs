//! Lists the webhooks this token made in a workspace and reports how each is doing: the check to
//! run on a schedule, because Asana deletes a webhook after 24 hours of failed deliveries.
//!
//! ```text
//! ASANA_ACCESS_TOKEN=...    the token that made the webhooks
//! ASANA_WORKSPACE_GID=...   their workspace
//! ASANA_PROJECT_GID=...     optional: only the webhooks on this project
//! cargo run -p asana-client --example webhook_health
//! ```

use std::env;

use asana_client::openapi::ids::{ProjectGid, WorkspaceGid};
use asana_client::{Auth, Client};
use futures_util::TryStreamExt;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let workspace: WorkspaceGid = env::var("ASANA_WORKSPACE_GID")?.parse()?;
    let client = Client::builder()
        .auth(Auth::personal_access_token(env::var("ASANA_ACCESS_TOKEN")?))
        .build()?;

    let mut listing = client.webhooks().list(workspace);
    if let Ok(project) = env::var("ASANA_PROJECT_GID") {
        listing = listing.resource(project.parse::<ProjectGid>()?);
    }
    let webhooks: Vec<_> = listing.stream().try_collect().await?;

    for webhook in webhooks {
        let Some(gid) = webhook.gid else { continue };
        // One `GET /webhooks/{gid}` with the delivery fields. A webhook Asana has already
        // deleted is `Error::NotFound`: establish it again.
        let health = client.webhooks().health(&gid).await?;
        match health.will_be_deleted_at() {
            Some(deadline) => println!("{gid}: failing, deleted at {deadline} unless it recovers"),
            None if health.is_failing() => println!("{gid}: a delivery failed"),
            None => println!("{gid}: healthy, last success {:?}", health.last_success_at),
        }
        // What the receiver last answered, as Asana recorded it: untrusted text, and kept out of
        // `Debug` and `Display`.
        if let Some(answer) = health.last_failure_content() {
            println!(
                "  last failure: {}",
                answer.chars().take(200).collect::<String>()
            );
        }
    }
    Ok(())
}
