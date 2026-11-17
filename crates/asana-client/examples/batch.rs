//! Sends several operations in one request with the batch API, and reads each result by the
//! ticket its push returned.
//!
//! ```text
//! ASANA_ACCESS_TOKEN=...   a personal access token
//! ASANA_TASK_GIDS=...      one to eight task GIDs, separated by commas
//! ASANA_TAG_GID=...        a tag to add to each of them
//! cargo run -p asana-client --example batch
//! ```

use std::env;

use asana_client::openapi::endpoints::GetTask;
use asana_client::openapi::ids::{TagGid, TaskGid};
use asana_client::{Auth, Client, fields};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let tasks = env::var("ASANA_TASK_GIDS")?
        .split(',')
        .map(|gid| gid.trim().parse())
        .collect::<Result<Vec<TaskGid>, _>>()?;
    let tag: TagGid = env::var("ASANA_TAG_GID")?.parse()?;
    let client = Client::builder()
        .auth(Auth::personal_access_token(env::var("ASANA_ACCESS_TOKEN")?))
        .build()?;

    let mut batch = client.batch();
    // Any descriptor can go in: one taken from a handle's request with `into_endpoint`, or a
    // generated one built directly.
    let me = batch.push(client.users().me().into_endpoint())?;
    let mut tagged = Vec::new();
    for task in &tasks {
        let request = client.tasks().add_tag(task.clone(), tag.clone());
        // The eleventh push is refused with `Error::BatchFull`, before anything is sent.
        tagged.push((task, batch.push(request.into_endpoint())?));
    }
    let first = tasks.first().ok_or("name at least one task")?;
    let read_back = batch.push(
        GetTask::builder(first.clone())
            .opt_fields(fields![Task: name, tags.name])
            .build(),
    )?;

    // One request. A 429 is waited out and retried like any call; one action failing does not
    // fail the others, and each result is read with its own ticket.
    let mut results = batch.send().await?;
    println!("as {:?}", results.take(me)?.name);
    for (task, ticket) in tagged {
        match results.take(ticket) {
            Ok(_) => println!("tagged {task}"),
            Err(error) => println!("{task}: {error} (status {:?})", error.status()),
        }
    }
    let first = results.take(read_back)?;
    println!(
        "{:?} now has {} tags",
        first.name,
        first.tags.unwrap_or_default().len()
    );
    Ok(())
}
