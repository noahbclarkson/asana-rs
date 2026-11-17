//! Reading one event from JSON, and writing it back in Asana's shape.
//!
//! Written by hand rather than derived so that each way an event can be wrong is a named
//! [`UnreadableReason`] and never a serde message (which can quote the offending value), and so
//! that a change value keeps its raw JSON beside the resource it names.

use serde_json::{Map, Value};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use super::{
    Change, ChangeValue, Event, EventAction, EventUser, ResourceGid, ResourceRef, UnreadableReason,
};
use asana_openapi::ids::{Gid, UserGid};

/// The names of one resource-shaped field and its parts, for reporting which one was wrong.
struct Names {
    object: &'static str,
    gid: &'static str,
    resource_type: &'static str,
}

const RESOURCE: Names = Names {
    object: "resource",
    gid: "resource.gid",
    resource_type: "resource.resource_type",
};

const PARENT: Names = Names {
    object: "parent",
    gid: "parent.gid",
    resource_type: "parent.resource_type",
};

pub(super) fn event_from_json(value: &Value) -> Result<Event, UnreadableReason> {
    let object = value
        .as_object()
        .ok_or(UnreadableReason::NotAnObject("event"))?;
    let user = match present(object, "user") {
        None => None,
        Some(user) => Some(user_from_json(user)?),
    };
    let resource = present(object, "resource").ok_or(UnreadableReason::Missing("resource"))?;
    let resource = resource_from_json(resource, &RESOURCE)?;
    let parent = match present(object, "parent") {
        None => None,
        Some(parent) => Some(resource_from_json(parent, &PARENT)?),
    };
    let action = EventAction::parse(required_str(object, "action", "action")?);
    let change = match present(object, "change") {
        None => None,
        Some(change) => Some(change_from_json(change)?),
    };
    let created_at =
        OffsetDateTime::parse(required_str(object, "created_at", "created_at")?, &Rfc3339)
            .map_err(|_| UnreadableReason::InvalidTimestamp)?;
    Ok(Event {
        user,
        resource,
        parent,
        action,
        change,
        created_at,
    })
}

/// The field's value, treating `null` as absent.
fn present<'a>(object: &'a Map<String, Value>, key: &str) -> Option<&'a Value> {
    object.get(key).filter(|value| !value.is_null())
}

fn required_str<'a>(
    object: &'a Map<String, Value>,
    key: &str,
    name: &'static str,
) -> Result<&'a str, UnreadableReason> {
    present(object, key)
        .ok_or(UnreadableReason::Missing(name))?
        .as_str()
        .ok_or(UnreadableReason::NotAString(name))
}

fn optional_string(object: &Map<String, Value>, key: &str) -> Option<String> {
    object.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn user_from_json(value: &Value) -> Result<EventUser, UnreadableReason> {
    let object = value
        .as_object()
        .ok_or(UnreadableReason::NotAnObject("user"))?;
    let gid = UserGid::parse(required_str(object, "gid", "user.gid")?)
        .map_err(|_| UnreadableReason::InvalidGid("user.gid"))?;
    Ok(EventUser {
        gid,
        name: optional_string(object, "name"),
    })
}

fn resource_from_json(value: &Value, names: &Names) -> Result<ResourceRef, UnreadableReason> {
    let object = value
        .as_object()
        .ok_or(UnreadableReason::NotAnObject(names.object))?;
    let gid = required_str(object, "gid", names.gid)?;
    let resource_type = required_str(object, "resource_type", names.resource_type)?;
    let gid = ResourceGid::new(resource_type, gid)
        .map_err(|_| UnreadableReason::InvalidGid(names.gid))?;
    Ok(ResourceRef {
        gid,
        resource_subtype: optional_string(object, "resource_subtype"),
        name: optional_string(object, "name"),
    })
}

fn change_from_json(value: &Value) -> Result<Change, UnreadableReason> {
    let object = value
        .as_object()
        .ok_or(UnreadableReason::NotAnObject("change"))?;
    let value_of = |key: &str| object.get(key).map(change_value_from_json);
    Ok(Change {
        field: required_str(object, "field", "change.field")?.to_owned(),
        action: EventAction::parse(required_str(object, "action", "change.action")?),
        new_value: value_of("new_value"),
        added_value: value_of("added_value"),
        removed_value: value_of("removed_value"),
    })
}

/// Never fails: a value that does not name a resource is simply kept as raw JSON.
pub(crate) fn change_value_from_json(value: &Value) -> ChangeValue {
    let resource = value.as_object().and_then(|object| {
        let gid = object.get("gid")?.as_str()?;
        let resource_type = object.get("resource_type")?.as_str()?;
        Some(ResourceRef {
            gid: ResourceGid::new(resource_type, gid).ok()?,
            resource_subtype: optional_string(object, "resource_subtype"),
            name: optional_string(object, "name"),
        })
    });
    ChangeValue {
        raw: value.clone(),
        resource,
    }
}

pub(super) fn event_to_json(event: &Event) -> Value {
    let mut object = Map::new();
    object.insert(
        "user".to_owned(),
        event.user.as_ref().map_or(Value::Null, |user| {
            let mut user_object = Map::new();
            user_object.insert("gid".to_owned(), Value::from(user.gid.as_str()));
            user_object.insert("resource_type".to_owned(), Value::from("user"));
            if let Some(name) = &user.name {
                user_object.insert("name".to_owned(), Value::from(name.as_str()));
            }
            Value::Object(user_object)
        }),
    );
    object.insert("resource".to_owned(), resource_to_json(&event.resource));
    object.insert(
        "parent".to_owned(),
        event.parent.as_ref().map_or(Value::Null, resource_to_json),
    );
    object.insert("action".to_owned(), Value::from(event.action.as_str()));
    if let Some(change) = &event.change {
        let mut change_object = Map::new();
        change_object.insert("field".to_owned(), Value::from(change.field.as_str()));
        change_object.insert("action".to_owned(), Value::from(change.action.as_str()));
        for (key, value) in [
            ("new_value", &change.new_value),
            ("added_value", &change.added_value),
            ("removed_value", &change.removed_value),
        ] {
            if let Some(value) = value {
                change_object.insert(key.to_owned(), value.raw.clone());
            }
        }
        object.insert("change".to_owned(), Value::Object(change_object));
    }
    object.insert(
        "created_at".to_owned(),
        Value::from(format_timestamp(event.created_at)),
    );
    Value::Object(object)
}

fn resource_to_json(resource: &ResourceRef) -> Value {
    let mut object = Map::new();
    object.insert("gid".to_owned(), Value::from(resource.gid.gid_str()));
    object.insert(
        "resource_type".to_owned(),
        Value::from(resource.gid.resource_type()),
    );
    if let Some(subtype) = &resource.resource_subtype {
        object.insert("resource_subtype".to_owned(), Value::from(subtype.as_str()));
    }
    if let Some(name) = &resource.name {
        object.insert("name".to_owned(), Value::from(name.as_str()));
    }
    Value::Object(object)
}

/// RFC 3339 in UTC. Every `OffsetDateTime` formats as RFC 3339, so the fallback never runs.
fn format_timestamp(at: OffsetDateTime) -> String {
    at.to_offset(time::UtcOffset::UTC)
        .format(&Rfc3339)
        .unwrap_or_else(|_| at.unix_timestamp().to_string())
}
