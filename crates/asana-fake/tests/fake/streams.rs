//! The events stream, jobs from templates, and the batch API.

use std::time::Duration;

use asana_client::Error;
use asana_client::openapi::endpoints::{GetEvents, GetJob, GetTask};
use asana_fake::FakeAsana;
use asana_openapi::ids::TaskGid;

use crate::{World, client};

#[tokio::test]
async fn the_events_stream_starts_with_412_then_returns_what_changed() {
    let world = World::new();
    let client = client(&world.fake);

    // The client turns the first call's 412 into an empty page carrying the token.
    let first = client.events().sync(&world.project, None).await.unwrap();
    assert!(first.events.is_empty());
    let calls = world.fake.requests_for::<GetEvents>();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].query("sync"), None);
    let sync = first.sync;

    world
        .fake
        .as_user(&world.ada)
        .move_task(&world.task, &world.sending);
    let page = client
        .events()
        .sync(&world.project, Some(sync))
        .await
        .unwrap();
    assert!(!page.has_more);
    // Removed from the old section, added to the new one, and the move's story.
    assert_eq!(page.events.len(), 3);
    assert_eq!(
        page.events[0].section_left(),
        Some((world.task.clone(), world.planned.clone()))
    );
    assert_eq!(
        page.events[1].section_entered(),
        Some((world.task.clone(), world.sending.clone()))
    );
    assert!(page.events[2].story_added().is_some());

    let quiet = client
        .events()
        .sync(&world.project, Some(page.sync.clone()))
        .await
        .unwrap();
    assert!(quiet.events.is_empty());

    world.fake.expire_sync_tokens();
    let expired = client
        .events()
        .sync(&world.project, Some(quiet.sync))
        .await
        .unwrap_err();
    assert!(matches!(expired, Error::SyncTokenExpired { .. }));
}

#[tokio::test(start_paused = true)]
async fn a_job_from_a_task_template_finishes_with_its_task() {
    let fake = FakeAsana::new();
    let project = fake.workspace("Acme").project("Onboarding");
    let template = project.template("Welcome", "Welcome pack", "Send the welcome pack");
    fake.set_job_polls(2);
    let client = client(&fake);

    let job = client
        .task_templates()
        .instantiate(template)
        .name("Welcome Ada")
        .send()
        .await
        .unwrap();
    let new_task: TaskGid = job.new_task_gid().unwrap().clone();
    let task = job
        .wait_for_task(&client, Duration::from_secs(30))
        .await
        .unwrap();

    assert_eq!(task.gid, Some(new_task.clone()));
    assert_eq!(task.name.as_deref(), Some("Welcome Ada"));
    // Two reads answer `in_progress`, the third `succeeded`.
    assert_eq!(fake.requests_for::<GetJob>().len(), 3);
    let record = fake.record(&new_task).unwrap();
    assert_eq!(record["notes"], "Send the welcome pack");
    assert_eq!(record["projects"][0]["gid"], project.gid().to_string());
}

#[tokio::test]
async fn a_batch_answers_each_action_as_if_sent_alone() {
    let world = World::new();
    let client = client(&world.fake);
    let missing: TaskGid = "1299999999999999".parse().unwrap();

    let mut batch = client.batch();
    let found = batch
        .push(GetTask::builder(world.task.clone()).build())
        .unwrap();
    let lost = batch.push(GetTask::builder(missing).build()).unwrap();
    let mut results = batch.send().await.unwrap();

    let task = results.take(found).unwrap();
    assert_eq!(task.name.as_deref(), Some("Welcome pack"));
    assert!(results.take(lost).unwrap_err().is_not_found());
    assert_eq!(world.fake.requests().len(), 1);
}
