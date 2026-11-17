//! Turning a change into events, for every webhook and events stream that can see it.
//!
//! Each change the fake makes through the API (or through [`FakeAsana::as_user`]) becomes one
//! compact event shaped as Asana's documentation shows it: `user`, `created_at`, `action`,
//! `resource`, `parent` and `change`. It is queued on every webhook whose resource is the changed
//! object or contains it, and whose filters it passes, and appended to every events stream on such
//! a resource. Seeding makes no events.
//!
//! What contains what follows Asana's rules: a project's webhook sees its sections, its tasks,
//! their subtasks and their stories; a task's webhook sees its subtasks and stories; a workspace's
//! webhook sees projects, tags and custom fields but never tasks, subtasks or stories.
//!
//! [`FakeAsana::as_user`]: crate::FakeAsana::as_user

use serde_json::{Map, Value, json};

use crate::answer::stamp;
use crate::select::bare_ref;
use crate::state::{Id, Kind, State};

/// One change, before it is written as an event.
pub(crate) struct Change<'a> {
    action: &'static str,
    resource: &'a str,
    parent: Option<&'a str>,
    change: Option<Value>,
}

impl<'a> Change<'a> {
    /// `added`: `resource` was added to `parent` (a task to a project, section or parent task; a
    /// story to a task).
    pub(crate) fn added(resource: &'a str, parent: &'a str) -> Self {
        Self {
            action: "added",
            resource,
            parent: Some(parent),
            change: None,
        }
    }

    /// `removed`: `resource` was removed from `parent`.
    pub(crate) fn removed(resource: &'a str, parent: &'a str) -> Self {
        Self {
            action: "removed",
            resource,
            parent: Some(parent),
            change: None,
        }
    }

    /// `deleted`: `resource` was deleted.
    pub(crate) fn deleted(resource: &'a str) -> Self {
        Self {
            action: "deleted",
            resource,
            parent: None,
            change: None,
        }
    }

    /// `changed`: a field of `resource` was replaced.
    pub(crate) fn changed(resource: &'a str, field: &str, new_value: Option<Value>) -> Self {
        let mut change = Map::new();
        change.insert("field".into(), Value::String(field.to_owned()));
        change.insert("action".into(), Value::String("changed".to_owned()));
        if let Some(value) = new_value {
            change.insert("new_value".into(), value);
        }
        Self {
            action: "changed",
            resource,
            parent: None,
            change: Some(Value::Object(change)),
        }
    }

    /// `changed`: `value` was added to or removed from a list field of `resource`.
    pub(crate) fn listed(resource: &'a str, field: &str, added: bool, value: Value) -> Self {
        let (action, key) = if added {
            ("added", "added_value")
        } else {
            ("removed", "removed_value")
        };
        Self {
            action: "changed",
            resource,
            parent: None,
            change: Some(json!({ "field": field, "action": action, key: value })),
        }
    }
}

impl State {
    /// Queues `change` as an event for every webhook and stream that sees it. Call it before
    /// deleting the changed object, so what contains it is still known.
    pub(crate) fn emit(&mut self, change: Change<'_>) {
        if self.silent {
            return;
        }
        let event = self.event(&change);
        let scopes = self.scopes(change.resource);
        for webhook in self.webhooks.values_mut() {
            if scopes.contains(&webhook.resource) && passes(&webhook.filters, &event) {
                webhook.pending.push_back(event.clone());
            }
        }
        for stream in &mut self.streams {
            if scopes.contains(&stream.resource) {
                stream.events.push(event.clone());
            }
        }
    }

    fn event(&mut self, change: &Change<'_>) -> Value {
        let user = self.acting_user();
        let mut actor = Map::new();
        actor.insert("gid".into(), Value::String(user.clone()));
        actor.insert("resource_type".into(), Value::String("user".to_owned()));
        if let Some(name) = self.name_of(&user) {
            actor.insert("name".into(), Value::String(name.to_owned()));
        }
        let mut resource = match bare_ref(self, change.resource) {
            Value::Object(resource) => resource,
            _ => Map::new(),
        };
        if let Some(subtype) = self.subtype_of(change.resource) {
            resource.insert("resource_subtype".into(), Value::String(subtype));
        }
        if let Some(name) = self.name_of(change.resource) {
            resource.insert("name".into(), Value::String(name.to_owned()));
        }
        json!({
            "user": Value::Object(actor),
            "created_at": stamp(self.now()),
            "action": change.action,
            "resource": Value::Object(resource),
            "parent": change.parent.map_or(Value::Null, |parent| bare_ref(self, parent)),
            "change": change.change.clone().unwrap_or(Value::Null),
        })
    }

    fn subtype_of(&self, gid: &str) -> Option<String> {
        match self.kind_of(gid)? {
            Kind::Task => self
                .tasks
                .get(gid)
                .map(|task| task.resource_subtype.clone()),
            Kind::Story => self
                .stories
                .get(gid)
                .map(|story| story.resource_subtype.clone()),
            Kind::Project => Some("default".to_owned()),
            Kind::Section => Some("default_section".to_owned()),
            _ => None,
        }
    }

    /// Every GID whose webhook can see a change to `gid`: itself and what contains it.
    pub(crate) fn scopes(&self, gid: &str) -> Vec<Id> {
        let mut scopes = vec![gid.to_owned()];
        match self.kind_of(gid) {
            Some(Kind::Task) => self.task_scopes(gid, &mut scopes),
            Some(Kind::Story) => {
                if let Some(story) = self.stories.get(gid) {
                    scopes.push(story.task.clone());
                    self.task_scopes(&story.task, &mut scopes);
                }
            }
            Some(Kind::Attachment) => {
                if let Some(attachment) = self.attachments.get(gid) {
                    scopes.push(attachment.parent.clone());
                    if self.kind_of(&attachment.parent) == Some(Kind::Task) {
                        self.task_scopes(&attachment.parent, &mut scopes);
                    }
                }
            }
            Some(Kind::Section) => {
                if let Some(section) = self.sections.get(gid) {
                    scopes.push(section.project.clone());
                }
            }
            Some(Kind::Project | Kind::Tag | Kind::CustomField) => {
                if let Some(workspace) = self.workspace_of(gid) {
                    scopes.push(workspace);
                }
            }
            _ => {}
        }
        scopes
    }

    /// A task's projects and sections, and its ancestors' (a project sees its subtasks).
    fn task_scopes(&self, gid: &str, scopes: &mut Vec<Id>) {
        let mut current = self.tasks.get(gid);
        let mut depth = 0;
        while let Some(task) = current {
            for membership in &task.memberships {
                scopes.push(membership.project.clone());
                if let Some(section) = &membership.section {
                    scopes.push(section.clone());
                }
            }
            depth += 1;
            current = match &task.parent {
                Some(parent) if depth < 32 => {
                    scopes.push(parent.clone());
                    self.tasks.get(parent)
                }
                _ => None,
            };
        }
    }
}

/// Whether `event` passes a webhook's filters: any one of them, or all events when there are
/// none.
pub(crate) fn passes(filters: &[Value], event: &Value) -> bool {
    filters.is_empty() || filters.iter().any(|filter| matches(filter, event))
}

fn matches(filter: &Value, event: &Value) -> bool {
    let wanted = |key: &str| filter.get(key).and_then(Value::as_str);
    let resource = event.get("resource");
    let member = |key: &str| {
        resource
            .and_then(|resource| resource.get(key))
            .and_then(Value::as_str)
    };
    if wanted("resource_type").is_some_and(|kind| member("resource_type") != Some(kind)) {
        return false;
    }
    if wanted("resource_subtype").is_some_and(|kind| member("resource_subtype") != Some(kind)) {
        return false;
    }
    if wanted("action")
        .is_some_and(|action| event.get("action").and_then(Value::as_str) != Some(action))
    {
        return false;
    }
    if let Some(fields) = filter.get("fields").and_then(Value::as_array)
        && !fields.is_empty()
    {
        let field = event
            .get("change")
            .and_then(|change| change.get("field"))
            .and_then(Value::as_str);
        return field.is_some_and(|field| fields.iter().any(|wanted| wanted == field));
    }
    true
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::passes;

    fn event(action: &str, field: Option<&str>) -> serde_json::Value {
        json!({
            "action": action,
            "resource": { "gid": "1200000000000001", "resource_type": "task", "resource_subtype": "default_task" },
            "change": field.map(|field| json!({ "field": field, "action": "changed" })),
        })
    }

    #[test]
    fn no_filters_pass_everything_and_any_one_filter_is_enough() {
        assert!(passes(&[], &event("added", None)));
        let filters = [
            json!({ "resource_type": "section" }),
            json!({ "resource_type": "task", "action": "added" }),
        ];
        assert!(passes(&filters, &event("added", None)));
        assert!(!passes(&filters, &event("removed", None)));
    }

    #[test]
    fn a_fields_filter_keeps_only_changes_to_those_fields() {
        let filters =
            [json!({ "resource_type": "task", "action": "changed", "fields": ["completed"] })];
        assert!(passes(&filters, &event("changed", Some("completed"))));
        assert!(!passes(&filters, &event("changed", Some("name"))));
    }
}
