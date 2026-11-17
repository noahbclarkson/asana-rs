//! Reads lists four ways: every item as a stream, a page at a time, the first few, and one page
//! at a time with the offset kept between runs.
//!
//! ```text
//! ASANA_ACCESS_TOKEN=...   a personal access token
//! ASANA_PROJECT_GID=...    a project with some tasks
//! cargo run -p asana-client --example paging
//! ```

use std::env;

use asana_client::openapi::endpoint::{Offset, PageLimit};
use asana_client::openapi::endpoints::GetTasksForProject;
use asana_client::openapi::ids::ProjectGid;
use asana_client::{Auth, Client, fields};
use futures_util::{StreamExt, TryStreamExt};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let project: ProjectGid = env::var("ASANA_PROJECT_GID")?.parse()?;
    let client = Client::builder()
        .auth(Auth::personal_access_token(env::var("ASANA_ACCESS_TOKEN")?))
        .build()?;

    // Every task, fetched 100 at a time as the stream is read. The stream ends after its first
    // error, so `?` on each item is enough.
    let mut tasks = client
        .projects()
        .tasks(project.clone())
        .fields(fields![Task: name, completed])
        .stream();
    let mut open = 0;
    while let Some(task) = tasks.next().await {
        if task?.completed == Some(false) {
            open += 1;
        }
    }
    println!("{open} open tasks");

    // A page at a time, when the work is done per page.
    let mut pages = client.sections().list(project.clone()).pages();
    while let Some(page) = pages.try_next().await? {
        println!(
            "a page of {} sections (last: {})",
            page.items.len(),
            page.is_last()
        );
    }

    // The first few, fetching no more pages than that needs.
    let first_five = client
        .tasks()
        .list()
        .project(project.clone())
        .collect_max(5)
        .await?;
    println!("first five: {}", first_five.len());

    // One page now and the next one later: keep the offset (it is opaque text, and it expires).
    let saved: Option<Offset> = env::var("ASANA_OFFSET").ok().map(Offset::new);
    let page = client
        .send_page(
            GetTasksForProject::builder(project)
                .limit(PageLimit::new(20))
                .maybe_offset(saved)
                .build(),
        )
        .await?;
    match page.next {
        Some(next) => println!(
            "{} tasks; ASANA_OFFSET={} for the next page",
            page.items.len(),
            next.as_str()
        ),
        None => println!("{} tasks; that was the last page", page.items.len()),
    }
    Ok(())
}
