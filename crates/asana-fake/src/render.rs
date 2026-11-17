//! Each record described as [`Node`]s, with the members Asana's full record has.

use serde_json::{Value, json};

use crate::answer::{stamp, stamp_value};
use crate::select::{Fields, Node};
use crate::state::{Content, FieldKind, FieldValue, Id, JobStatus, Kind, State, Task};

/// The host the fake serves attachment downloads from.
pub(crate) const FILES_HOST: &str = "files.asana-fake.example";

/// Where an attachment's bytes are served.
pub(crate) fn download_url(gid: &str) -> String {
    format!("https://{FILES_HOST}/attachments/{gid}")
}

/// Every member of the record `gid` names, or `None` when it does not exist.
pub(crate) fn record(state: &State, gid: &str) -> Option<Fields> {
    let kind = state.kind_of(gid)?;
    let mut fields: Fields = vec![
        ("gid", Node::text(gid)),
        ("resource_type", Node::text(kind.resource_type())),
    ];
    match kind {
        Kind::Workspace => workspace_fields(state, gid, &mut fields)?,
        Kind::User => user_fields(state, gid, &mut fields)?,
        Kind::Project => project_fields(state, gid, &mut fields)?,
        Kind::Section => section_fields(state, gid, &mut fields)?,
        Kind::Task => task_fields(state, gid, &mut fields)?,
        Kind::CustomField => fields.extend(field(state, gid)?),
        Kind::EnumOption => enum_option_fields(state, gid, &mut fields)?,
        Kind::CustomFieldSetting => custom_field_setting_fields(state, gid, &mut fields)?,
        Kind::Tag => tag_fields(state, gid, &mut fields)?,
        Kind::Story => story_fields(state, gid, &mut fields)?,
        Kind::Attachment => attachment_fields(state, gid, &mut fields)?,
        Kind::TaskTemplate => task_template_fields(state, gid, &mut fields)?,
        Kind::Job => job_fields(state, gid, &mut fields)?,
        Kind::Webhook => webhook_fields(state, gid, &mut fields)?,
    }
    Some(fields)
}

fn workspace_fields(state: &State, gid: &str, fields: &mut Fields) -> Option<()> {
    let workspace = state.workspaces.get(gid)?;
    fields.extend([
        ("name", Node::text(&workspace.name)),
        ("is_organization", Node::Value(Value::Bool(false))),
        ("email_domains", Node::Value(json!(["example.com"]))),
    ]);
    Some(())
}

fn user_fields(state: &State, gid: &str, fields: &mut Fields) -> Option<()> {
    let user = state.users.get(gid)?;
    fields.extend([
        ("name", Node::text(&user.name)),
        ("email", Node::text(&user.email)),
        ("photo", Node::Value(Value::Null)),
        ("workspaces", Node::refs(&user.workspaces)),
    ]);
    Some(())
}

fn section_fields(state: &State, gid: &str, fields: &mut Fields) -> Option<()> {
    let section = state.sections.get(gid)?;
    fields.extend([
        ("name", Node::text(&section.name)),
        ("created_at", Node::text(stamp(section.created_at))),
        ("project", Node::Ref(section.project.clone())),
        ("projects", Node::refs([&section.project])),
    ]);
    Some(())
}

fn enum_option_fields(state: &State, gid: &str, fields: &mut Fields) -> Option<()> {
    let option = state.options.get(gid)?;
    fields.extend([
        ("name", Node::text(&option.name)),
        ("enabled", Node::Value(Value::Bool(option.enabled))),
        ("color", Node::text(&option.color)),
    ]);
    Some(())
}

fn custom_field_setting_fields(state: &State, gid: &str, fields: &mut Fields) -> Option<()> {
    let setting = state.settings.get(gid)?;
    let mut custom_field = vec![
        ("gid", Node::text(&setting.field)),
        ("resource_type", Node::text("custom_field")),
    ];
    custom_field.extend(field(state, &setting.field)?);
    fields.extend([
        ("custom_field", Node::Object(custom_field)),
        ("project", Node::Ref(setting.project.clone())),
        ("parent", Node::Ref(setting.project.clone())),
        ("is_important", Node::Value(Value::Bool(setting.important))),
    ]);
    Some(())
}

fn tag_fields(state: &State, gid: &str, fields: &mut Fields) -> Option<()> {
    let tag = state.tags.get(gid)?;
    fields.extend([
        ("name", Node::text(&tag.name)),
        ("color", Node::maybe_text(tag.color.as_ref())),
        ("notes", Node::text(&tag.notes)),
        ("created_at", Node::text(stamp(tag.created_at))),
        ("followers", Node::List(Vec::new())),
        ("workspace", Node::Ref(tag.workspace.clone())),
        (
            "permalink_url",
            Node::text(format!("https://app.asana.com/0/{gid}/list")),
        ),
    ]);
    Some(())
}

fn story_fields(state: &State, gid: &str, fields: &mut Fields) -> Option<()> {
    let story = state.stories.get(gid)?;
    let comment = story.is_comment();
    fields.extend([
        ("created_at", Node::text(stamp(story.created_at))),
        ("created_by", Node::maybe_ref(story.created_by.as_ref())),
        ("resource_subtype", Node::text(&story.resource_subtype)),
        ("text", Node::text(&story.text)),
        (
            "type",
            Node::text(if comment { "comment" } else { "system" }),
        ),
        ("is_pinned", Node::Value(Value::Bool(story.pinned))),
        ("is_edited", Node::Value(Value::Bool(story.edited))),
        ("source", Node::text("api")),
        ("target", Node::Ref(story.task.clone())),
    ]);
    if comment {
        fields.push(("html_text", Node::maybe_text(story.html_text.as_ref())));
    }
    if story.resource_subtype == "section_changed" {
        fields.push(("old_section", Node::maybe_ref(story.old_section.as_ref())));
        fields.push(("new_section", Node::maybe_ref(story.new_section.as_ref())));
    }
    Some(())
}

fn attachment_fields(state: &State, gid: &str, fields: &mut Fields) -> Option<()> {
    let attachment = state.attachments.get(gid)?;
    let (subtype, host, download, size) = match &attachment.content {
        Content::File { bytes, .. } => (
            "asana",
            "asana",
            Value::String(download_url(gid)),
            Value::from(bytes.len()),
        ),
        Content::External { .. } => ("external", "external", Value::Null, Value::Null),
    };
    let view = match &attachment.content {
        Content::File { .. } => Value::String(download_url(gid)),
        Content::External { url } => Value::String(url.clone()),
    };
    fields.extend([
        ("name", Node::text(&attachment.name)),
        ("resource_subtype", Node::text(subtype)),
        ("created_at", Node::text(stamp(attachment.created_at))),
        ("download_url", Node::Value(download)),
        ("permanent_url", Node::Value(view.clone())),
        ("view_url", Node::Value(view)),
        ("host", Node::text(host)),
        ("size", Node::Value(size)),
        ("connected_to_app", Node::Value(Value::Bool(false))),
        ("parent", Node::Ref(attachment.parent.clone())),
    ]);
    Some(())
}

fn task_template_fields(state: &State, gid: &str, fields: &mut Fields) -> Option<()> {
    let template = state.templates.get(gid)?;
    fields.extend([
        ("name", Node::text(&template.name)),
        ("project", Node::Ref(template.project.clone())),
        ("created_at", Node::text(stamp(template.created_at))),
        ("created_by", Node::maybe_ref(template.created_by.as_ref())),
        (
            "template",
            Node::Object(vec![
                ("name", Node::text(&template.task_name)),
                ("description", Node::text(&template.description)),
                ("task_resource_subtype", Node::text("default_task")),
                ("memberships", Node::refs([&template.project])),
            ]),
        ),
    ]);
    Some(())
}

fn job_fields(state: &State, gid: &str, fields: &mut Fields) -> Option<()> {
    let job = state.jobs.get(gid)?;
    let status = match job.status {
        JobStatus::InProgress => "in_progress",
        JobStatus::Succeeded => "succeeded",
    };
    fields.extend([
        ("resource_subtype", Node::text(job.subtype)),
        ("status", Node::text(status)),
        ("new_task", Node::maybe_ref(job.new_task.as_ref())),
        ("new_project", Node::Value(Value::Null)),
        ("new_project_template", Node::Value(Value::Null)),
    ]);
    Some(())
}

fn webhook_fields(state: &State, gid: &str, fields: &mut Fields) -> Option<()> {
    let webhook = state.webhooks.get(gid)?;
    fields.extend([
        ("active", Node::Value(Value::Bool(true))),
        ("resource", Node::Ref(webhook.resource.clone())),
        ("target", Node::text(&webhook.target)),
        ("created_at", Node::text(stamp(webhook.created_at))),
        (
            "filters",
            Node::Value(Value::Array(webhook.filters.clone())),
        ),
        (
            "last_success_at",
            Node::Value(stamp_value(webhook.last_success_at)),
        ),
        (
            "last_failure_at",
            Node::Value(stamp_value(webhook.last_failure_at)),
        ),
        (
            "last_failure_content",
            Node::maybe_text(webhook.last_failure_content.as_ref()),
        ),
        (
            "delivery_retry_count",
            Node::Value(Value::from(webhook.delivery_retry_count)),
        ),
        ("next_attempt_after", Node::Value(Value::Null)),
        (
            "failure_deletion_timestamp",
            Node::Value(stamp_value(
                webhook
                    .last_failure_at
                    .filter(|_| webhook.delivery_retry_count > 0)
                    .map(|failed| failed + time::Duration::hours(24)),
            )),
        ),
    ]);
    Some(())
}

fn project_fields(state: &State, gid: &str, fields: &mut Fields) -> Option<()> {
    let project = state.projects.get(gid)?;
    let settings: Vec<Node> = project
        .settings
        .iter()
        .filter_map(|setting| record(state, setting).map(Node::Object))
        .collect();
    let members = state
        .workspaces
        .get(&project.workspace)
        .map(|workspace| Node::refs(&workspace.users))
        .unwrap_or(Node::List(Vec::new()));
    fields.extend([
        ("name", Node::text(&project.name)),
        ("archived", Node::Value(Value::Bool(project.archived))),
        ("color", Node::Value(Value::Null)),
        ("completed", Node::Value(Value::Bool(false))),
        ("created_at", Node::text(stamp(project.created_at))),
        ("modified_at", Node::text(stamp(project.modified_at))),
        ("notes", Node::text(&project.notes)),
        ("public", Node::Value(Value::Bool(true))),
        ("default_view", Node::text("list")),
        ("due_on", Node::Value(Value::Null)),
        ("start_on", Node::Value(Value::Null)),
        ("current_status", Node::Value(Value::Null)),
        ("workspace", Node::Ref(project.workspace.clone())),
        ("team", Node::Value(Value::Null)),
        ("owner", Node::maybe_ref(state.me.as_ref())),
        ("members", members),
        ("followers", Node::List(Vec::new())),
        ("custom_field_settings", Node::List(settings)),
        (
            "permalink_url",
            Node::text(format!("https://app.asana.com/0/{gid}/list")),
        ),
    ]);
    Some(())
}

fn task_fields(state: &State, gid: &str, fields: &mut Fields) -> Option<()> {
    let task = state.tasks.get(gid)?;
    let memberships = task
        .memberships
        .iter()
        .map(|membership| {
            Node::Object(vec![
                ("project", Node::Ref(membership.project.clone())),
                ("section", Node::maybe_ref(membership.section.as_ref())),
            ])
        })
        .collect();
    let dependents: Vec<&Id> = state
        .tasks
        .iter()
        .filter(|(_, other)| {
            other
                .dependencies
                .iter()
                .any(|dependency| dependency == gid)
        })
        .map(|(other, _)| other)
        .collect();
    let custom_fields = state
        .fields_on_task(task)
        .iter()
        .filter_map(|field| field_entry(state, task, field))
        .collect();
    fields.extend([
        ("name", Node::text(&task.name)),
        ("resource_subtype", Node::text(&task.resource_subtype)),
        ("notes", Node::text(&task.notes)),
        (
            "html_notes",
            Node::text(
                task.html_notes
                    .clone()
                    .unwrap_or_else(|| format!("<body>{}</body>", escape(&task.notes))),
            ),
        ),
        ("completed", Node::Value(Value::Bool(task.completed))),
        ("completed_at", Node::Value(stamp_value(task.completed_at))),
        ("created_at", Node::text(stamp(task.created_at))),
        ("modified_at", Node::text(stamp(task.modified_at))),
        ("created_by", Node::maybe_ref(task.created_by.as_ref())),
        ("due_on", Node::maybe_text(task.due_on.as_ref())),
        ("due_at", Node::maybe_text(task.due_at.as_ref())),
        ("start_on", Node::maybe_text(task.start_on.as_ref())),
        ("start_at", Node::maybe_text(task.start_at.as_ref())),
        ("assignee", Node::maybe_ref(task.assignee.as_ref())),
        ("followers", Node::refs(&task.followers)),
        ("tags", Node::refs(&task.tags)),
        (
            "projects",
            Node::refs(
                task.memberships
                    .iter()
                    .map(|membership| &membership.project),
            ),
        ),
        ("memberships", Node::List(memberships)),
        ("parent", Node::maybe_ref(task.parent.as_ref())),
        (
            "num_subtasks",
            Node::Value(Value::from(task.subtasks.len())),
        ),
        ("dependencies", Node::refs(&task.dependencies)),
        ("dependents", Node::refs(dependents)),
        ("workspace", Node::Ref(task.workspace.clone())),
        ("custom_fields", Node::List(custom_fields)),
        ("liked", Node::Value(Value::Bool(false))),
        ("num_likes", Node::Value(Value::from(0))),
        ("likes", Node::List(Vec::new())),
        (
            "permalink_url",
            Node::text(format!("https://app.asana.com/0/0/{gid}")),
        ),
    ]);
    Some(())
}

/// A custom field's definition members (everything but `gid` and `resource_type`).
fn field(state: &State, gid: &str) -> Option<Fields> {
    let field = state.fields.get(gid)?;
    let mut fields = vec![
        ("name", Node::text(&field.name)),
        ("resource_subtype", Node::text(field.kind.subtype())),
        ("type", Node::text(field.kind.subtype())),
        ("description", Node::text(&field.description)),
        ("enabled", Node::Value(Value::Bool(true))),
        ("is_global_to_workspace", Node::Value(Value::Bool(true))),
        ("has_notifications_enabled", Node::Value(Value::Bool(false))),
        ("workspace", Node::Ref(field.workspace.clone())),
    ];
    match field.kind {
        FieldKind::Number => {
            fields.push(("precision", Node::Value(Value::from(field.precision))));
            fields.push(("format", Node::text("custom")));
        }
        FieldKind::Enum | FieldKind::MultiEnum => {
            let options = field
                .options
                .iter()
                .filter_map(|option| embedded(state, option))
                .collect();
            fields.push(("enum_options", Node::List(options)));
        }
        _ => {}
    }
    Some(fields)
}

/// A record written in full where Asana embeds it (enum options).
fn embedded(state: &State, gid: &str) -> Option<Node> {
    record(state, gid).map(Node::Object)
}

/// One entry of a task's `custom_fields`: the field's definition and the task's value.
fn field_entry(state: &State, task: &Task, gid: &Id) -> Option<Node> {
    let definition = state.fields.get(gid)?;
    let mut fields = vec![
        ("gid", Node::text(gid)),
        ("resource_type", Node::text("custom_field")),
    ];
    fields.extend(field(state, gid)?);
    let value = task.values.get(gid);
    let display = value.and_then(|value| display_value(state, definition.precision, value));
    let (key, node) = match (definition.kind, value) {
        (FieldKind::Text, Some(FieldValue::Text(text))) => ("text_value", Node::text(text)),
        (FieldKind::Text, _) => ("text_value", Node::Value(Value::Null)),
        (FieldKind::Number, Some(FieldValue::Number(number))) => {
            ("number_value", Node::Value(Value::Number(number.clone())))
        }
        (FieldKind::Number, _) => ("number_value", Node::Value(Value::Null)),
        (FieldKind::Enum, Some(FieldValue::Enum(option))) => (
            "enum_value",
            embedded(state, option).unwrap_or(Node::Value(Value::Null)),
        ),
        (FieldKind::Enum, _) => ("enum_value", Node::Value(Value::Null)),
        (FieldKind::MultiEnum, Some(FieldValue::MultiEnum(options))) => (
            "multi_enum_values",
            Node::List(
                options
                    .iter()
                    .filter_map(|option| embedded(state, option))
                    .collect(),
            ),
        ),
        (FieldKind::MultiEnum, _) => ("multi_enum_values", Node::List(Vec::new())),
        (FieldKind::Date, Some(FieldValue::Date { date, date_time })) => (
            "date_value",
            Node::Value(json!({ "date": date, "date_time": date_time })),
        ),
        (FieldKind::Date, _) => ("date_value", Node::Value(Value::Null)),
        (FieldKind::People, Some(FieldValue::People(people))) => {
            ("people_value", Node::refs(people))
        }
        (FieldKind::People, _) => ("people_value", Node::List(Vec::new())),
        (FieldKind::Reference, Some(FieldValue::Reference(references))) => {
            ("reference_value", Node::refs(references))
        }
        (FieldKind::Reference, _) => ("reference_value", Node::List(Vec::new())),
    };
    fields.push((key, node));
    fields.push(("display_value", Node::maybe_text(display.as_ref())));
    Some(Node::Object(fields))
}

/// The text Asana shows for a value.
fn display_value(state: &State, precision: u32, value: &FieldValue) -> Option<String> {
    let names = |gids: &[Id]| {
        gids.iter()
            .filter_map(|gid| state.name_of(gid))
            .collect::<Vec<_>>()
            .join(", ")
    };
    Some(match value {
        FieldValue::Text(text) => text.clone(),
        FieldValue::Number(number) => {
            let precision = usize::try_from(precision).unwrap_or(0);
            match number.as_f64() {
                Some(number) => format!("{number:.precision$}"),
                None => number.to_string(),
            }
        }
        FieldValue::Enum(option) => state.name_of(option)?.to_owned(),
        FieldValue::MultiEnum(options) => names(options),
        FieldValue::Date { date, date_time } => date_time.clone().unwrap_or_else(|| date.clone()),
        FieldValue::People(people) => names(people),
        FieldValue::Reference(references) => names(references),
    })
}

/// Escapes text for Asana's rich-text bodies.
pub(crate) fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
