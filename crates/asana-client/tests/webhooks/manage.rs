//! Looking after webhooks: health and its derived answers, listing, deleting, changing filters.

use asana_client::openapi::ids::{ProjectGid, WebhookGid, WorkspaceGid};
use asana_client::webhooks::{WebhookError, WebhookHealth};
use asana_webhooks::{EventAction, Filter};
use futures_util::TryStreamExt;
use time::macros::datetime;

use crate::support::{Scripted, client, reply};
use crate::{PROJECT, WEBHOOK, body_json, query};

const FAILURE_CONTENT: &str = "<html>502 Bad Gateway from acme-proxy</html>";

fn webhook() -> WebhookGid {
    WEBHOOK.parse().unwrap()
}

/// A webhook record shaped like Asana's documented example, failing for a while.
fn failing_record() -> String {
    serde_json::json!({
        "data": {
            "gid": WEBHOOK,
            "resource_type": "webhook",
            "active": true,
            "resource": {"gid": PROJECT, "resource_type": "project", "name": "Acme launch"},
            "target": "https://hooks.example.com/asana/route",
            "created_at": "2026-02-01T10:00:00.000Z",
            "last_success_at": "2026-03-01T08:00:00.000Z",
            "last_failure_at": "2026-03-02T09:00:00.000Z",
            "last_failure_content": FAILURE_CONTENT,
            "delivery_retry_count": 7,
            "next_attempt_after": "2026-03-02T09:30:00.000Z",
            "failure_deletion_timestamp": "2026-03-03T08:00:00.000Z"
        }
    })
    .to_string()
}

#[tokio::test]
async fn health_decodes_a_failing_webhook_and_says_when_it_goes() {
    let transport = Scripted::replies(vec![Ok(reply(200, &failing_record()))]);
    let client = client(transport.clone());

    let health = client.webhooks().health(&webhook()).await.unwrap();

    assert!(health.active);
    assert_eq!(health.delivery_retry_count, 7);
    assert_eq!(
        health.last_failure_at,
        Some(datetime!(2026-03-02 09:00 UTC))
    );
    assert_eq!(
        health.next_attempt_after,
        Some(datetime!(2026-03-02 09:30 UTC))
    );
    assert!(health.is_failing());
    assert_eq!(
        health.will_be_deleted_at(),
        Some(datetime!(2026-03-03 08:00 UTC))
    );
    assert_eq!(health.last_failure_content(), Some(FAILURE_CONTENT));
    // The receiver's own answer stays out of what gets logged.
    assert!(!health.to_string().contains("Bad Gateway"), "{health}");
    assert!(!format!("{health:?}").contains("Bad Gateway"));

    // The health fields are asked for by name.
    let request = transport.request(0);
    assert!(
        request
            .uri()
            .path()
            .ends_with(&format!("/webhooks/{WEBHOOK}"))
    );
    let fields = query(&request)
        .into_iter()
        .find(|(name, _)| name == "opt_fields")
        .unwrap()
        .1;
    for field in [
        "last_success_at",
        "last_failure_content",
        "delivery_retry_count",
        "failure_deletion_timestamp",
    ] {
        assert!(fields.split(',').any(|listed| listed == field), "{fields}");
    }
}

#[tokio::test]
async fn a_failure_followed_by_a_success_is_healthy() {
    let body = serde_json::json!({
        "data": {
            "gid": WEBHOOK,
            "active": true,
            "last_success_at": "2026-03-02T09:10:00.000Z",
            "last_failure_at": "2026-03-02T09:00:00.000Z",
            "last_failure_content": "",
            "delivery_retry_count": 0,
            "failure_deletion_timestamp": null
        }
    })
    .to_string();
    let transport = Scripted::replies(vec![Ok(reply(200, &body))]);
    let client = client(transport);

    let health = client.webhooks().health(&webhook()).await.unwrap();

    assert!(!health.is_failing());
    assert_eq!(health.will_be_deleted_at(), None);
    assert_eq!(health.last_failure_content(), None);
    assert!(health.to_string().contains("healthy"));
}

#[test]
fn a_record_that_never_succeeded_but_failed_is_failing() {
    let record: asana_client::openapi::models::Webhook =
        serde_json::from_value(serde_json::json!({
            "gid": WEBHOOK,
            "active": true,
            "last_failure_at": "2026-03-02T09:00:00.000Z"
        }))
        .unwrap();

    let health = WebhookHealth::from_webhook(&record);

    assert!(health.is_failing());
    assert_eq!(health.delivery_retry_count, 0);
}

#[tokio::test]
async fn deleting_a_webhook_that_is_already_gone_succeeds() {
    let transport = Scripted::replies(vec![Ok(reply(
        404,
        r#"{"errors": [{"message": "webhook: Unknown object"}]}"#,
    ))]);
    let client = client(transport.clone());

    client.webhooks().delete(&webhook()).await.unwrap();

    let request = transport.request(0);
    assert_eq!(request.method(), http::Method::DELETE);
    assert!(
        request
            .uri()
            .path()
            .ends_with(&format!("/webhooks/{WEBHOOK}"))
    );
}

#[tokio::test]
async fn deleting_fails_on_any_other_refusal() {
    let transport = Scripted::replies(vec![Ok(reply(
        403,
        r#"{"errors": [{"message": "Forbidden"}]}"#,
    ))]);
    let client = client(transport);

    let error = client.webhooks().delete(&webhook()).await.unwrap_err();

    assert_eq!(error.status(), Some(http::StatusCode::FORBIDDEN));
}

#[tokio::test]
async fn listing_streams_every_page_for_one_resource() {
    let transport = Scripted::replies(vec![
        Ok(reply(
            200,
            r#"{"data": [{"gid": "1201234567890777", "resource_type": "webhook", "active": true}],
                "next_page": {"offset": "eyJ0eXAiOiJKV1QifQ", "path": "/webhooks", "uri": "https://app.asana.com"}}"#,
        )),
        Ok(reply(
            200,
            r#"{"data": [{"gid": "1201234567890778", "resource_type": "webhook", "active": false}],
                "next_page": null}"#,
        )),
    ]);
    let client = client(transport.clone());
    let workspace: WorkspaceGid = "1201234567890000".parse().unwrap();
    let project: ProjectGid = PROJECT.parse().unwrap();

    let webhooks: Vec<_> = client
        .webhooks()
        .list(workspace)
        .resource(project)
        .stream()
        .try_collect()
        .await
        .unwrap();

    assert_eq!(webhooks.len(), 2);
    let first = query(&transport.request(0));
    assert!(first.contains(&("workspace".into(), "1201234567890000".into())));
    assert!(first.contains(&("resource".into(), PROJECT.into())));
    let second = query(&transport.request(1));
    assert!(second.contains(&("offset".into(), "eyJ0eXAiOiJKV1QifQ".into())));
}

#[tokio::test]
async fn updating_filters_sends_them_as_asana_reads_them() {
    let transport = Scripted::replies(vec![Ok(reply(200, &failing_record()))]);
    let client = client(transport.clone());
    let filters = [Filter::task()
        .action(EventAction::Changed)
        .fields(["assignee"])];

    client
        .webhooks()
        .update_filters(&webhook(), filters.clone())
        .await
        .unwrap();

    let request = transport.request(0);
    assert_eq!(request.method(), http::Method::PUT);
    assert_eq!(
        body_json(&request)["data"]["filters"],
        serde_json::to_value(filters).unwrap()
    );
}

#[tokio::test]
async fn filters_listing_fields_without_changed_are_refused_before_sending() {
    let transport = Scripted::replies(vec![Ok(reply(200, &failing_record()))]);
    let client = client(transport.clone());

    let error = client
        .webhooks()
        .update_filters(
            &webhook(),
            [Filter::task().action(EventAction::Added).field("assignee")],
        )
        .await
        .unwrap_err();

    assert!(
        matches!(error, WebhookError::InvalidFilters(_)),
        "{error:?}"
    );
    assert_eq!(transport.count(), 0);
}
