//! Comments in rich text, reads the latest comments, uploads and downloads a file, and makes a
//! task from a template, waiting for Asana's job to finish.
//!
//! ```text
//! ASANA_ACCESS_TOKEN=...      a personal access token
//! ASANA_TASK_GID=...          a task to comment on and attach to
//! ASANA_TASK_TEMPLATE_GID=... optional: a task template to instantiate
//! cargo run -p asana-client --example stories_and_files
//! ```

use std::env;
use std::time::Duration;

use asana_client::openapi::ids::{TaskGid, TaskTemplateGid};
use asana_client::resources::attachments::Upload;
use asana_client::resources::stories::StoryExt;
use asana_client::{Auth, Client, Html, Require, fields};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let task: TaskGid = env::var("ASANA_TASK_GID")?.parse()?;
    let client = Client::builder()
        .auth(Auth::personal_access_token(env::var("ASANA_ACCESS_TOKEN")?))
        .build()?;
    let me = client.users().me().send().await?.gid.require("gid")?;

    // Rich text: everything passed as text is escaped, links are kept to http(s) and mailto, and
    // a mention notifies the user without making them a follower.
    let comment = Html::paragraph("Figures for ")
        .bold("Acme Ltd")
        .text(" are ready, ")
        .mention(&me)
        .next_paragraph("Sources:")
        .list([
            Html::new().link("https://example.com/report", "the report"),
            Html::from("last month's notes"),
        ]);
    client
        .stories()
        .comment(task.clone(), comment)
        .send()
        .await?;

    // Asana lists stories oldest first; `latest` reads the newest without walking them all.
    for story in client
        .stories()
        .latest(task.clone(), 5)
        .comments_only()
        .fields(fields![Story: text, created_at, created_by.name])
        .send()
        .await?
    {
        if story.is_comment() {
            println!("{:?}: {:?}", story.created_at, story.text);
        }
    }

    // An upload is multipart; it is never retried after an unknown outcome.
    let attachment = client
        .attachments()
        .upload(
            task.clone(),
            Upload::bytes(
                "acme-figures.csv",
                "month,total\n2026-10,1250.50\n",
                "text/csv",
            ),
        )
        .send()
        .await?
        .gid
        .require("gid")?;
    // A download reads the short-lived `download_url` and fetches it without the token.
    let bytes = client.attachments().download(attachment).send().await?;
    println!("downloaded {} bytes", bytes.len());

    // Templates answer with a job; `wait_for_task` polls it until the task is fully made.
    if let Ok(template) = env::var("ASANA_TASK_TEMPLATE_GID") {
        let template: TaskTemplateGid = template.parse()?;
        let job = client
            .task_templates()
            .instantiate(template)
            .name("Month end: November")
            .send()
            .await?;
        // The GID is known at once; the task's subtasks and fields may still be arriving.
        println!("making {:?}", job.new_task_gid());
        let made = job.wait_for_task(&client, Duration::from_secs(60)).await?;
        println!("made {:?}", made.gid);
    }
    Ok(())
}
