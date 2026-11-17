//! Webhooks through the client: establishing one across a simulated handshake, looking after it,
//! resolving events into work, and the section watcher. Against a scripted transport; the GIDs,
//! hosts and secrets are made up.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers fail the test loudly on an impossible setup"
)]

#[path = "../pipeline/support.rs"]
#[expect(
    dead_code,
    reason = "the shared support module has helpers only the pipeline tests use"
)]
mod support;

mod establish;
mod manage;
mod resolver;
mod watcher;

use asana_webhooks::Event;
use bytes::Bytes;
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::macros::datetime;

pub(crate) const WEBHOOK: &str = "1201234567890777";
pub(crate) const PROJECT: &str = "1201234567890456";
pub(crate) const TASK_A: &str = "1201234567890123";
pub(crate) const TASK_B: &str = "1201234567890124";
pub(crate) const TASK_C: &str = "1201234567890125";
pub(crate) const SECTION_A: &str = "1201234567890401";
pub(crate) const SECTION_B: &str = "1201234567890402";
pub(crate) const SECTION_C: &str = "1201234567890403";
pub(crate) const SECTION_D: &str = "1201234567890404";
pub(crate) const ALICE: &str = "1201234567890001";
pub(crate) const BOT: &str = "1201234567890999";

/// The time the tests treat as now.
pub(crate) const NOW: OffsetDateTime = datetime!(2026-03-02 09:20 UTC);

/// A request's body as JSON.
pub(crate) fn body_json(request: &http::Request<Bytes>) -> Value {
    serde_json::from_slice(request.body()).unwrap()
}

/// A request's query parameters.
pub(crate) fn query(request: &http::Request<Bytes>) -> Vec<(String, String)> {
    url::form_urlencoded::parse(request.uri().query().unwrap_or("").as_bytes())
        .into_owned()
        .collect()
}

/// An event in Asana's shape: `action` on `resource` (a task unless the GID says otherwise),
/// with an optional parent, by `user`, `seconds` after 09:00.
pub(crate) fn event(
    action: &str,
    resource: (&str, &str),
    parent: Option<(&str, &str)>,
    user: Option<&str>,
    seconds: i64,
) -> Event {
    let at = datetime!(2026-03-02 09:00 UTC) + time::Duration::seconds(seconds);
    let mut value = json!({
        "user": user.map(|gid| json!({"gid": gid, "resource_type": "user"})),
        "created_at": at.format(&time::format_description::well_known::Rfc3339).unwrap(),
        "action": action,
        "resource": {"gid": resource.1, "resource_type": resource.0},
    });
    if let Some((kind, gid)) = parent {
        value["parent"] = json!({"gid": gid, "resource_type": kind});
    }
    serde_json::from_value(value).unwrap()
}

/// A `changed` event on a task's `field`.
pub(crate) fn changed(task: &str, field: &str, seconds: i64) -> Event {
    let mut event = event("changed", ("task", task), None, Some(ALICE), seconds);
    let mut value = serde_json::to_value(&event).unwrap();
    value["change"] = json!({"field": field, "action": "changed"});
    event = serde_json::from_value(value).unwrap();
    event
}
