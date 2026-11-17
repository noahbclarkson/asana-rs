//! `client.projects()`: reading and listing projects, and what they delegate.

use asana_client::fields;
use futures_util::TryStreamExt;

use crate::{PROJECT, SECTION, TASK, WORKSPACE, answering, gid, page, paged, sent};

#[tokio::test(start_paused = true)]
async fn get_and_list_send_their_filters() {
    let (transport, client) = answering(
        200,
        &format!(
            r#"{{"data": {{"gid": "{PROJECT}", "resource_type": "project", "name": "Stuff to buy",
                "archived": false, "color": "light-green"}}}}"#
        ),
    );
    let project = client
        .projects()
        .get(gid(PROJECT))
        .fields(fields![Project: name, archived, color])
        .send()
        .await
        .unwrap();
    assert_eq!(project.name.as_deref(), Some("Stuff to buy"));
    let request = sent(&transport, 0);
    assert_eq!(
        (request.method.as_str(), request.path.as_str()),
        ("GET", "/projects/1200000000000100")
    );
    assert_eq!(request.param("opt_fields"), Some("name,archived,color"));

    let (transport, client) = paged(vec![(
        None,
        page(
            &[serde_json::json!({"gid": PROJECT, "resource_type": "project", "name": "Acme"})],
            None,
        ),
    )]);
    let listed: Vec<_> = client
        .projects()
        .list()
        .workspace(gid(WORKSPACE))
        .archived(false)
        .stream()
        .try_collect()
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);
    let request = sent(&transport, 0);
    assert_eq!(request.path, "/projects");
    assert_eq!(request.param("workspace"), Some(WORKSPACE));
    assert_eq!(request.param("archived"), Some("false"));
}

#[tokio::test(start_paused = true)]
async fn sections_and_tasks_delegate_to_their_handles() {
    let (transport, client) = answering(
        200,
        &page(
            &[serde_json::json!({"gid": SECTION, "resource_type": "section", "name": "To do"})],
            None,
        ),
    );
    client
        .projects()
        .sections(gid(PROJECT))
        .collect_max(5)
        .await
        .unwrap();
    assert_eq!(
        sent(&transport, 0).path,
        format!("/projects/{PROJECT}/sections")
    );

    let (transport, client) = answering(
        200,
        &page(
            &[serde_json::json!({"gid": TASK, "resource_type": "task", "name": "a"})],
            None,
        ),
    );
    let tasks = client
        .projects()
        .tasks(gid(PROJECT))
        .collect_max(5)
        .await
        .unwrap();
    assert_eq!(tasks.len(), 1);
    let request = sent(&transport, 0);
    assert_eq!(request.path, "/tasks");
    assert_eq!(request.param("project"), Some(PROJECT));
}

#[tokio::test(start_paused = true)]
async fn custom_field_settings_list_the_projects_fields() {
    let (transport, client) = answering(
        200,
        &page(
            &[serde_json::json!({
                "gid": "1200000000000800", "resource_type": "custom_field_setting",
                "is_important": true,
                "custom_field": {"gid": "1200000000000501", "resource_type": "custom_field",
                    "name": "Status", "resource_subtype": "enum"}
            })],
            None,
        ),
    );
    let settings = client
        .projects()
        .custom_field_settings(gid(PROJECT))
        .fields(fields![CustomFieldSetting: custom_field.name, is_important])
        .collect_max(100)
        .await
        .unwrap();

    let field = settings[0].custom_field.as_ref().unwrap();
    assert_eq!(field.name.as_deref(), Some("Status"));
    let request = sent(&transport, 0);
    assert_eq!(
        request.path,
        format!("/projects/{PROJECT}/custom_field_settings")
    );
    assert_eq!(
        request.param("opt_fields"),
        Some("custom_field.name,is_important")
    );
}

#[tokio::test(start_paused = true)]
async fn task_counts_ask_for_every_count_by_default() {
    let (transport, client) = answering(
        200,
        r#"{"data": {"num_tasks": 200, "num_incomplete_tasks": 50, "num_completed_tasks": 150,
            "num_milestones": 10, "num_incomplete_milestones": 7, "num_completed_milestones": 3}}"#,
    );
    let counts = client
        .projects()
        .task_counts(gid(PROJECT))
        .send()
        .await
        .unwrap();

    assert_eq!(counts.num_tasks, Some(200));
    assert_eq!(counts.num_incomplete_tasks, Some(50));
    let request = sent(&transport, 0);
    assert_eq!(request.path, format!("/projects/{PROJECT}/task_counts"));
    assert_eq!(
        request.param("opt_fields"),
        Some(
            "num_completed_milestones,num_completed_tasks,num_incomplete_milestones,\
             num_incomplete_tasks,num_milestones,num_tasks"
        )
    );
}
