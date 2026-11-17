//! Instantiating a template and waiting on its job, with tokio's clock paused.

use std::time::Duration;

use asana_client::Error;
use serde_json::json;

use crate::support::{BASE, Scripted, client, reply};
use crate::{JOB, NEW_TASK, TEMPLATE, gid, json_body};

fn job(status: &str) -> String {
    json!({ "data": {
        "gid": JOB,
        "resource_type": "job",
        "resource_subtype": "duplicate_task",
        "status": status,
        "new_task": { "gid": NEW_TASK, "resource_type": "task", "name": "Acme: onboarding" },
        "new_project": null,
    }})
    .to_string()
}

#[tokio::test(start_paused = true)]
async fn a_job_that_succeeds_on_the_third_poll_returns_its_task() {
    let transport = Scripted::replies(vec![
        Ok(reply(201, &job("not_started"))),
        Ok(reply(200, &job("in_progress"))),
        Ok(reply(200, &job("in_progress"))),
        Ok(reply(200, &job("succeeded"))),
    ]);
    let client = client(transport.clone());
    let handle = client
        .task_templates()
        .instantiate(gid(TEMPLATE))
        .name("Acme: onboarding")
        .send()
        .await
        .unwrap();
    assert_eq!(handle.new_task_gid().unwrap().to_string(), NEW_TASK);

    let task = handle
        .wait_for_task(&client, Duration::from_secs(30))
        .await
        .unwrap();
    assert_eq!(task.name.as_deref(), Some("Acme: onboarding"));

    let start = transport.request(0);
    assert_eq!(
        start.uri().to_string(),
        format!("{BASE}/task_templates/{TEMPLATE}/instantiateTask")
    );
    assert_eq!(
        json_body(&start),
        json!({ "data": { "name": "Acme: onboarding" } })
    );
    assert_eq!(transport.count(), 4);
    assert!(
        transport
            .request(1)
            .uri()
            .path()
            .ends_with(&format!("/jobs/{JOB}"))
    );
    let times = transport.times();
    assert_eq!(times[2] - times[1], Duration::from_millis(500));
    assert_eq!(times[3] - times[2], Duration::from_secs(1));
}

#[tokio::test(start_paused = true)]
async fn a_failed_job_is_a_typed_error() {
    let transport = Scripted::replies(vec![
        Ok(reply(201, &job("in_progress"))),
        Ok(reply(200, &job("failed"))),
    ]);
    let client = client(transport);
    let handle = client
        .task_templates()
        .instantiate(gid(TEMPLATE))
        .send()
        .await
        .unwrap();
    let error = handle
        .wait(&client, Duration::from_secs(30))
        .await
        .unwrap_err();
    assert!(matches!(error, Error::JobFailed { job } if job.to_string() == JOB));
}

#[tokio::test(start_paused = true)]
async fn a_job_still_running_at_the_timeout_says_so() {
    let transport = Scripted::new(|_, _| Ok(reply(200, &job("in_progress"))));
    let client = client(transport.clone());
    let handle =
        asana_client::JobHandle::from_job(client.jobs().get(gid(JOB)).send().await.unwrap())
            .unwrap();
    let error = handle
        .wait(&client, Duration::from_secs(3))
        .await
        .unwrap_err();
    assert!(matches!(error, Error::JobTimedOut { .. }), "{error:?}");
    // Polls at 0, 0.5 and 1.5 s; the next wait (2 s) would pass the 3 s timeout.
    assert_eq!(transport.count(), 1 + 3);
}
