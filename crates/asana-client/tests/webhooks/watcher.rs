//! The section watcher: one request per poll, only new tasks, and no double reports with the
//! resolver.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use asana_client::Transport as _;
use asana_client::openapi::fields;
use asana_client::openapi::ids::{SectionGid, TaskGid};
use asana_client::webhooks::{Coalesced, Resolver, SectionWatcher, SeenTasks};
use asana_webhooks::ManualClock;
use futures_util::FutureExt;

use crate::support::{Scripted, client, reply};
use crate::{ALICE, NOW, SECTION_A, TASK_A, TASK_B, TASK_C, event, query};

fn section() -> SectionGid {
    SECTION_A.parse().unwrap()
}

fn task(gid: &str) -> TaskGid {
    gid.parse().unwrap()
}

/// A transport listing the section as `listing` holds it at the time of each request.
fn listing(tasks: &Arc<Mutex<Vec<&'static str>>>) -> Arc<Scripted> {
    let tasks = tasks.clone();
    Scripted::new(move |_, _| {
        let data: Vec<_> = tasks
            .lock()
            .unwrap()
            .iter()
            .map(|gid| serde_json::json!({"gid": gid, "resource_type": "task", "name": "Acme"}))
            .collect();
        Ok(reply(
            200,
            &serde_json::json!({"data": data, "next_page": null}).to_string(),
        ))
    })
}

fn gids(arrivals: &asana_client::webhooks::Arrivals) -> Vec<String> {
    arrivals
        .tasks
        .iter()
        .map(|task| task.gid.as_ref().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn each_poll_is_one_request_and_returns_only_new_tasks() {
    let tasks = Arc::new(Mutex::new(vec![TASK_A]));
    let transport = listing(&tasks);
    let client = client(transport.clone());
    let mut watcher = SectionWatcher::new(section(), SeenTasks::new(Duration::from_secs(3600)))
        .fields(fields![Task: name]);

    // The first poll records what is already there.
    let first = watcher.poll(&client).await.unwrap();
    assert!(first.tasks.is_empty());
    assert!(first.complete);

    tasks.lock().unwrap().push(TASK_B);
    assert_eq!(gids(&watcher.poll(&client).await.unwrap()), [TASK_B]);
    assert!(watcher.poll(&client).await.unwrap().tasks.is_empty());

    assert_eq!(transport.count(), 3);
    let request = transport.request(0);
    assert!(
        request
            .uri()
            .path()
            .ends_with(&format!("/sections/{SECTION_A}/tasks"))
    );
    let query = query(&request);
    assert!(query.contains(&("limit".into(), "100".into())));
    assert!(query.contains(&("opt_fields".into(), "name".into())));
}

#[tokio::test]
async fn existing_tasks_can_be_reported_on_the_first_poll() {
    let tasks = Arc::new(Mutex::new(vec![TASK_A, TASK_B]));
    let client = client(listing(&tasks));
    let mut watcher = SectionWatcher::new(section(), SeenTasks::new(Duration::from_secs(3600)))
        .include_existing(true);

    assert_eq!(
        gids(&watcher.poll(&client).await.unwrap()),
        [TASK_A, TASK_B]
    );
}

#[tokio::test]
async fn a_task_the_resolver_already_reported_is_not_reported_again() {
    let clock = ManualClock::new(NOW);
    let seen = SeenTasks::with_clock(Duration::from_secs(3600), clock.clock());
    let resolver = Resolver::builder().seen(seen.clone()).build();
    let tasks = Arc::new(Mutex::new(vec![]));
    let client = client(listing(&tasks));
    let mut watcher = SectionWatcher::new(section(), seen.clone());
    watcher.poll(&client).await.unwrap();

    // The webhook is first for task B; the watcher is first for task C.
    let from_webhook = resolver.coalesce(vec![event(
        "added",
        ("task", TASK_B),
        Some(("section", SECTION_A)),
        Some(ALICE),
        0,
    )]);
    assert!(matches!(
        &from_webhook[..],
        [Coalesced::SectionEntered { .. }]
    ));
    tasks.lock().unwrap().extend([TASK_B, TASK_C]);

    assert_eq!(gids(&watcher.poll(&client).await.unwrap()), [TASK_C]);
    let late_webhook = resolver.coalesce(vec![event(
        "added",
        ("task", TASK_C),
        Some(("section", SECTION_A)),
        Some(ALICE),
        5,
    )]);
    assert!(late_webhook.is_empty(), "{late_webhook:?}");
}

#[tokio::test]
async fn a_task_that_leaves_and_returns_is_reported_again() {
    let tasks = Arc::new(Mutex::new(vec![]));
    let seen = SeenTasks::new(Duration::from_secs(3600));
    let client = client(listing(&tasks));
    let mut watcher = SectionWatcher::new(section(), seen.clone());
    watcher.poll(&client).await.unwrap();

    tasks.lock().unwrap().push(TASK_A);
    assert_eq!(gids(&watcher.poll(&client).await.unwrap()), [TASK_A]);
    tasks.lock().unwrap().clear();
    assert!(watcher.poll(&client).await.unwrap().tasks.is_empty());
    assert!(!seen.contains(&task(TASK_A), &section()));
    tasks.lock().unwrap().push(TASK_A);
    assert_eq!(gids(&watcher.poll(&client).await.unwrap()), [TASK_A]);
}

#[tokio::test]
async fn a_failed_poll_leaves_the_watcher_as_it_was() {
    let tasks = Arc::new(Mutex::new(vec![TASK_A]));
    let failing = Arc::new(Mutex::new(false));
    let listed = listing(&tasks);
    let fail = failing.clone();
    let transport = Scripted::new(move |request, _| {
        if *fail.lock().unwrap() {
            return Ok(reply(403, r#"{"errors": [{"message": "Forbidden"}]}"#));
        }
        listed
            .send(request.clone())
            .now_or_never()
            .expect("the listing answers at once")
    });
    let client = client(transport);
    let mut watcher = SectionWatcher::new(section(), SeenTasks::new(Duration::from_secs(3600)));
    watcher.poll(&client).await.unwrap();

    *failing.lock().unwrap() = true;
    assert!(watcher.poll(&client).await.is_err());
    *failing.lock().unwrap() = false;
    tasks.lock().unwrap().push(TASK_B);
    // Compared with the last successful poll: task A is not new, task B is.
    assert_eq!(gids(&watcher.poll(&client).await.unwrap()), [TASK_B]);
}
