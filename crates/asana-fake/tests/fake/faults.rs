//! Queued faults, tokens and the premium-only search.

use std::time::Duration;

use asana_client::openapi::endpoints::{CreateTask, GetTask, SearchTasksForWorkspace};
use asana_client::{Auth, Client, Error};
use asana_fake::{FakeAsana, Fault};
use http::StatusCode;

use crate::{World, client};

#[tokio::test(start_paused = true)]
async fn a_queued_429_is_waited_out_and_retried_by_the_client() {
    let world = World::new();
    let client = client(&world.fake);
    world
        .fake
        .fail_next::<GetTask>(Fault::rate_limited(Duration::from_secs(2)));

    let started = tokio::time::Instant::now();
    let task = client.tasks().get(world.task.clone()).send().await.unwrap();

    assert_eq!(task.gid, Some(world.task));
    assert_eq!(world.fake.requests_for::<GetTask>().len(), 2);
    assert!(started.elapsed() >= Duration::from_secs(2));
}

#[tokio::test(start_paused = true)]
async fn a_timed_out_create_is_not_repeated_and_changes_nothing() {
    let world = World::new();
    let client = client(&world.fake);
    world.fake.fail_next::<CreateTask>(Fault::timeout());

    let error = client
        .tasks()
        .create()
        .workspace(world.workspace.clone())
        .name("Never made")
        .send()
        .await
        .unwrap_err();

    assert!(matches!(error, Error::Transport { .. }));
    assert_eq!(world.fake.requests_for::<CreateTask>().len(), 1);
    let search = client
        .workspaces()
        .search_tasks(world.workspace)
        .text("Never made")
        .send()
        .await
        .unwrap();
    assert!(search.is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_queued_server_error_on_a_read_is_retried() {
    let world = World::new();
    let client = client(&world.fake);
    world.fake.fail_next_request(Fault::server_error());

    client.tasks().get(world.task.clone()).send().await.unwrap();
    assert_eq!(world.fake.requests().len(), 2);
}

#[tokio::test]
async fn search_finds_by_text_and_section_and_is_premium_only() {
    let world = World::new();
    let client = client(&world.fake);

    let found = client
        .workspaces()
        .search_tasks(world.workspace.clone())
        .text("welcome")
        .sections_any([world.planned.clone()])
        .send()
        .await
        .unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].gid, Some(world.task.clone()));

    let free = FakeAsana::new();
    let workspace = free.workspace("Acme").free().gid();
    let error = crate::client(&free)
        .workspaces()
        .search_tasks(workspace)
        .send()
        .await
        .unwrap_err();
    assert!(matches!(error, Error::PremiumRequired(_)));
    assert_eq!(free.requests_for::<SearchTasksForWorkspace>().len(), 1);
}

#[tokio::test]
async fn an_unknown_token_is_refused_once_tokens_are_registered() {
    let world = World::new();
    world.fake.accept_token("0/known", &world.ada);

    let error = client(&world.fake)
        .tasks()
        .get(world.task.clone())
        .send()
        .await
        .unwrap_err();
    assert_eq!(error.status(), Some(StatusCode::UNAUTHORIZED));

    let known = Client::builder()
        .auth(Auth::personal_access_token("0/known"))
        .transport(world.fake.transport())
        .build()
        .unwrap();
    known.tasks().get(world.task).send().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_slow_answer_arrives_after_its_delay() {
    let world = World::new();
    let client = client(&world.fake);
    world
        .fake
        .fail_next::<GetTask>(Fault::slow(Duration::from_secs(5)));

    let started = tokio::time::Instant::now();
    client.tasks().get(world.task.clone()).send().await.unwrap();
    assert!(started.elapsed() >= Duration::from_secs(5));
    assert_eq!(world.fake.requests_for::<GetTask>().len(), 1);
}
