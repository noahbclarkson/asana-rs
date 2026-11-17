//! A batch of actions with typed results, one of them failing, and the eleventh refused.

use asana_client::Error;
use asana_client::openapi::endpoints::{GetTask, UpdateTask};
use asana_client::openapi::fields;
use asana_client::openapi::models::TaskUpdateRequest;
use serde_json::json;

use crate::support::{BASE, Scripted, TASK, client, reply};
use crate::{gid, json_body};

const OTHER: &str = "1201234567890124";
const MISSING: &str = "1209999999999999";

#[tokio::test(start_paused = true)]
async fn three_actions_answer_in_order_and_one_failure_stays_its_own() {
    let answer = json!({ "data": [
        { "status_code": 200, "headers": {},
          "body": { "data": { "gid": TASK, "resource_type": "task", "name": "Acme launch" } } },
        { "status_code": 404, "headers": {},
          "body": { "errors": [{ "message": "task: Unknown object: 1209999999999999" }] } },
        { "status_code": 200, "headers": {},
          "body": { "data": { "gid": OTHER, "resource_type": "task", "name": "Renamed" } } },
    ]})
    .to_string();
    let transport = Scripted::new(move |_, _| Ok(reply(200, &answer)));
    let client = client(transport.clone());

    let mut batch = client.batch();
    let first = batch
        .push(
            GetTask::builder(gid(TASK))
                .opt_fields(fields![Task: name, due_on])
                .build(),
        )
        .unwrap();
    let missing = batch.push(GetTask::builder(gid(MISSING)).build()).unwrap();
    let renamed = batch
        .push(
            UpdateTask::builder(gid(OTHER))
                .data(TaskUpdateRequest::builder().name("Renamed").build())
                .build(),
        )
        .unwrap();
    let mut results = batch.send().await.unwrap();

    assert_eq!(
        results.take(first).unwrap().name.as_deref(),
        Some("Acme launch")
    );
    let error = results.take(missing).unwrap_err();
    assert!(error.is_not_found(), "{error:?}");
    assert_eq!(
        results.take(renamed).unwrap().name.as_deref(),
        Some("Renamed")
    );

    let request = transport.request(0);
    assert_eq!(request.uri().to_string(), format!("{BASE}/batch"));
    assert_eq!(
        json_body(&request),
        json!({ "data": { "actions": [
            { "method": "get", "relative_path": format!("/tasks/{TASK}"),
              "options": { "fields": ["name", "due_on"] } },
            { "method": "get", "relative_path": format!("/tasks/{MISSING}") },
            { "method": "put", "relative_path": format!("/tasks/{OTHER}"),
              "data": { "name": "Renamed" } },
        ]}})
    );
}

#[tokio::test(start_paused = true)]
async fn the_eleventh_action_is_refused_when_pushed() {
    let transport = Scripted::new(|_, _| Ok(reply(500, "{}")));
    let client = client(transport.clone());
    let mut batch = client.batch();
    for _ in 0..10 {
        let _ticket = batch.push(GetTask::builder(gid(TASK)).build()).unwrap();
    }
    let error = batch.push(GetTask::builder(gid(TASK)).build()).unwrap_err();
    assert!(matches!(error, Error::BatchFull { limit: 10 }));
    assert_eq!(batch.len(), 10);
    assert_eq!(transport.count(), 0);
}

#[tokio::test(start_paused = true)]
async fn a_ticket_from_another_batch_is_refused() {
    let answer = json!({ "data": [{ "status_code": 200, "body": { "data": { "gid": TASK } } }] })
        .to_string();
    let transport = Scripted::new(move |_, _| Ok(reply(200, &answer)));
    let client = client(transport);
    let mut one = client.batch();
    let mut two = client.batch();
    let _own = one.push(GetTask::builder(gid(TASK)).build()).unwrap();
    let foreign = two.push(GetTask::builder(gid(TASK)).build()).unwrap();
    let mut results = one.send().await.unwrap();
    assert!(matches!(
        results.take(foreign),
        Err(Error::InvalidRequest { .. })
    ));
}
