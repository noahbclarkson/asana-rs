//! Searches a workspace's tasks, reading past the 100-result cap by paging on creation time.
//!
//! ```text
//! ASANA_ACCESS_TOKEN=...      a personal access token for a user on a paid plan
//! ASANA_WORKSPACE_GID=...     the workspace to search
//! ASANA_SEARCH_TEXT=...       the text to look for; default "invoice"
//! ASANA_NUMBER_FIELD_GID=...  optional: only tasks whose number field is over 1000
//! cargo run -p asana-client --example search
//! ```
//!
//! Search is eventually consistent (a change appears 10 to 60 seconds later), returns at most
//! 100 tasks a call with no offset, and needs a paid plan.

use std::collections::HashSet;
use std::env;

use asana_client::openapi::endpoints::SearchTasksForWorkspaceSortBy;
use asana_client::openapi::ids::{CustomFieldGid, TaskGid, WorkspaceGid};
use asana_client::{Auth, Client, CustomFieldFilter, Error, fields};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let workspace: WorkspaceGid = env::var("ASANA_WORKSPACE_GID")?.parse()?;
    let text = env::var("ASANA_SEARCH_TEXT").unwrap_or_else(|_| "invoice".to_owned());
    let over: Option<CustomFieldGid> = match env::var("ASANA_NUMBER_FIELD_GID") {
        Ok(gid) => Some(gid.parse()?),
        Err(_) => None,
    };
    let client = Client::builder()
        .auth(Auth::personal_access_token(env::var("ASANA_ACCESS_TOKEN")?))
        .build()?;

    let mut seen: HashSet<TaskGid> = HashSet::new();
    let mut after = None;
    loop {
        // Oldest first, with `created_at`, so the next call can start after the last one seen.
        let mut search = client
            .workspaces()
            .search_tasks(workspace.clone())
            .text(text.clone())
            .completed(false)
            .sort_by(SearchTasksForWorkspaceSortBy::CreatedAt)
            .sort_ascending(true)
            .limit(100)
            .fields(fields![Task: name, created_at]);
        if let Some(field) = &over {
            search = search.custom_field(field, CustomFieldFilter::GreaterThan, "1000");
        }
        if let Some(moment) = after {
            search = search.created_at_after(moment);
        }

        let tasks = match search.send().await {
            Ok(tasks) => tasks,
            Err(Error::PremiumRequired(_)) => {
                println!("search needs a paid Asana plan in this workspace");
                return Ok(());
            }
            Err(other) => return Err(other.into()),
        };
        let full = tasks.len() == 100;
        let mut new = 0;
        for task in tasks {
            after = task.created_at.or(after);
            // Asana does not say whether the bound is inclusive: drop what was already seen.
            if let Some(gid) = task.gid
                && seen.insert(gid.clone())
            {
                println!("{gid} {}", task.name.as_deref().unwrap_or(""));
                new += 1;
            }
        }
        // A short page is the last; a page of nothing new means the bound stopped moving.
        if !full || new == 0 {
            break;
        }
    }
    println!("{} tasks", seen.len());
    Ok(())
}
