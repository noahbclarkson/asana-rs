//! The shape of real generated descriptors: what each sends (method, route, path, query, body) and
//! what the client needs to know about it (scope, idempotency, envelope, paging).

use asana_openapi::endpoint::{
    Endpoint, Envelope, Method, Offset, PageLimit, Paginated, PartValue, RequestBody,
};
use asana_openapi::endpoints::{
    AddFollowersForTask, AddTaskForSection, AttachmentContent, CreateAttachmentForObject,
    CreateStoryForTask, CreateTask, CreateWebhook, CreatedWebhook, EventsPage, GetEvents,
    GetSectionsForProject, GetTask, GetUser, OPERATIONS, SearchTasksForWorkspace,
    SearchTasksForWorkspaceSortBy, UpdateTask,
};
use asana_openapi::fields;
use asana_openapi::ids::{ProjectGid, SectionGid, TaskGid, UserRef, WorkspaceGid};
use asana_openapi::models::{
    Clearable, Section, SectionTaskInsertRequest, TaskAddFollowersRequest, TaskCreateRequest,
    TaskStoryCreateRequest, TaskUpdateRequest, WebhookFilterInput, WebhookRequest,
};
use asana_openapi::scopes::Scope;
use serde_json::json;

const TASK: &str = "1201234567890123";

fn task() -> TaskGid {
    match TASK.parse() {
        Ok(gid) => gid,
        Err(error) => panic!("{error}"),
    }
}

fn query<E: Endpoint>(endpoint: &E) -> Vec<(String, String)> {
    endpoint
        .query()
        .into_iter()
        .map(|(name, value)| (name.into_owned(), value))
        .collect()
}

fn pairs(expected: &[(&str, &str)]) -> Vec<(String, String)> {
    expected
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect()
}

/// Read through a function, so the assertion is on a value rather than on a constant.
fn idempotent<E: Endpoint>() -> bool {
    E::IDEMPOTENT
}

fn json_body<E: Endpoint>(endpoint: &E) -> serde_json::Value {
    match endpoint.body() {
        Ok(RequestBody::Json(body)) => body,
        other => panic!("expected a JSON body, got {other:?}"),
    }
}

#[test]
fn get_a_task_with_chosen_fields() {
    let get = GetTask::builder(task())
        .opt_fields(fields![Task: name, due_on, memberships.section.name])
        .build();
    assert_eq!(GetTask::OPERATION_ID, "getTask");
    assert_eq!(GetTask::METHOD, Method::Get);
    assert_eq!(GetTask::ROUTE, "/tasks/{task_gid}");
    assert_eq!(GetTask::SCOPE, Some(Scope::TasksRead));
    assert!(idempotent::<GetTask>());
    assert_eq!(GetTask::ENVELOPE, Envelope::Data);
    assert_eq!(get.path(), "/tasks/1201234567890123");
    assert_eq!(
        query(&get),
        pairs(&[("opt_fields", "name,due_on,memberships.section.name")])
    );
    assert!(matches!(get.body().unwrap(), RequestBody::Empty));
    assert!(query(&GetTask::builder(task()).build()).is_empty());
}

#[test]
fn create_a_task_wraps_the_request_in_data() {
    let create = CreateTask::builder()
        .data(
            TaskCreateRequest::builder()
                .name("Draft the plan")
                .workspace(WorkspaceGid::try_from("1201234567890999".to_owned()).unwrap())
                .build(),
        )
        .build();
    assert_eq!(CreateTask::METHOD, Method::Post);
    assert_eq!(CreateTask::ROUTE, "/tasks");
    assert_eq!(CreateTask::SCOPE, Some(Scope::TasksWrite));
    assert!(
        !idempotent::<CreateTask>(),
        "a retried create makes a second task"
    );
    assert_eq!(create.path(), "/tasks");
    assert_eq!(
        json_body(&create),
        json!({"data": {"name": "Draft the plan", "workspace": "1201234567890999"}})
    );
}

#[test]
fn update_a_task_can_clear_its_due_date() {
    let update = UpdateTask::builder(task())
        .data(
            TaskUpdateRequest::builder()
                .due_on(Clearable::Clear)
                .build(),
        )
        .build();
    assert_eq!(UpdateTask::METHOD, Method::Put);
    assert!(idempotent::<UpdateTask>());
    assert_eq!(update.path(), "/tasks/1201234567890123");
    assert_eq!(json_body(&update), json!({"data": {"due_on": null}}));
}

#[test]
fn add_a_task_to_a_section_is_a_post_that_may_be_resent() {
    let section: SectionGid = "1201234567890456".parse().unwrap();
    let add = AddTaskForSection::builder(section)
        .data(SectionTaskInsertRequest::builder().task(task()).build())
        .build();
    assert_eq!(AddTaskForSection::METHOD, Method::Post);
    assert!(
        idempotent::<AddTaskForSection>(),
        "moving a task twice leaves it in one place"
    );
    assert_eq!(add.path(), "/sections/1201234567890456/addTask");
    assert_eq!(json_body(&add), json!({"data": {"task": TASK}}));
}

#[test]
fn add_followers_names_users_as_me_email_or_gid() {
    let add = AddFollowersForTask::builder(task())
        .data(
            TaskAddFollowersRequest::builder()
                .followers(vec![
                    UserRef::Me,
                    UserRef::email("pa@example.com"),
                    UserRef::Gid("1201234567890777".parse().unwrap()),
                ])
                .build(),
        )
        .build();
    assert!(idempotent::<AddFollowersForTask>());
    assert_eq!(add.path(), "/tasks/1201234567890123/addFollowers");
    assert_eq!(
        json_body(&add),
        json!({"data": {"followers": ["me", "pa@example.com", "1201234567890777"]}})
    );
}

#[test]
fn create_a_story_is_never_resent() {
    let story = CreateStoryForTask::builder(task())
        .data(TaskStoryCreateRequest::builder().text("Approved").build())
        .build();
    assert_eq!(CreateStoryForTask::SCOPE, Some(Scope::StoriesWrite));
    assert!(!idempotent::<CreateStoryForTask>());
    assert_eq!(story.path(), "/tasks/1201234567890123/stories");
    assert_eq!(json_body(&story), json!({"data": {"text": "Approved"}}));
}

#[test]
fn a_projects_sections_come_in_pages() {
    fn item_is_section<E: Paginated<Item = Section>>(_: &E) {}

    let project: ProjectGid = "1201234567890321".parse().unwrap();
    let mut list = GetSectionsForProject::builder(project).build();
    item_is_section(&list);
    assert_eq!(list.path(), "/projects/1201234567890321/sections");
    assert!(query(&list).is_empty());
    list.set_limit(PageLimit::new(50));
    list.set_offset(Some(Offset::new("eyJ0eXAiOJiKV1iQLCJhbGciOiJIUzI1NiJ9")));
    assert_eq!(
        query(&list),
        pairs(&[
            ("limit", "50"),
            ("offset", "eyJ0eXAiOJiKV1iQLCJhbGciOiJIUzI1NiJ9")
        ])
    );
    list.set_offset(None);
    assert_eq!(query(&list), pairs(&[("limit", "50")]));
}

#[test]
fn search_renders_dates_booleans_enums_and_null() {
    let workspace: WorkspaceGid = "1201234567890001".parse().unwrap();
    let search = SearchTasksForWorkspace::builder(workspace)
        .text("invoice")
        .completed(false)
        .due_on_before(time::macros::date!(2026 - 12 - 01))
        .start_on(None)
        .sort_by(SearchTasksForWorkspaceSortBy::DueDate)
        .opt_fields(fields![Task: name])
        .build();
    assert_eq!(SearchTasksForWorkspace::SCOPE, Some(Scope::TasksRead));
    assert_eq!(search.path(), "/workspaces/1201234567890001/tasks/search");
    let rendered = query(&search);
    for expected in pairs(&[
        ("text", "invoice"),
        ("completed", "false"),
        ("due_on.before", "2026-12-01"),
        ("start_on", "null"),
        ("sort_by", "due_date"),
        ("opt_fields", "name"),
    ]) {
        assert!(rendered.contains(&expected), "{expected:?} in {rendered:?}");
    }
    assert_eq!(rendered.len(), 6, "only what was set is sent: {rendered:?}");
    assert_eq!(query(&search), rendered, "the order is stable");
}

#[test]
fn create_a_webhook_with_filters_returns_the_whole_body_with_a_hidden_secret() {
    let create = CreateWebhook::builder()
        .data(
            WebhookRequest::builder()
                .resource("1201234567890321")
                .target("https://example.com/hooks/asana")
                .filters(vec![
                    WebhookFilterInput::builder()
                        .resource_type("task")
                        .action("changed")
                        .fields(vec!["due_on".to_owned()])
                        .build(),
                ])
                .build(),
        )
        .build();
    assert_eq!(CreateWebhook::ENVELOPE, Envelope::Whole);
    assert_eq!(CreateWebhook::SCOPE, Some(Scope::WebhooksWrite));
    assert!(!idempotent::<CreateWebhook>());
    assert_eq!(
        json_body(&create),
        json!({"data": {
            "resource": "1201234567890321",
            "target": "https://example.com/hooks/asana",
            "filters": [{"resource_type": "task", "action": "changed", "fields": ["due_on"]}],
        }})
    );

    let created: CreatedWebhook = serde_json::from_value(json!({
        "data": {"gid": "1201234567890555", "resource_type": "webhook", "active": true},
        "X-Hook-Secret": "b537207f20cbfa02357cf448134da559e8bd39d61597dcd5631b8012eae53e81",
    }))
    .unwrap();
    assert!(created.hook_secret.is_some());
    assert!(
        !format!("{created:?}").contains("b537207f"),
        "the secret never prints"
    );
}

#[test]
fn get_events_sends_the_sync_token_and_reads_the_whole_page() {
    let get = GetEvents::builder()
        .resource("1201234567890321")
        .sync("de4774f6915eae04714ca93bb2f5ee81")
        .build();
    assert_eq!(GetEvents::ENVELOPE, Envelope::Whole);
    assert_eq!(get.path(), "/events");
    assert_eq!(
        query(&get),
        pairs(&[
            ("resource", "1201234567890321"),
            ("sync", "de4774f6915eae04714ca93bb2f5ee81"),
        ])
    );

    let page: EventsPage = serde_json::from_value(json!({
        "data": [{
            "action": "changed",
            "resource": {"gid": TASK, "resource_type": "task", "name": "Draft the plan"},
            "created_at": "2026-11-17T01:02:03.456Z",
        }],
        "sync": "edfc0896b370b7a479886d316131bf5c",
        "has_more": false,
    }))
    .unwrap();
    assert_eq!(page.data.len(), 1);
    assert_eq!(
        page.sync.as_deref(),
        Some("edfc0896b370b7a479886d316131bf5c")
    );
    assert!(!page.has_more);
}

#[test]
fn upload_an_attachment_as_multipart_parts() {
    let upload = CreateAttachmentForObject::builder(
        task(),
        AttachmentContent::file(
            "statement.pdf",
            "application/pdf",
            bytes::Bytes::from_static(b"%PDF-1.7"),
        ),
    )
    .build();
    assert_eq!(CreateAttachmentForObject::METHOD, Method::Post);
    assert_eq!(
        CreateAttachmentForObject::SCOPE,
        Some(Scope::AttachmentsWrite)
    );
    assert_eq!(upload.path(), "/attachments");
    let RequestBody::Multipart(parts) = upload.body().unwrap() else {
        panic!("an upload is multipart");
    };
    let names: Vec<&str> = parts.iter().map(|part| part.name.as_ref()).collect();
    assert_eq!(names, ["parent", "resource_subtype", "file"]);
    assert!(matches!(&parts[0].value, PartValue::Text(parent) if parent == TASK));
    assert!(matches!(&parts[1].value, PartValue::Text(subtype) if subtype == "asana"));
    assert!(matches!(
        &parts[2].value,
        PartValue::File { file_name, content_type, bytes }
            if file_name == "statement.pdf" && content_type == "application/pdf" && bytes.len() == 8
    ));
    assert!(
        !format!("{upload:?}").contains("PDF-1.7"),
        "the bytes never print"
    );

    let link = CreateAttachmentForObject::builder(
        task(),
        AttachmentContent::external("https://example.com/files/statement.pdf", "Statement"),
    )
    .connect_to_app(false)
    .build();
    let RequestBody::Multipart(parts) = link.body().unwrap() else {
        panic!("a link is multipart too");
    };
    let texts: Vec<(&str, &str)> = parts
        .iter()
        .map(|part| match &part.value {
            PartValue::Text(text) => (part.name.as_ref(), text.as_str()),
            PartValue::File { .. } => panic!("a link sends no file"),
        })
        .collect();
    assert_eq!(
        texts,
        [
            ("parent", TASK),
            ("resource_subtype", "external"),
            ("url", "https://example.com/files/statement.pdf"),
            ("name", "Statement"),
            ("connect_to_app", "false"),
        ]
    );
}

#[test]
fn a_user_in_a_path_may_be_me_or_an_email() {
    assert_eq!(GetUser::builder(UserRef::Me).build().path(), "/users/me");
    assert_eq!(
        GetUser::builder(UserRef::email("pa@example.com"))
            .build()
            .path(),
        "/users/pa%40example%2Ecom"
    );
}

#[test]
fn the_index_lists_every_operation_in_the_specification_once() {
    let specification = include_str!("../../../spec/asana_oas.yaml");
    let declared = specification
        .lines()
        .filter(|line| line.trim_start().starts_with("operationId:"))
        .count();
    assert_eq!(OPERATIONS.len(), declared);
    let mut ids: Vec<&str> = OPERATIONS.iter().map(|info| info.operation_id).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), OPERATIONS.len());
    let get_task = OPERATIONS
        .iter()
        .find(|info| info.operation_id == "getTask")
        .unwrap();
    assert_eq!(get_task.route, "/tasks/{task_gid}");
    assert_eq!(get_task.tag, "Tasks");
    assert!(
        OPERATIONS
            .iter()
            .any(|info| info.operation_id == "getTasksForProject" && info.paginated)
    );
}
