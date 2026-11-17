//! The workspace task search: the query it sends, its rate limit and the premium error.

use std::time::Duration;

use asana_client::openapi::endpoints::SearchTasksForWorkspaceSortBy;
use asana_client::openapi::ids::{CustomFieldGid, WorkspaceGid};
use asana_client::{CustomFieldFilter, Error, Limits, RetryPolicy, fields};
use time::macros::{date, datetime};

use crate::helpers::{path, query};
use crate::support::{Scripted, client, client_with, reply};

const RESULTS: &str = r#"{"data": [
  {"gid": "1201234567890123", "resource_type": "task", "name": "Acme invoice",
   "created_at": "2026-11-02T21:15:00.000Z"}]}"#;

fn workspace() -> WorkspaceGid {
    "1201234567890100".parse().unwrap()
}

#[tokio::test(start_paused = true)]
async fn the_filters_become_asanas_query_parameters() {
    let transport = Scripted::new(|_, _| Ok(reply(200, RESULTS)));
    let client = client(transport.clone());
    let stage: CustomFieldGid = "1201234567890003".parse().unwrap();

    let tasks = client
        .workspaces()
        .search_tasks(workspace())
        .text("invoice")
        .projects_any([
            "1201234567890301".parse().unwrap(),
            "1201234567890302".parse().unwrap(),
        ])
        .projects_not(["1201234567890303".parse().unwrap()])
        .sections_any(["1201234567890401".parse().unwrap()])
        .assignee_any([asana_client::openapi::ids::UserRef::Me])
        .completed(false)
        .created_at_after(datetime!(2026-11-01 00:00 UTC))
        .modified_on_before(date!(2026 - 11 - 17))
        .due_on(None)
        .custom_field(&stage, CustomFieldFilter::Value, "1201234567890031")
        .custom_field(&stage, CustomFieldFilter::IsSet, "true")
        .param(
            "custom_types.1201234567890500.custom_type_status_option.gid",
            "1201234567890501",
        )
        .sort_by(SearchTasksForWorkspaceSortBy::CreatedAt)
        .sort_ascending(true)
        .limit(250)
        .fields(fields![Task: name, created_at])
        .send()
        .await
        .unwrap();
    assert_eq!(tasks[0].name.as_deref(), Some("Acme invoice"));

    let request = transport.request(0);
    assert_eq!(path(&request), "/workspaces/1201234567890100/tasks/search");
    let pairs: Vec<(String, String)> = query(&request);
    let expected: Vec<(String, String)> = [
        ("limit", "100"),
        ("text", "invoice"),
        ("assignee.any", "me"),
        ("projects.any", "1201234567890301,1201234567890302"),
        ("projects.not", "1201234567890303"),
        ("sections.any", "1201234567890401"),
        ("due_on", "null"),
        ("created_at.after", "2026-11-01T00:00:00.000Z"),
        ("modified_on.before", "2026-11-17"),
        ("completed", "false"),
        ("sort_by", "created_at"),
        ("sort_ascending", "true"),
        ("opt_fields", "name,created_at"),
        ("custom_fields.1201234567890003.value", "1201234567890031"),
        ("custom_fields.1201234567890003.is_set", "true"),
        (
            "custom_types.1201234567890500.custom_type_status_option.gid",
            "1201234567890501",
        ),
    ]
    .into_iter()
    .map(|(name, value)| (name.to_owned(), value.to_owned()))
    .collect();
    assert_eq!(pairs, expected);
}

#[tokio::test(start_paused = true)]
async fn a_search_without_premium_is_premium_required() {
    let transport = Scripted::new(|_, _| {
        Ok(reply(
            402,
            r#"{"errors": [{"message": "Search is only available to premium users.", "help": "For more information on API status codes and how to handle them, read the docs on errors: https://developers.asana.com/docs/errors"}]}"#,
        ))
    });
    let client = client(transport.clone());

    let error = client
        .workspaces()
        .search_tasks(workspace())
        .text("invoice")
        .send()
        .await
        .unwrap_err();
    assert!(matches!(error, Error::PremiumRequired(_)));
    assert_eq!(transport.count(), 1, "a 402 is not retried");
}

#[tokio::test(start_paused = true)]
async fn searches_take_from_the_search_limiter() {
    let transport = Scripted::new(|_, _| Ok(reply(200, RESULTS)));
    let client = client_with(
        transport.clone(),
        RetryPolicy::default(),
        Limits::builder().searches_per_minute(1).build(),
    );
    let start = tokio::time::Instant::now();
    for _ in 0..2 {
        client
            .workspaces()
            .search_tasks(workspace())
            .send()
            .await
            .unwrap();
    }
    let second = transport.times()[1] - start;
    assert!(
        second >= Duration::from_secs(59),
        "the second search waited {second:?}"
    );
}
