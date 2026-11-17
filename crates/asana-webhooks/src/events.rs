//! The events a delivery carries, decoded leniently into types.
//!
//! A delivery's body is `{"events": [...]}`. Each event is compact: who did it (`user`, `null` for
//! the system), what it happened to (`resource`), where (`parent`, for `added` and `removed`), what
//! happened (`action`), which field changed and how (`change`, for `changed`), and when
//! (`created_at`). Integrations are expected to re-read the resource before acting on it.
//!
//! Decoding keeps whatever Asana sends that this crate does not know: an unknown `resource_type`
//! becomes [`ResourceGid::Other`], an unknown action [`EventAction::Unknown`], extra fields are
//! ignored, and an event that cannot be read at all (no `resource`, a malformed timestamp) is kept
//! with its raw JSON in [`EventBatch::unreadable`] instead of failing its neighbours. Only a body
//! that is not JSON, has no `events` array, or holds too many events fails the batch.

mod key;
mod parse;
mod resource;

use std::fmt;

use asana_openapi::ids::{CustomFieldGid, ProjectGid, SectionGid, StoryGid, TaskGid, UserGid};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use time::OffsetDateTime;

pub use key::EventKey;
pub use resource::{EventUser, ResourceGid, ResourceRef, ResourceType};

/// What happened to an event's resource, or (in [`Change::action`]) to one of its fields.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum EventAction {
    /// `added`: created, or added to the event's `parent`.
    Added,
    /// `changed`: one of the resource's fields changed; [`Event::change`] says which.
    Changed,
    /// `removed`: removed from the event's `parent`.
    Removed,
    /// `deleted`: the resource itself was deleted.
    Deleted,
    /// `undeleted`: a deletion was undone.
    Undeleted,
    /// Any other action, as Asana wrote it.
    Unknown(String),
}

impl EventAction {
    /// The action as Asana writes it.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Added => "added",
            Self::Changed => "changed",
            Self::Removed => "removed",
            Self::Deleted => "deleted",
            Self::Undeleted => "undeleted",
            Self::Unknown(other) => other,
        }
    }

    /// Reads an action; an unknown one becomes [`Unknown`](Self::Unknown).
    #[must_use]
    pub fn parse(text: &str) -> Self {
        match text {
            "added" => Self::Added,
            "changed" => Self::Changed,
            "removed" => Self::Removed,
            "deleted" => Self::Deleted,
            "undeleted" => Self::Undeleted,
            other => Self::Unknown(other.to_owned()),
        }
    }
}

impl fmt::Display for EventAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for EventAction {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for EventAction {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Ok(Self::parse(&text))
    }
}

/// One value in a [`Change`]: the raw JSON always, and the resource it names when it names one.
///
/// Asana sends a resource value as `{gid, resource_type}`, sometimes with more (a custom field's new
/// value carries its `enum_value`, `number_value`, …); the raw JSON keeps all of it. A JSON `null`
/// (a cleared assignee, say) is kept as `raw: null` with no resource.
#[derive(Clone, Debug, PartialEq)]
pub struct ChangeValue {
    /// The value as Asana sent it.
    pub raw: Value,
    /// The resource the value names, when it is an object with a `gid` and a `resource_type`.
    pub resource: Option<ResourceRef>,
}

/// How a `changed` event's resource changed.
#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    /// The field that changed, for example `assignee`, `due_on`, `custom_fields` or `name`.
    pub field: String,
    /// How the field changed: `changed` (replaced), `added` or `removed` (for list fields such as
    /// followers or tags).
    pub action: EventAction,
    /// The new value, when `action` is `changed` and Asana sends one (it does for resources).
    pub new_value: Option<ChangeValue>,
    /// The value added to a list field.
    pub added_value: Option<ChangeValue>,
    /// The value removed from a list field.
    pub removed_value: Option<ChangeValue>,
}

/// One change observed by a webhook.
///
/// Implements `Serialize` and `Deserialize` in Asana's own shape, so an event can be queued or
/// stored as JSON and read back.
#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    /// Who caused it; `None` for changes the system made.
    pub user: Option<EventUser>,
    /// The resource that changed. It may be below the webhook's own resource (a task in a watched
    /// project, a story on a watched task).
    pub resource: ResourceRef,
    /// For `added` and `removed`, what the resource was added to or removed from.
    pub parent: Option<ResourceRef>,
    /// What happened to the resource.
    pub action: EventAction,
    /// For `changed`, which field changed and how.
    pub change: Option<Change>,
    /// When it happened. Asana delivers most events within a minute, some much later.
    pub created_at: OffsetDateTime,
}

impl Event {
    /// The GID of the user who caused the event, if a user did.
    #[must_use]
    pub fn actor(&self) -> Option<&UserGid> {
        self.user.as_ref().map(|user| &user.gid)
    }

    /// Whether `user` caused the event; use it to skip the integration's own writes.
    #[must_use]
    pub fn is_by(&self, user: &UserGid) -> bool {
        self.actor() == Some(user)
    }

    /// How long ago the event happened, as of `now`. Negative when `now` is behind Asana's clock.
    #[must_use]
    pub fn age(&self, now: OffsetDateTime) -> time::Duration {
        now - self.created_at
    }

    /// Whether the event happened more than `max_age` before `now`.
    #[must_use]
    pub fn is_older_than(&self, max_age: std::time::Duration, now: OffsetDateTime) -> bool {
        self.age(now) > max_age
    }

    /// A task entered a section: the task was `added` with the section as its parent.
    ///
    /// A move between sections also sends the matching [`section_left`](Self::section_left). The
    /// event does not say which project the section is in, and the task may have moved again
    /// since, so re-read the task's `memberships` before acting.
    #[must_use]
    pub fn section_entered(&self) -> Option<(TaskGid, SectionGid)> {
        self.task_and_parent(&EventAction::Added, |parent| match parent {
            ResourceGid::Section(section) => Some(section.clone()),
            _ => None,
        })
    }

    /// A task left a section: the task was `removed` with the section as its parent.
    #[must_use]
    pub fn section_left(&self) -> Option<(TaskGid, SectionGid)> {
        self.task_and_parent(&EventAction::Removed, |parent| match parent {
            ResourceGid::Section(section) => Some(section.clone()),
            _ => None,
        })
    }

    /// A task was added to a project (created in it, or added to it later).
    #[must_use]
    pub fn task_added_to_project(&self) -> Option<(TaskGid, ProjectGid)> {
        self.task_and_parent(&EventAction::Added, |parent| match parent {
            ResourceGid::Project(project) => Some(project.clone()),
            _ => None,
        })
    }

    /// A task was removed from a project.
    #[must_use]
    pub fn task_removed_from_project(&self) -> Option<(TaskGid, ProjectGid)> {
        self.task_and_parent(&EventAction::Removed, |parent| match parent {
            ResourceGid::Project(project) => Some(project.clone()),
            _ => None,
        })
    }

    /// A field of a task changed: the task and the field's name (`assignee`, `due_on`,
    /// `custom_fields`, `completed`, …).
    #[must_use]
    pub fn task_changed(&self) -> Option<(TaskGid, &str)> {
        match (&self.action, &self.resource.gid, &self.change) {
            (EventAction::Changed, ResourceGid::Task(task), Some(change)) => {
                Some((task.clone(), change.field.as_str()))
            }
            _ => None,
        }
    }

    /// A custom field's value changed on a task: the task and the custom field.
    #[must_use]
    pub fn custom_field_changed(&self) -> Option<(TaskGid, CustomFieldGid)> {
        let (task, field) = self.task_changed()?;
        if field != "custom_fields" {
            return None;
        }
        let change = self.change.as_ref()?;
        let value = change
            .new_value
            .as_ref()
            .or(change.added_value.as_ref())
            .or(change.removed_value.as_ref())?;
        match &value.resource.as_ref()?.gid {
            ResourceGid::CustomField(custom_field) => Some((task, custom_field.clone())),
            _ => None,
        }
    }

    /// A task was deleted.
    #[must_use]
    pub fn task_deleted(&self) -> Option<TaskGid> {
        match (&self.action, &self.resource.gid) {
            (EventAction::Deleted, ResourceGid::Task(task)) => Some(task.clone()),
            _ => None,
        }
    }

    /// A story (a comment, or an activity entry) was added: the story, and the task it is on when
    /// the event names one. Story consolidation means the story may no longer exist when read.
    #[must_use]
    pub fn story_added(&self) -> Option<(StoryGid, Option<TaskGid>)> {
        match (&self.action, &self.resource.gid) {
            (EventAction::Added, ResourceGid::Story(story)) => {
                let task = match self.parent.as_ref().map(|parent| &parent.gid) {
                    Some(ResourceGid::Task(task)) => Some(task.clone()),
                    _ => None,
                };
                Some((story.clone(), task))
            }
            _ => None,
        }
    }

    fn task_and_parent<P>(
        &self,
        action: &EventAction,
        parent: impl FnOnce(&ResourceGid) -> Option<P>,
    ) -> Option<(TaskGid, P)> {
        if &self.action != action {
            return None;
        }
        let ResourceGid::Task(task) = &self.resource.gid else {
            return None;
        };
        Some((task.clone(), parent(&self.parent.as_ref()?.gid)?))
    }
}

impl Serialize for Event {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        parse::event_to_json(self).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Event {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        parse::event_from_json(&value).map_err(serde::de::Error::custom)
    }
}

/// Why one event in a batch could not be read. Names a field, never repeats a value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum UnreadableReason {
    /// The event, or the named part of it, is not a JSON object.
    #[error("`{0}` is not an object")]
    NotAnObject(&'static str),
    /// A field every event carries is missing or `null`.
    #[error("`{0}` is missing")]
    Missing(&'static str),
    /// A field that must be a string is not one.
    #[error("`{0}` is not a string")]
    NotAString(&'static str),
    /// A GID of a known kind is not a valid GID.
    #[error("`{0}` is not a valid GID")]
    InvalidGid(&'static str),
    /// `created_at` is not an RFC 3339 timestamp.
    #[error("`created_at` is not an RFC 3339 timestamp")]
    InvalidTimestamp,
}

/// An event that could not be read, kept so that nothing is dropped silently.
#[derive(Clone, Debug, PartialEq)]
pub struct UnreadableEvent {
    /// Its position in the delivery's `events` array.
    pub index: usize,
    /// The event as Asana sent it.
    pub raw: Value,
    /// What was wrong with it.
    pub reason: UnreadableReason,
}

/// The events of one delivery, in the order Asana sent them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EventBatch {
    /// The events that were read.
    pub events: Vec<Event>,
    /// The events that could not be read, with their positions. Normally empty; log them.
    pub unreadable: Vec<UnreadableEvent>,
}

impl EventBatch {
    /// Whether this is a heartbeat: a delivery with no events, sent when the webhook is created and
    /// every eight hours after. Answer it `200` like any other delivery, or Asana deletes the
    /// webhook after 24 hours without a success.
    #[must_use]
    pub fn is_heartbeat(&self) -> bool {
        self.events.is_empty() && self.unreadable.is_empty()
    }

    pub(crate) fn decode(body: &[u8], limits: &Limits) -> Result<Self, DecodeError> {
        if body.iter().all(u8::is_ascii_whitespace) {
            // Asana describes a heartbeat as an empty payload; `{"events": []}` is handled below.
            return Ok(Self::default());
        }
        let value: Value = serde_json::from_slice(body).map_err(|error| DecodeError::NotJson {
            line: error.line(),
            column: error.column(),
        })?;
        let Value::Object(mut top) = value else {
            return Err(DecodeError::NoEventsArray);
        };
        let Some(Value::Array(raw_events)) = top.remove("events") else {
            return Err(DecodeError::NoEventsArray);
        };
        if raw_events.len() > limits.max_events {
            return Err(DecodeError::TooManyEvents {
                count: raw_events.len(),
                limit: limits.max_events,
            });
        }
        Ok(Self::from_values(raw_events))
    }

    /// Reads events that arrived some other way than a webhook delivery (the events API's `data`
    /// array), keeping each one that cannot be read in `unreadable` with its position, exactly as
    /// a delivery's events are read.
    #[must_use]
    pub fn from_values(raw_events: Vec<Value>) -> Self {
        let mut batch = Self {
            events: Vec::with_capacity(raw_events.len()),
            unreadable: Vec::new(),
        };
        for (index, raw) in raw_events.into_iter().enumerate() {
            match parse::event_from_json(&raw) {
                Ok(event) => batch.events.push(event),
                Err(reason) => batch
                    .unreadable
                    .push(UnreadableEvent { index, raw, reason }),
            }
        }
        batch
    }
}

/// Size limits on what a receiver accepts. `Limits::default()` is 256 KiB and 1,000 events.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Limits {
    /// The longest body accepted, in bytes. Default 256 KiB (262,144).
    pub max_body_bytes: usize,
    /// The most events accepted in one delivery. Default 1,000.
    pub max_events: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_body_bytes: 256 * 1024,
            max_events: 1_000,
        }
    }
}

/// Why a verified delivery could not be decoded. Never repeats any of the body.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum DecodeError {
    /// The body is not JSON.
    #[error("the body is not valid JSON (line {line}, column {column})")]
    NotJson {
        /// The line the parser stopped at.
        line: usize,
        /// The column the parser stopped at.
        column: usize,
    },
    /// The body is JSON but not an object with an `events` array.
    #[error("the body has no `events` array")]
    NoEventsArray,
    /// The delivery holds more events than the receiver accepts.
    #[error("the delivery holds {count} events, over the limit of {limit}")]
    TooManyEvents {
        /// How many events it holds.
        count: usize,
        /// The limit it broke ([`Limits::max_events`]).
        limit: usize,
    },
}

impl DecodeError {
    /// The status a receiver should answer: `413` for too many events, `400` otherwise.
    #[must_use]
    pub fn status_hint(&self) -> u16 {
        match self {
            Self::TooManyEvents { .. } => 413,
            Self::NotJson { .. } | Self::NoEventsArray => 400,
        }
    }
}
