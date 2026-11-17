//! Webhooks: list, read, change filters, delete, and the checks and bookkeeping around creating
//! one (the handshake itself is in [`crate::webhook_delivery`], because it calls the receiver).

use std::collections::VecDeque;

use asana_openapi::endpoints::{
    CreateWebhook, DeleteWebhook, GetWebhook, GetWebhooks, UpdateWebhook,
};
use serde_json::Value;

use crate::answer::{Answer, Call, Refusal, Reply};
use crate::paging::page;
use crate::router::{Action, Route, route, serve};
use crate::state::{Id, Kind, State, Webhook, lookup};

pub(crate) fn routes() -> Vec<Route> {
    vec![
        route::<CreateWebhook>(Action::CreateWebhook),
        serve::<GetWebhooks>(list),
        serve::<GetWebhook>(get),
        serve::<UpdateWebhook>(update),
        serve::<DeleteWebhook>(delete),
    ]
}

/// A webhook checked and given its GID and secret, waiting on the handshake.
#[derive(Clone, Debug)]
pub(crate) struct Pending {
    pub(crate) gid: Id,
    pub(crate) secret: String,
    pub(crate) target: String,
    resource: Id,
    workspace: Id,
    filters: Vec<Value>,
}

/// Checks a webhook's resource, target and filters as Asana does, and reserves its GID.
pub(crate) fn prepare(
    state: &mut State,
    resource: &str,
    target: &str,
    filters: Vec<Value>,
) -> Result<Pending, Refusal> {
    let kind = state
        .kind_of(resource)
        .filter(|_| state.exists(resource))
        .ok_or_else(|| Refusal::unknown("resource", resource))?;
    if !matches!(
        kind,
        Kind::Workspace | Kind::Project | Kind::Section | Kind::Task | Kind::Tag
    ) {
        return Err(Refusal::bad_request(
            "resource: Webhooks cannot be created on this kind of object",
        ));
    }
    let url =
        url::Url::parse(target).map_err(|_| Refusal::bad_request("target: Not a valid URL"))?;
    if url.scheme() != "https" {
        return Err(Refusal::bad_request(
            "target: The target must be an https URL",
        ));
    }
    let local = match url.host() {
        Some(url::Host::Domain(domain)) => {
            let domain = domain.trim_end_matches('.').to_ascii_lowercase();
            domain == "localhost" || domain.ends_with(".localhost")
        }
        Some(url::Host::Ipv4(address)) => address.is_loopback() || address.is_unspecified(),
        Some(url::Host::Ipv6(address)) => address.is_loopback() || address.is_unspecified(),
        None => true,
    };
    if local {
        return Err(Refusal::forbidden(
            "target: Webhooks cannot be delivered to localhost",
        ));
    }
    if kind == Kind::Workspace {
        if filters.is_empty() {
            return Err(Refusal::bad_request(
                "filters: Webhooks on a workspace must have filters",
            ));
        }
        let lower_level = filters.iter().any(|filter| {
            matches!(
                filter.get("resource_type").and_then(Value::as_str),
                None | Some("task" | "subtask" | "story")
            )
        });
        if lower_level {
            return Err(Refusal::bad_request(
                "filters: Webhooks on a workspace cannot see tasks, subtasks or stories",
            ));
        }
    }
    let workspace = state
        .workspace_of(resource)
        .ok_or_else(|| Refusal::bad_request("resource: Has no workspace"))?;
    let gid = state.new_gid(Kind::Webhook);
    let secret = state.new_token() + &state.new_token();
    Ok(Pending {
        gid,
        secret,
        target: target.to_owned(),
        resource: resource.to_owned(),
        workspace,
        filters,
    })
}

/// Stores a webhook whose handshake succeeded.
pub(crate) fn finish(state: &mut State, pending: Pending) -> Id {
    let created_at = state.now();
    state.webhooks.insert(
        pending.gid.clone(),
        Webhook {
            resource: pending.resource,
            workspace: pending.workspace,
            target: pending.target,
            filters: pending.filters,
            secret: pending.secret,
            created_at,
            last_success_at: None,
            last_failure_at: None,
            last_failure_content: None,
            delivery_retry_count: 0,
            pending: VecDeque::new(),
        },
    );
    pending.gid
}

/// Reads a create request's `data`: resource, target and filters.
pub(crate) fn read_create(call: &Call) -> Result<(String, String, Vec<Value>), Refusal> {
    let resource = call.required("resource")?;
    let target = call.required("target")?;
    let filters = filters(call)?;
    Ok((resource, target, filters))
}

fn filters(call: &Call) -> Result<Vec<Value>, Refusal> {
    match call.data().get("filters") {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(filters)) if filters.iter().all(Value::is_object) => Ok(filters.clone()),
        Some(_) => Err(Refusal::bad_request("filters: Must be a list of objects")),
    }
}

fn list(state: &mut State, call: &Call) -> Reply {
    let workspace = call
        .query("workspace")
        .ok_or_else(|| Refusal::bad_request("workspace: Missing input"))?;
    lookup(&state.workspaces, "workspace", workspace)?;
    let resource = call.query("resource");
    let webhooks: Vec<Id> = state
        .webhooks
        .iter()
        .filter(|(_, webhook)| webhook.workspace == workspace)
        .filter(|(_, webhook)| resource.is_none_or(|resource| webhook.resource == resource))
        .map(|(gid, _)| gid.clone())
        .collect();
    page(state, call, &webhooks)
}

fn get(state: &mut State, call: &Call) -> Reply {
    lookup(&state.webhooks, "webhook", call.param(0))?;
    Ok(Answer::data(call.selection.one(state, call.param(0))))
}

fn update(state: &mut State, call: &Call) -> Reply {
    let gid = call.param(0).to_owned();
    lookup(&state.webhooks, "webhook", &gid)?;
    let filters = filters(call)?;
    if let Some(webhook) = state.webhooks.get_mut(&gid) {
        webhook.filters = filters;
    }
    Ok(Answer::data(call.selection.one(state, &gid)))
}

fn delete(state: &mut State, call: &Call) -> Reply {
    lookup(&state.webhooks, "webhook", call.param(0))?;
    state.webhooks.remove(call.param(0));
    Ok(Answer::empty())
}
