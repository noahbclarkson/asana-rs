//! A tour of the common calls: read a task with chosen fields, create a task in a section, move
//! it to another section, comment on it and set a custom field.
//!
//! ```text
//! ASANA_ACCESS_TOKEN=...       a personal access token
//! ASANA_TASK_GID=...           any task the token can read
//! ASANA_PROJECT_GID=...        a project with at least two sections
//! ASANA_SECTION_GID=...        the section the new task starts in
//! ASANA_NEXT_SECTION_GID=...   the section it is moved to
//! ASANA_NUMBER_FIELD_GID=...   a number custom field on that project
//! cargo run -p asana-client --example quickstart
//! ```
//!
//! It writes to the project: it leaves one task called "Quick start" behind.

use std::env;

use asana_client::openapi::ids::{CustomFieldGid, ProjectGid, SectionGid, TaskGid};
use asana_client::{Auth, Client, CustomFieldValue, Html, Require, fields};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // A GID is parsed once, into a type of its own: a section's GID cannot go where a task's is
    // wanted, and text that is not a GID never reaches a request path.
    let task: TaskGid = env::var("ASANA_TASK_GID")?.parse()?;
    let project: ProjectGid = env::var("ASANA_PROJECT_GID")?.parse()?;
    let section: SectionGid = env::var("ASANA_SECTION_GID")?.parse()?;
    let next_section: SectionGid = env::var("ASANA_NEXT_SECTION_GID")?.parse()?;
    let amount: CustomFieldGid = env::var("ASANA_NUMBER_FIELD_GID")?.parse()?;

    // One client per token: it holds the token's rate limits. Cloning it is cheap.
    let client = Client::builder()
        .auth(Auth::personal_access_token(env::var("ASANA_ACCESS_TOKEN")?))
        .user_agent("acme-quickstart/0.1")
        .build()?;

    // Read a task with the fields you need. A misspelt field does not compile.
    let read = client
        .tasks()
        .get(task)
        .fields(fields![Task: name, completed, due_on, memberships.section.name])
        .send()
        .await?;
    // Every field of a model is an `Option`; `require` turns a missing one into an error.
    println!("read {:?}", read.name.require("name")?);

    // Create a task straight into a section.
    let created = client
        .tasks()
        .create()
        .section(project, section)
        .name("Quick start")
        .notes("Made by the asana-rs quick start.")
        .send()
        .await?;
    let created = created.gid.require("gid")?;

    // Move it to another section of the same project.
    client
        .sections()
        .add_task(next_section, created.clone())
        .send()
        .await?;

    // Comment on it, in rich text. `Html` escapes what it is given.
    client
        .stories()
        .comment(
            created.clone(),
            Html::paragraph("Moved by ").bold("asana-rs"),
        )
        .send()
        .await?;

    // Set a custom field, typed by its kind: here a number, a decimal sent as a JSON number.
    client
        .tasks()
        .update(created.clone())
        .custom_field(amount, CustomFieldValue::number(1250))
        .send()
        .await?;

    println!("made task {created}");
    Ok(())
}
