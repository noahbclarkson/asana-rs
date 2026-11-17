//! `client.tasks()`: what each method sends and what it decodes.

use asana_client::custom_field_value::CustomFieldValue;
use asana_client::fields;
use asana_client::openapi::ids::{Gid, SectionGid, TaskGid, UserGid, UserRef};
use asana_client::resources::tasks::{DuplicateInclude, TaskExt};
use asana_client::{Error, openapi::models::JobStatus};
use futures_util::TryStreamExt;
use rust_decimal::Decimal;
use time::macros::{date, datetime};

use crate::{
    EMPTY, OTHER_PROJECT, OTHER_SECTION, OTHER_TASK, PROJECT, SECTION, TAG, TASK, USER, WORKSPACE,
    answering, gid, json, page, paged, sent,
};

/// A task as Asana's documentation shows one, with invented GIDs.
fn documented_task() -> String {
    format!(
        r#"{{"data": {{
            "gid": "{TASK}", "resource_type": "task", "name": "Buy catnip",
            "resource_subtype": "default_task", "completed": false, "due_on": "2026-12-17",
            "notes": "Mittens really likes the stuff from Humboldt.",
            "permalink_url": "https://app.asana.com/0/resource/{TASK}/list",
            "assignee": {{"gid": "{USER}", "resource_type": "user", "name": "Greg Sanchez"}},
            "memberships": [
                {{"project": {{"gid": "{PROJECT}", "resource_type": "project", "name": "Stuff to buy"}},
                  "section": {{"gid": "{SECTION}", "resource_type": "section", "name": "Next Actions"}}}},
                {{"project": {{"gid": "{OTHER_PROJECT}", "resource_type": "project", "name": "Acme"}},
                  "section": {{"gid": "{OTHER_SECTION}", "resource_type": "section", "name": "Done"}}}}
            ],
            "some_field_added_later": {{"kept": true}}
        }}}}"#
    )
}

fn compact(gid: &str, name: &str) -> serde_json::Value {
    serde_json::json!({ "gid": gid, "resource_type": "task", "name": name })
}

#[tokio::test(start_paused = true)]
async fn get_sends_the_chosen_fields_and_decodes_the_task() {
    let (transport, client) = answering(200, &documented_task());
    let task = client
        .tasks()
        .get(gid(TASK))
        .fields(fields![Task: name, due_on, memberships.section.name])
        .send()
        .await
        .unwrap();

    let request = sent(&transport, 0);
    assert_eq!(request.method, "GET");
    assert_eq!(request.path, format!("/tasks/{TASK}"));
    assert_eq!(
        request.param("opt_fields"),
        Some("name,due_on,memberships.section.name")
    );
    assert_eq!(request.body, None);
    assert_eq!(task.name.as_deref(), Some("Buy catnip"));
    assert_eq!(task.due_on, Some(date!(2026 - 12 - 17)));
}

#[tokio::test(start_paused = true)]
async fn the_task_helpers_read_its_memberships() {
    let (_, client) = answering(200, &documented_task());
    let task = client.tasks().get(gid(TASK)).send().await.unwrap();

    let section = task.section_in(&gid(PROJECT)).unwrap();
    assert_eq!(section.name.as_deref(), Some("Next Actions"));
    assert!(task.section_in(&gid(WORKSPACE)).is_none());
    assert_eq!(task.sections().count(), 2);
    assert!(task.is_in_section(&gid(OTHER_SECTION)));
    assert!(!task.is_in_section(&gid::<SectionGid>("1200000000000999")));
    assert_eq!(
        task.permalink(),
        Some(format!("https://app.asana.com/0/resource/{TASK}/list").as_str())
    );
}

#[tokio::test(start_paused = true)]
async fn a_project_list_reads_every_page_through_get_tasks() {
    let (transport, client) = paged(vec![
        (
            None,
            page(
                &[compact(TASK, "a"), compact(OTHER_TASK, "b")],
                Some("eyJvZmZzZXQiOjF9"),
            ),
        ),
        (
            Some("eyJvZmZzZXQiOjF9"),
            page(&[compact("1201234567890125", "c")], None),
        ),
    ]);
    let since = datetime!(2026-11-01 00:00 UTC);
    let tasks: Vec<_> = client
        .tasks()
        .list()
        .project(gid(PROJECT))
        .modified_since(since)
        .fields(fields![Task: name])
        .stream()
        .try_collect()
        .await
        .unwrap();

    let names: Vec<_> = tasks
        .iter()
        .filter_map(|task| task.name.as_deref())
        .collect();
    assert_eq!(names, ["a", "b", "c"]);
    assert_eq!(transport.count(), 2);
    let first = sent(&transport, 0);
    assert_eq!(first.path, "/tasks");
    assert_eq!(first.param("project"), Some(PROJECT));
    assert_eq!(first.param("limit"), Some("100"));
    assert_eq!(first.param("opt_fields"), Some("name"));
    assert_eq!(
        first.param("modified_since"),
        Some("2026-11-01T00:00:00.000Z")
    );
    assert_eq!(first.param("offset"), None);
    assert_eq!(
        sent(&transport, 1).param("offset"),
        Some("eyJvZmZzZXQiOjF9")
    );
}

#[tokio::test(start_paused = true)]
async fn a_tag_list_uses_the_tags_own_operation() {
    let (transport, client) = paged(vec![(None, page(&[compact(TASK, "a")], None))]);
    let tasks = client
        .tasks()
        .list()
        .tag(gid(TAG))
        .collect_max(10)
        .await
        .unwrap();

    assert_eq!(tasks.len(), 1);
    let request = sent(&transport, 0);
    assert_eq!(request.path, format!("/tags/{TAG}/tasks"));
    assert_eq!(request.param("limit"), Some("10"));
}

#[tokio::test(start_paused = true)]
async fn an_assignee_list_names_the_user_and_the_workspace() {
    let (transport, client) = paged(vec![(None, page(&[], None))]);
    let tasks = client
        .tasks()
        .list()
        .assignee(UserRef::Me, gid(WORKSPACE))
        .first_page()
        .await
        .unwrap();

    assert!(tasks.items.is_empty());
    let request = sent(&transport, 0);
    assert_eq!(request.path, "/tasks");
    assert_eq!(request.param("assignee"), Some("me"));
    assert_eq!(request.param("workspace"), Some(WORKSPACE));
}

#[tokio::test(start_paused = true)]
async fn a_list_that_asana_would_refuse_is_refused_before_sending() {
    let (transport, client) = answering(200, &page(&[], None));

    let none = client.tasks().list().collect_max(5).await.unwrap_err();
    assert!(matches!(none, Error::InvalidRequest { .. }));

    let two = client
        .tasks()
        .list()
        .project(gid(PROJECT))
        .section(gid(SECTION))
        .stream()
        .try_collect::<Vec<_>>()
        .await
        .unwrap_err();
    assert!(matches!(two, Error::InvalidRequest { .. }));

    let tag_since = client
        .tasks()
        .list()
        .tag(gid(TAG))
        .completed_since(datetime!(2026-11-01 00:00 UTC))
        .first_page()
        .await
        .unwrap_err();
    assert!(matches!(
        tag_since,
        Error::InvalidRequest {
            operation: "getTasksForTag",
            ..
        }
    ));
    assert_eq!(transport.count(), 0);
}

#[tokio::test(start_paused = true)]
async fn create_sends_every_field_it_was_given() {
    let (transport, client) = answering(201, &documented_task());
    let task = client
        .tasks()
        .create()
        .name("Buy catnip")
        .html_notes("<body>Mittens <strong>really</strong> likes it.</body>")
        .due_on(date!(2026 - 12 - 17))
        .assignee(UserRef::email("pa@example.com"))
        .section(gid(PROJECT), gid(SECTION))
        .project(gid(OTHER_PROJECT))
        .follower(UserRef::Me)
        .follower(gid::<UserGid>(USER))
        .tag(gid(TAG))
        .custom_field(
            gid("1200000000000501"),
            CustomFieldValue::text("Smith group"),
        )
        .custom_field(
            gid("1200000000000502"),
            CustomFieldValue::number(Decimal::new(125_050, 2)),
        )
        .custom_field(
            gid("1200000000000503"),
            CustomFieldValue::enum_option(gid::<asana_client::openapi::ids::EnumOptionGid>(
                "1200000000000600",
            )),
        )
        .custom_field(gid("1200000000000504"), CustomFieldValue::clear())
        .fields(fields![Task: name])
        .send()
        .await
        .unwrap();

    let request = sent(&transport, 0);
    assert_eq!(request.method, "POST");
    assert_eq!(request.path, "/tasks");
    assert_eq!(request.param("opt_fields"), Some("name"));
    assert_eq!(
        request.body.unwrap(),
        json(&format!(
            r#"{{"data": {{
                "name": "Buy catnip",
                "html_notes": "<body>Mittens <strong>really</strong> likes it.</body>",
                "due_on": "2026-12-17",
                "assignee": "pa@example.com",
                "memberships": [{{"project": "{PROJECT}", "section": "{SECTION}"}}],
                "projects": ["{OTHER_PROJECT}"],
                "followers": ["me", "{USER}"],
                "tags": ["{TAG}"],
                "custom_fields": {{
                    "1200000000000501": "Smith group",
                    "1200000000000502": 1250.50,
                    "1200000000000503": "1200000000000600",
                    "1200000000000504": null
                }}
            }}}}"#
        ))
    );
    assert_eq!(task.gid, Some(gid(TASK)));
}

#[tokio::test(start_paused = true)]
async fn a_failed_create_is_not_sent_again() {
    let (transport, client) = answering(503, r#"{"errors": [{"message": "Down"}]}"#);
    let error = client
        .tasks()
        .create()
        .name("Once only")
        .workspace(gid(WORKSPACE))
        .send()
        .await
        .unwrap_err();

    assert!(matches!(error, Error::Server { .. }));
    assert_eq!(transport.count(), 1);
}

#[tokio::test(start_paused = true)]
async fn clearing_a_due_date_sends_null_and_nothing_else() {
    let (transport, client) = answering(200, &documented_task());
    client
        .tasks()
        .update(gid(TASK))
        .clear_due_on()
        .send()
        .await
        .unwrap();

    let request = sent(&transport, 0);
    assert_eq!(request.method, "PUT");
    assert_eq!(request.path, format!("/tasks/{TASK}"));
    assert_eq!(request.body.unwrap(), json(r#"{"data": {"due_on": null}}"#));
}

#[tokio::test(start_paused = true)]
async fn update_sets_and_clears_side_by_side() {
    let (transport, client) = answering(200, &documented_task());
    client
        .tasks()
        .update(gid(TASK))
        .name("Buy more catnip")
        .completed(true)
        .due_at(datetime!(2026-12-17 02:30 UTC))
        .unassign()
        .clear_start_on()
        .custom_field(gid("1200000000000501"), CustomFieldValue::text("Acme"))
        .send()
        .await
        .unwrap();

    assert_eq!(
        sent(&transport, 0).body.unwrap(),
        json(
            r#"{"data": {
                "name": "Buy more catnip", "completed": true,
                "due_at": "2026-12-17T02:30:00Z", "assignee": null, "start_on": null,
                "custom_fields": {"1200000000000501": "Acme"}
            }}"#
        )
    );
}

#[tokio::test(start_paused = true)]
async fn delete_sends_a_delete() {
    let (transport, client) = answering(200, EMPTY);
    client.tasks().delete(gid(TASK)).send().await.unwrap();

    let request = sent(&transport, 0);
    assert_eq!(request.method, "DELETE");
    assert_eq!(request.path, format!("/tasks/{TASK}"));
}

#[tokio::test(start_paused = true)]
async fn subtasks_are_listed_and_created_under_the_parent() {
    let (transport, client) = paged(vec![(None, page(&[compact(OTHER_TASK, "child")], None))]);
    let children = client
        .tasks()
        .subtasks(gid(TASK))
        .collect_max(50)
        .await
        .unwrap();
    assert_eq!(children[0].name.as_deref(), Some("child"));
    assert_eq!(sent(&transport, 0).path, format!("/tasks/{TASK}/subtasks"));

    let (transport, client) = answering(201, &documented_task());
    client
        .tasks()
        .create_subtask(gid(TASK))
        .name("Child")
        .assignee(gid::<UserGid>(USER))
        .send()
        .await
        .unwrap();
    let request = sent(&transport, 0);
    assert_eq!(request.method, "POST");
    assert_eq!(request.path, format!("/tasks/{TASK}/subtasks"));
    assert_eq!(
        request.body.unwrap(),
        json(&format!(
            r#"{{"data": {{"name": "Child", "assignee": "{USER}"}}}}"#
        ))
    );
}

#[tokio::test(start_paused = true)]
async fn add_project_places_the_task_in_a_section() {
    let (transport, client) = answering(200, EMPTY);
    client
        .tasks()
        .add_project(gid(TASK), gid(PROJECT))
        .section(gid(SECTION))
        .insert_before(gid(OTHER_TASK))
        .send()
        .await
        .unwrap();
    client
        .tasks()
        .add_project(gid(TASK), gid(PROJECT))
        .section(gid(SECTION))
        .at_start()
        .send()
        .await
        .unwrap();

    let request = sent(&transport, 0);
    assert_eq!(request.method, "POST");
    assert_eq!(request.path, format!("/tasks/{TASK}/addProject"));
    assert_eq!(
        request.body.unwrap(),
        json(&format!(
            r#"{{"data": {{"project": "{PROJECT}", "section": "{SECTION}", "insert_before": "{OTHER_TASK}"}}}}"#
        ))
    );
    assert_eq!(
        sent(&transport, 1).body.unwrap(),
        json(&format!(
            r#"{{"data": {{"project": "{PROJECT}", "section": "{SECTION}", "insert_after": null}}}}"#
        ))
    );
}

#[tokio::test(start_paused = true)]
async fn the_membership_calls_send_their_bodies() {
    let (transport, client) = answering(200, &documented_task());
    let tasks = client.tasks();
    tasks
        .remove_project(gid(TASK), gid(PROJECT))
        .send()
        .await
        .unwrap();
    tasks
        .add_followers(gid(TASK), [UserRef::Me, UserRef::email("pa@example.com")])
        .send()
        .await
        .unwrap();
    tasks
        .remove_followers(gid(TASK), [gid::<UserGid>(USER)])
        .send()
        .await
        .unwrap();
    tasks.add_tag(gid(TASK), gid(TAG)).send().await.unwrap();
    tasks.remove_tag(gid(TASK), gid(TAG)).send().await.unwrap();

    let expected = [
        ("removeProject", format!(r#"{{"project": "{PROJECT}"}}"#)),
        (
            "addFollowers",
            r#"{"followers": ["me", "pa@example.com"]}"#.to_owned(),
        ),
        ("removeFollowers", format!(r#"{{"followers": ["{USER}"]}}"#)),
        ("addTag", format!(r#"{{"tag": "{TAG}"}}"#)),
        ("removeTag", format!(r#"{{"tag": "{TAG}"}}"#)),
    ];
    for (index, (action, data)) in expected.iter().enumerate() {
        let request = sent(&transport, index);
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, format!("/tasks/{TASK}/{action}"));
        assert_eq!(
            request.body.unwrap(),
            json(&format!(r#"{{"data": {data}}}"#)),
            "{action}"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn an_idempotent_membership_call_is_retried_after_a_server_error() {
    let transport = crate::support::Scripted::replies(vec![
        Ok(crate::support::reply(
            503,
            r#"{"errors": [{"message": "Down"}]}"#,
        )),
        Ok(crate::support::reply(200, EMPTY)),
    ]);
    let client = crate::support::client(std::sync::Arc::clone(&transport));
    client
        .tasks()
        .add_tag(gid(TASK), gid(TAG))
        .send()
        .await
        .unwrap();
    assert_eq!(transport.count(), 2);
}

#[tokio::test(start_paused = true)]
async fn set_parent_places_the_task_and_remove_parent_sends_null() {
    let (transport, client) = answering(200, &documented_task());
    client
        .tasks()
        .set_parent(gid(OTHER_TASK), gid(TASK))
        .insert_after(gid("1201234567890125"))
        .send()
        .await
        .unwrap();
    client
        .tasks()
        .remove_parent(gid(OTHER_TASK))
        .send()
        .await
        .unwrap();

    let request = sent(&transport, 0);
    assert_eq!(request.path, format!("/tasks/{OTHER_TASK}/setParent"));
    assert_eq!(
        request.body.unwrap(),
        json(&format!(
            r#"{{"data": {{"parent": "{TASK}", "insert_after": "1201234567890125"}}}}"#
        ))
    );
    assert_eq!(
        sent(&transport, 1).body.unwrap(),
        json(r#"{"data": {"parent": null}}"#)
    );
}

#[tokio::test(start_paused = true)]
async fn duplicate_names_the_copy_and_returns_the_job() {
    let (transport, client) = answering(
        201,
        &format!(
            r#"{{"data": {{"gid": "1200000000000700", "resource_type": "job",
                "resource_subtype": "duplicate_task", "status": "in_progress",
                "new_task": {{"gid": "{OTHER_TASK}", "resource_type": "task", "name": "Copy"}}}}}}"#
        ),
    );
    let job = client
        .tasks()
        .duplicate(gid(TASK), "Copy")
        .include([DuplicateInclude::Assignee, DuplicateInclude::Subtasks])
        .send()
        .await
        .unwrap();

    let request = sent(&transport, 0);
    assert_eq!(request.method, "POST");
    assert_eq!(request.path, format!("/tasks/{TASK}/duplicate"));
    assert_eq!(
        request.body.unwrap(),
        json(r#"{"data": {"name": "Copy", "include": "assignee,subtasks"}}"#)
    );
    assert_eq!(job.gid().as_str(), "1200000000000700");
    assert_eq!(job.job().status, Some(JobStatus::InProgress));
    assert_eq!(job.new_task_gid(), Some(&gid::<TaskGid>(OTHER_TASK)));
}

#[tokio::test(start_paused = true)]
async fn dependencies_are_listed_added_and_removed() {
    let (transport, client) = answering(200, &page(&[compact(OTHER_TASK, "first")], None));
    let blockers = client
        .tasks()
        .dependencies(gid(TASK))
        .collect_max(10)
        .await
        .unwrap();
    assert_eq!(blockers.len(), 1);
    assert_eq!(
        sent(&transport, 0).path,
        format!("/tasks/{TASK}/dependencies")
    );

    let (transport, client) = answering(200, EMPTY);
    client
        .tasks()
        .add_dependencies(gid(TASK), [gid(OTHER_TASK)])
        .send()
        .await
        .unwrap();
    client
        .tasks()
        .remove_dependents(gid(TASK), [gid(OTHER_TASK)])
        .send()
        .await
        .unwrap();
    let added = sent(&transport, 0);
    assert_eq!(added.path, format!("/tasks/{TASK}/addDependencies"));
    assert_eq!(
        added.body.unwrap(),
        json(&format!(
            r#"{{"data": {{"dependencies": ["{OTHER_TASK}"]}}}}"#
        ))
    );
    let removed = sent(&transport, 1);
    assert_eq!(removed.path, format!("/tasks/{TASK}/removeDependents"));
    assert_eq!(
        removed.body.unwrap(),
        json(&format!(
            r#"{{"data": {{"dependents": ["{OTHER_TASK}"]}}}}"#
        ))
    );
}
