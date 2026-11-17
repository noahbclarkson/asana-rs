//! Task search in a workspace, with the filters most callers use.
//!
//! Search is premium-only: a workspace seeded as free answers `402`. Unlike Asana, the fake's
//! index is never behind, so a task is found the moment it is made. As in Asana there is no
//! `offset`: one answer holds at most `limit` tasks (100 by default), and a caller pages by
//! `created_at`. Filters the fake does not understand are ignored rather than refused.

use asana_openapi::endpoints::SearchTasksForWorkspace;
use http::StatusCode;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::answer::{Answer, Call, Refusal, Reply};
use crate::router::{Route, serve};
use crate::state::{FieldValue, Id, State, Task, lookup};

pub(crate) fn routes() -> Vec<Route> {
    vec![serve::<SearchTasksForWorkspace>(search)]
}

fn search(state: &mut State, call: &Call) -> Reply {
    let workspace = call.param(0).to_owned();
    if !lookup(&state.workspaces, "workspace", &workspace)?.premium {
        return Err(Refusal::new(
            StatusCode::PAYMENT_REQUIRED,
            "Search is only available to premium users.",
        ));
    }
    let limit = match call.query("limit") {
        None => 100,
        Some(text) => match text.parse::<usize>() {
            Ok(limit @ 1..=100) => limit,
            _ => {
                return Err(Refusal::bad_request(
                    "limit: The limit must be between 1 and 100",
                ));
            }
        },
    };
    let mut found: Vec<(&Id, &Task)> = Vec::new();
    for (gid, task) in &state.tasks {
        if task.workspace == workspace && keep(call, task)? {
            found.push((gid, task));
        }
    }
    let ascending = call.query("sort_ascending") == Some("true");
    match call.query("sort_by").unwrap_or("modified_at") {
        "created_at" => found.sort_by_key(|(_, task)| task.created_at),
        "completed_at" => found.sort_by_key(|(_, task)| task.completed_at),
        "due_date" => found.sort_by(|(_, a), (_, b)| a.due_on.cmp(&b.due_on)),
        _ => found.sort_by_key(|(_, task)| task.modified_at),
    }
    if !ascending {
        found.reverse();
    }
    let data = found
        .iter()
        .take(limit)
        .map(|(gid, _)| call.selection.item(state, gid))
        .collect();
    Ok(Answer::data(serde_json::Value::Array(data)))
}

/// A comma-separated list of GIDs from the query.
fn list<'a>(call: &'a Call, name: &str) -> Option<Vec<&'a str>> {
    call.query(name).map(|text| {
        text.split(',')
            .map(str::trim)
            .filter(|gid| !gid.is_empty())
            .collect()
    })
}

fn time_after(call: &Call, name: &str) -> Result<Option<OffsetDateTime>, Refusal> {
    call.query(name)
        .map(|text| {
            OffsetDateTime::parse(text, &Rfc3339)
                .map_err(|_| Refusal::bad_request(format!("{name}: Not a valid date-time")))
        })
        .transpose()
}

/// Whether `task` passes every filter the call names.
fn keep(call: &Call, task: &Task) -> Result<bool, Refusal> {
    let projects: Vec<&str> = task
        .memberships
        .iter()
        .map(|membership| membership.project.as_str())
        .collect();
    let sections: Vec<&str> = task
        .memberships
        .iter()
        .filter_map(|membership| membership.section.as_deref())
        .collect();
    let tags: Vec<&str> = task.tags.iter().map(String::as_str).collect();
    let any = |wanted: Option<Vec<&str>>, have: &[&str]| {
        wanted.is_none_or(|wanted| wanted.iter().any(|gid| have.contains(gid)))
    };
    let all = |wanted: Option<Vec<&str>>, have: &[&str]| {
        wanted.is_none_or(|wanted| wanted.iter().all(|gid| have.contains(gid)))
    };
    let none = |wanted: Option<Vec<&str>>, have: &[&str]| {
        wanted.is_none_or(|wanted| !wanted.iter().any(|gid| have.contains(gid)))
    };
    let assignee: Vec<&str> = task.assignee.iter().map(String::as_str).collect();
    let mut keep = any(list(call, "projects.any"), &projects)
        && all(list(call, "projects.all"), &projects)
        && none(list(call, "projects.not"), &projects)
        && any(list(call, "sections.any"), &sections)
        && all(list(call, "sections.all"), &sections)
        && none(list(call, "sections.not"), &sections)
        && any(list(call, "tags.any"), &tags)
        && all(list(call, "tags.all"), &tags)
        && none(list(call, "tags.not"), &tags)
        && any(list(call, "assignee.any"), &assignee)
        && none(list(call, "assignee.not"), &assignee);
    if let Some(text) = call.query("text") {
        let text = text.to_lowercase();
        keep &=
            task.name.to_lowercase().contains(&text) || task.notes.to_lowercase().contains(&text);
    }
    if let Some(completed) = call.query("completed") {
        keep &= (completed == "true") == task.completed;
    }
    if let Some(subtask) = call.query("is_subtask") {
        keep &= (subtask == "true") == task.parent.is_some();
    }
    if let Some(subtype) = call.query("resource_subtype") {
        keep &= task.resource_subtype == subtype;
    }
    if let Some(after) = time_after(call, "created_at.after")? {
        keep &= task.created_at > after;
    }
    if let Some(before) = time_after(call, "created_at.before")? {
        keep &= task.created_at < before;
    }
    if let Some(after) = time_after(call, "modified_at.after")? {
        keep &= task.modified_at > after;
    }
    if let Some(before) = time_after(call, "modified_at.before")? {
        keep &= task.modified_at < before;
    }
    if let Some(due) = call.query("due_on") {
        keep &= task.due_on.as_deref() == Some(due);
    }
    if let Some(after) = call.query("due_on.after") {
        keep &= task.due_on.as_deref().is_some_and(|due| due > after);
    }
    if let Some(before) = call.query("due_on.before") {
        keep &= task.due_on.as_deref().is_some_and(|due| due < before);
    }
    Ok(keep && custom_fields(call, task))
}

/// The `custom_fields.{gid}.{op}` filters.
fn custom_fields(call: &Call, task: &Task) -> bool {
    call.query.iter().all(|(name, wanted)| {
        let Some(rest) = name.strip_prefix("custom_fields.") else {
            return true;
        };
        let Some((field, op)) = rest.split_once('.') else {
            return true;
        };
        let value = task.values.get(field);
        match op {
            "is_set" => (wanted == "true") == value.is_some(),
            "value" => match value {
                Some(FieldValue::Text(text)) => text == wanted,
                Some(FieldValue::Number(number)) => number_eq(number, wanted),
                Some(FieldValue::Enum(option)) => option == wanted,
                Some(FieldValue::MultiEnum(options)) => wanted
                    .split(',')
                    .all(|gid| options.iter().any(|option| option == gid)),
                Some(FieldValue::People(people)) => people.iter().any(|person| person == wanted),
                Some(FieldValue::Date { date, .. }) => date == wanted,
                Some(FieldValue::Reference(references)) => {
                    references.iter().any(|reference| reference == wanted)
                }
                None => false,
            },
            "less_than" | "greater_than" => match (value, wanted.parse::<f64>()) {
                (Some(FieldValue::Number(number)), Ok(bound)) => {
                    number.as_f64().is_some_and(|number| {
                        if op == "less_than" {
                            number < bound
                        } else {
                            number > bound
                        }
                    })
                }
                _ => false,
            },
            "contains" | "starts_with" | "ends_with" => match value {
                Some(FieldValue::Text(text)) => {
                    let (text, wanted) = (text.to_lowercase(), wanted.to_lowercase());
                    match op {
                        "contains" => text.contains(&wanted),
                        "starts_with" => text.starts_with(&wanted),
                        _ => text.ends_with(&wanted),
                    }
                }
                _ => false,
            },
            // An operator the fake does not know is ignored, like an unknown filter.
            _ => true,
        }
    })
}

fn number_eq(number: &serde_json::Number, wanted: &str) -> bool {
    match (number.as_f64(), wanted.parse::<f64>()) {
        (Some(have), Ok(want)) => (have - want).abs() < f64::EPSILON,
        _ => number.to_string() == wanted,
    }
}
