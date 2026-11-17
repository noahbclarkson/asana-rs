//! The endpoints half's rules, checked on a small specification written in the shape of Asana's.

use serde_json::json;

use super::operation::{self, Data, Method, ParamKind, ResponseShape};
use super::render;
use crate::spec::Spec;

fn path_gid(name: &str) -> serde_json::Value {
    json!({"name": name, "in": "path", "required": true, "schema": {"type": "string"}})
}

fn data(schema: serde_json::Value) -> serde_json::Value {
    json!({"200": {"description": "ok", "content": {"application/json": {"schema": {
        "type": "object", "properties": {"data": schema}
    }}}}})
}

fn spec() -> Spec {
    let task = json!({"$ref": "#/components/schemas/TaskResponse"});
    let compact = json!({"$ref": "#/components/schemas/TaskCompact"});
    let raw = json!({
        "openapi": "3.0.0",
        "info": {"title": "Acme", "version": "1"},
        "components": {
            "parameters": {"task_path_gid": path_gid("task_gid")},
            "schemas": {
                "TaskCompact": {"type": "object", "properties": {"gid": {"type": "string"}}},
                "TaskResponse": {"allOf": [compact, {"type": "object", "properties": {
                    "due_on": {"type": "string", "format": "date", "nullable": true}
                }}]},
            },
        },
        "paths": {
            "/tasks/{task_gid}": {
                "parameters": [{"$ref": "#/components/parameters/task_path_gid"}],
                "get": {
                    "operationId": "getTask",
                    "tags": ["Tasks"],
                    "summary": "Get a task",
                    "security": [{"personalAccessToken": []}, {"oauth2": ["tasks:read"]}],
                    "parameters": [
                        {"$ref": "#/components/parameters/task_path_gid"},
                        {"name": "opt_fields", "in": "query", "style": "form", "explode": false,
                         "schema": {"type": "array", "items": {"type": "string",
                            "enum": ["name", "memberships.section.name"]}}},
                    ],
                    "responses": data(task),
                },
            },
            "/workspaces/{workspace_gid}/tasks/search": {
                "get": {
                    "operationId": "searchTasksForWorkspace",
                    "tags": ["Tasks"],
                    "security": [{"personalAccessToken": []}, {"oauth2": []}],
                    "parameters": [
                        path_gid("workspace_gid"),
                        {"name": "resource", "in": "query", "required": true,
                         "schema": {"type": "string"}},
                        {"name": "due_on", "in": "query",
                         "schema": {"type": "string", "format": "date", "nullable": true}},
                        {"name": "created_at.before", "in": "query",
                         "schema": {"type": "string", "format": "date-time"}},
                        {"name": "completed", "in": "query", "schema": {"type": "boolean"}},
                        {"name": "sort_by", "in": "query",
                         "schema": {"type": "string", "enum": ["due_date", "created_at"]}},
                        {"name": "type", "in": "query", "schema": {"type": "string"}},
                        {"name": "workspace", "in": "query", "schema": {"type": "string"}},
                        {"name": "assignee", "in": "query", "schema": {"type": "string"}},
                        {"name": "limit", "in": "query", "schema": {"type": "integer"}},
                        {"name": "offset", "in": "query", "schema": {"type": "string"}},
                        {"name": "opt_pretty", "in": "query", "schema": {"type": "boolean"}},
                        {"name": "opt_fields", "in": "query", "style": "form", "explode": false,
                         "schema": {"type": "array", "items": {"type": "string",
                            "enum": ["due_on"]}}},
                    ],
                    "responses": data(json!({"type": "array", "items": compact})),
                },
            },
            "/users/{user_gid}/things/{custom_id}": {
                "get": {
                    "operationId": "getThing",
                    "tags": ["Users"],
                    "parameters": [path_gid("custom_id"), path_gid("user_gid")],
                    "responses": data(json!({"type": "object"})),
                },
            },
        },
    });
    Spec::from_raw(raw).unwrap()
}

fn families() -> Vec<String> {
    vec!["Task".to_owned(), "Workspace".to_owned()]
}

/// The operations by `operationId`: the paths are read sorted, not in the order written here.
fn by_id<'a>(operations: &'a [operation::Operation<'a>], id: &str) -> &'a operation::Operation<'a> {
    operations
        .iter()
        .find(|operation| operation.id == id)
        .unwrap()
}

#[test]
fn an_operation_is_named_from_its_operation_id_and_keeps_its_route_and_scope() {
    let spec = spec();
    let operations = operation::collect(&spec, &families()).unwrap();
    let get_task = by_id(&operations, "getTask");
    assert_eq!(get_task.type_name, "GetTask");
    assert_eq!(get_task.method, Method::Get);
    assert_eq!(get_task.route, "/tasks/{task_gid}");
    assert_eq!(get_task.scope.as_deref(), Some("tasks:read"));
    assert_eq!(
        by_id(&operations, "searchTasksForWorkspace").scope,
        None,
        "an empty oauth2 list names no scope"
    );
}

#[test]
fn opt_fields_belong_to_the_family_the_response_data_names() {
    let spec = spec();
    let operations = operation::collect(&spec, &families()).unwrap();
    let (family, paths) = by_id(&operations, "getTask").opt_fields().unwrap();
    assert_eq!(family, "Task");
    assert_eq!(paths, ["name", "memberships.section.name"]);
    // A list's family is its element's: `[TaskCompact]` is still `Task`.
    let search = by_id(&operations, "searchTasksForWorkspace");
    assert!(matches!(
        search.response,
        ResponseShape::Data(Data::Many(_))
    ));
    assert_eq!(search.opt_fields().unwrap().0, "Task");
}

#[test]
fn path_parameters_are_typed_gids_except_users_and_custom_ids() {
    let spec = spec();
    let operations = operation::collect(&spec, &families()).unwrap();
    // Declared once on the path item and again on the operation: one parameter.
    let get_task = by_id(&operations, "getTask");
    assert_eq!(get_task.path_params.len(), 1);
    assert_eq!(
        get_task.path_params[0].kind,
        ParamKind::Gid("TaskGid".to_owned())
    );
    let thing = by_id(&operations, "getThing");
    let names: Vec<&str> = thing
        .path_params
        .iter()
        .map(|param| param.name.as_str())
        .collect();
    assert_eq!(names, ["user_gid", "custom_id"], "in route order");
    assert_eq!(thing.path_params[0].kind, ParamKind::UserRef);
    assert_eq!(thing.path_params[1].kind, ParamKind::Text);
}

#[test]
fn query_parameters_take_their_real_types() {
    let spec = spec();
    let operations = operation::collect(&spec, &families()).unwrap();
    let search = by_id(&operations, "searchTasksForWorkspace");
    let kind = |name: &str| {
        &search
            .query_params
            .iter()
            .find(|param| param.name == name)
            .unwrap()
            .kind
    };
    assert_eq!(kind("due_on"), &ParamKind::Date { nullable: true });
    assert_eq!(kind("created_at.before"), &ParamKind::DateTime);
    assert_eq!(kind("completed"), &ParamKind::Bool);
    assert_eq!(
        kind("workspace"),
        &ParamKind::Gid("WorkspaceGid".to_owned())
    );
    assert_eq!(kind("assignee"), &ParamKind::UserRef);
    assert_eq!(kind("limit"), &ParamKind::Limit);
    assert_eq!(kind("offset"), &ParamKind::Offset);
    assert_eq!(
        kind("sort_by"),
        &ParamKind::Enum {
            type_name: "SearchTasksForWorkspaceSortBy".to_owned(),
            values: vec!["due_date".to_owned(), "created_at".to_owned()],
        }
    );
    assert!(
        search
            .query_params
            .iter()
            .all(|param| param.name != "opt_pretty"),
        "opt_pretty only indents the response"
    );
    let field = |name: &str| {
        search
            .query_params
            .iter()
            .find(|param| param.name == name)
            .unwrap()
            .field
            .clone()
    };
    assert_eq!(field("created_at.before"), "created_at_before");
    assert_eq!(field("type"), "r#type");
    assert!(search.paginated(), "it takes offset and returns an array");
    assert!(!by_id(&operations, "getTask").paginated());
}

#[test]
fn required_query_parameters_come_first_and_optional_ones_go_through_the_helper() {
    let spec = spec();
    let operations = operation::collect(&spec, &families()).unwrap();
    let search = by_id(&operations, "searchTasksForWorkspace");
    let rendered = render::query_fn(&search.query_params).to_string();
    let required = rendered.find("\"resource\"").unwrap();
    let helper = rendered.find("super :: query").unwrap();
    let optional = rendered.find("\"due_on\"").unwrap();
    assert!(helper < required && required < optional, "{rendered}");
    assert!(rendered.contains("super :: nullable_date"));
    assert!(rendered.contains("super :: date_time"));
    assert!(rendered.contains("to_query_value"));
}

#[test]
fn a_dotted_path_becomes_one_pascal_case_variant() {
    assert_eq!(
        render::variant("memberships.section.name").to_string(),
        "MembershipsSectionName"
    );
    assert_eq!(render::variant("due_on").to_string(), "DueOn");
}
