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

#[tokio::test(start_paused = true)]
async fn a_create_whose_answer_is_lost_after_applying_has_still_made_the_task() {
    let world = World::new();
    let client = client(&world.fake);
    world
        .fake
        .fail_next::<CreateTask>(Fault::timeout_after_applying());

    let error = client
        .tasks()
        .create()
        .workspace(world.workspace.clone())
        .name("Made anyway")
        .send()
        .await
        .unwrap_err();

    // The client sees what it would see live, and does not send the create again...
    assert!(matches!(error, Error::Transport { .. }), "{error:?}");
    assert_eq!(error.outcome(), asana_client::Outcome::Unknown);
    assert!(!error.is_retryable());
    assert_eq!(world.fake.requests_for::<CreateTask>().len(), 1);
    // ...and the task exists, as it may in Asana: read before writing again.
    let found = client
        .workspaces()
        .search_tasks(world.workspace)
        .text("Made anyway")
        .send()
        .await
        .unwrap();
    assert_eq!(found.len(), 1);
}

/// Sends `request` straight to the fake and returns its status and error message.
async fn raw(fake: &FakeAsana, method: &str, path_and_query: &str) -> (StatusCode, String) {
    use asana_client::Transport as _;

    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://app.asana.com/api/1.0{path_and_query}"))
        .header("authorization", "Bearer 0/made-up")
        .body(bytes::Bytes::new())
        .unwrap();
    let response = fake.transport().send(request).await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(response.body()).unwrap();
    let message = body["errors"][0]["message"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    (response.status(), message)
}

#[tokio::test]
async fn a_search_filter_the_fake_does_not_model_is_refused_by_name() {
    let world = World::new();
    let client = client(&world.fake);

    // Ignoring it would find the task where Asana, applying the filter, would not.
    let error = client
        .workspaces()
        .search_tasks(world.workspace.clone())
        .text("welcome")
        .param("followers.any", world.ada.to_string())
        .send()
        .await
        .unwrap_err();
    assert_eq!(
        error.status(),
        Some(StatusCode::NOT_IMPLEMENTED),
        "{error:?}"
    );
    let (status, message) = raw(
        &world.fake,
        "GET",
        &format!(
            "/workspaces/{}/tasks/search?completed_at.after=2026-01-01T00:00:00Z",
            world.workspace
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
    assert!(message.contains("completed_at.after"), "{message}");
    let (status, message) = raw(
        &world.fake,
        "GET",
        &format!("/workspaces/{}/tasks/search?sort_by=likes", world.workspace),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
    assert!(message.contains("likes"), "{message}");

    // An operator Asana does not have is Asana's own refusal.
    let error = client
        .workspaces()
        .search_tasks(world.workspace)
        .param("custom_fields.1200000000000009.bogus", "1")
        .send()
        .await
        .unwrap_err();
    assert!(matches!(error, Error::BadRequest(_)), "{error:?}");
}

#[tokio::test]
async fn an_opt_fields_path_the_specification_does_not_list_is_refused() {
    use asana_client::openapi::fields::{Fields, TaskField};

    let world = World::new();
    let client = client(&world.fake);
    let error = client
        .tasks()
        .get(world.task.clone())
        .fields(Fields::<TaskField>::none().with_raw("nmae"))
        .send()
        .await
        .unwrap_err();
    assert!(matches!(error, Error::BadRequest(_)), "{error:?}");
    // A listed path, nested ones included, is served.
    client
        .tasks()
        .get(world.task.clone())
        .fields(asana_client::fields![Task: name, memberships.section.name])
        .send()
        .await
        .unwrap();

    // The same rule inside a batch, action by action.
    let mut batch = client.batch();
    let bad = batch
        .push(
            GetTask::builder(world.task.clone())
                .opt_fields(Fields::none().with_raw("nmae"))
                .build(),
        )
        .unwrap();
    let good = batch.push(GetTask::builder(world.task).build()).unwrap();
    let mut results = batch.send().await.unwrap();
    assert!(matches!(results.take(bad), Err(Error::BadRequest(_))));
    results.take(good).unwrap();
}

#[tokio::test]
async fn an_offset_from_one_list_is_refused_by_another() {
    use asana_client::openapi::endpoint::PageLimit;
    use asana_client::openapi::endpoints::{GetTasksForProject, GetTasksForSection};

    let world = World::new();
    let client = client(&world.fake);
    // A second task, so the project's list has a next page.
    let _ = client
        .tasks()
        .create()
        .workspace(world.workspace.clone())
        .name("Second")
        .section(world.project.clone(), world.planned.clone())
        .send()
        .await
        .unwrap();
    let first = client
        .send_page(
            GetTasksForProject::builder(world.project.clone())
                .limit(PageLimit::new(1))
                .build(),
        )
        .await
        .unwrap();
    let offset = first.next.unwrap();

    // Good for the list that issued it...
    client
        .send_page(
            GetTasksForProject::builder(world.project)
                .limit(PageLimit::new(1))
                .offset(offset.clone())
                .build(),
        )
        .await
        .unwrap();
    // ...refused by another.
    let error = client
        .send_page(
            GetTasksForSection::builder(world.planned)
                .limit(PageLimit::new(1))
                .offset(offset)
                .build(),
        )
        .await
        .unwrap_err();
    assert!(matches!(error, Error::BadRequest(_)), "{error:?}");
}

#[tokio::test]
async fn a_path_no_operation_has_is_404() {
    let world = World::new();
    let (status, message) = raw(&world.fake, "GET", "/no/such/route").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(message, "No matching route for request");
}
