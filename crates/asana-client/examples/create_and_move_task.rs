//! Creates a task, finds sections by name, moves the task between them, places it at the top,
//! comments, checks where it ended up, and deletes it again.
//!
//! ```text
//! ASANA_ACCESS_TOKEN=...   a personal access token
//! ASANA_PROJECT_GID=...    a project with sections named "Planned" and "In progress"
//! cargo run -p asana-client --example create_and_move_task
//! ```

use std::env;

use asana_client::openapi::ids::{ProjectGid, SectionGid, UserRef};
use asana_client::resources::tasks::TaskExt;
use asana_client::{Auth, Client, Html, NameMatch, Require, fields};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let project: ProjectGid = env::var("ASANA_PROJECT_GID")?.parse()?;
    let client = Client::builder()
        .auth(Auth::personal_access_token(env::var("ASANA_ACCESS_TOKEN")?))
        .build()?;

    // Sections by name. Names are not unique in Asana, so the answer says how many matched.
    let planned = section_named(&client, &project, "Planned").await?;
    let in_progress = section_named(&client, &project, "In progress").await?;

    // Create the task in "Planned", followed by the token's own user.
    let task = client
        .tasks()
        .create()
        .section(project.clone(), planned)
        .name("Write the release notes")
        .follower(UserRef::Me)
        .send()
        .await?
        .gid
        .require("gid")?;

    // Moving between sections of one project: add the task to the section.
    client
        .sections()
        .add_task(in_progress.clone(), task.clone())
        .send()
        .await?;

    // Adding a task to a project it is already in moves it; this puts it first in the section.
    client
        .tasks()
        .add_project(task.clone(), project.clone())
        .section(in_progress.clone())
        .at_start()
        .send()
        .await?;

    client
        .stories()
        .comment(
            task.clone(),
            Html::paragraph("Started. Checklist:").list(["Draft", "Review"]),
        )
        .send()
        .await?;

    // Webhook events and moves can race; read the task back before trusting where it is.
    let now = client
        .tasks()
        .get(task.clone())
        .fields(fields![Task: name, permalink_url, memberships.project, memberships.section.name])
        .send()
        .await?;
    println!(
        "in {:?}: {}",
        now.section_in(&project)
            .and_then(|section| section.name.as_deref()),
        now.permalink().unwrap_or("(no link)")
    );
    assert!(now.is_in_section(&in_progress));

    client.tasks().delete(task).send().await?;
    Ok(())
}

/// The one section of `project` named `name`, or an error when there is none or several.
async fn section_named(
    client: &Client,
    project: &ProjectGid,
    name: &str,
) -> Result<SectionGid, Box<dyn std::error::Error>> {
    match client
        .sections()
        .find_by_name(project.clone(), name)
        .send()
        .await?
    {
        NameMatch::One(section) => Ok(section.gid.require("gid")?),
        NameMatch::Several(sections) => {
            Err(format!("{} sections are named {name:?}", sections.len()).into())
        }
        // `NameMatch::Missing`, and any case a later version adds.
        _ => Err(format!("no section named {name:?}").into()),
    }
}
