//! The changes the fake makes to tasks, shared by the API handlers, the seeding builders and
//! [`FakeAsana::as_user`](crate::FakeAsana::as_user): placing a task in a project or section,
//! parenting, custom field values, stories and deletion. Every change here emits its events.

use serde_json::Value;

use crate::answer::Refusal;
use crate::changes::Change;
use crate::render::escape;
use crate::select::bare_ref;
use crate::state::{FieldKind, FieldValue, Id, Kind, Membership, State, Story, Task};

/// Where in an ordered list something goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Position {
    Start,
    End,
    Before(Id),
    After(Id),
}

impl Position {
    /// Reads `insert_before` / `insert_after` from a request (`insert_after: null` is the start),
    /// with `default` when neither is given.
    pub(crate) fn read(call: &crate::answer::Call, default: Self) -> Result<Self, Refusal> {
        let before = call.text("insert_before")?;
        let after = call.text("insert_after")?;
        match (before, after) {
            (Some(_), Some(_)) => Err(Refusal::bad_request(
                "insert_before, insert_after: Only one of these may be given",
            )),
            (Some(before), None) => Ok(Self::Before(before)),
            (None, Some(after)) => Ok(Self::After(after)),
            (None, None) if call.has("insert_after") => Ok(Self::Start),
            (None, None) if call.has("insert_before") => Ok(Self::End),
            (None, None) => Ok(default),
        }
    }
}

/// Inserts `item` into `list` at `position`; `param` names the request member for a refusal.
pub(crate) fn insert(
    list: &mut Vec<Id>,
    item: Id,
    position: &Position,
    param: &str,
) -> Result<(), Refusal> {
    let index = match position {
        Position::Start => 0,
        Position::End => list.len(),
        Position::Before(anchor) | Position::After(anchor) => {
            let found = list
                .iter()
                .position(|other| other == anchor)
                .ok_or_else(|| {
                    Refusal::bad_request(format!("{param}: {anchor} is not in the same list"))
                })?;
            if matches!(position, Position::After(_)) {
                found + 1
            } else {
                found
            }
        }
    };
    list.insert(index, item);
    Ok(())
}

/// A validated custom field value: `None` clears the field.
pub(crate) type Assignment = (Id, Option<FieldValue>);

impl State {
    /// A task named `name` in `workspace`, made by the acting user now, in no project.
    pub(crate) fn blank_task(&mut self, workspace: Id, name: String) -> Task {
        let now = self.now();
        let creator = self.acting_user();
        Task {
            name,
            resource_subtype: "default_task".to_owned(),
            notes: String::new(),
            html_notes: None,
            workspace,
            completed: false,
            completed_at: None,
            due_on: None,
            due_at: None,
            start_on: None,
            start_at: None,
            assignee: None,
            created_by: Some(creator.clone()),
            created_at: now,
            modified_at: now,
            memberships: Vec::new(),
            parent: None,
            subtasks: Vec::new(),
            followers: vec![creator],
            tags: Vec::new(),
            dependencies: Vec::new(),
            values: std::collections::BTreeMap::new(),
        }
    }

    /// Puts `task` into `project` (in `section`, or the first section, or where it already is),
    /// at `position` within its section. Emits `added` for a new project and section and
    /// `removed` for the section it left, and records a `section_changed` story for a move.
    pub(crate) fn place(
        &mut self,
        task: &Id,
        project: &Id,
        section: Option<&Id>,
        position: &Position,
    ) -> Result<(), Refusal> {
        let record = crate::state::lookup(&self.projects, "project", project)?;
        if let Some(section) = section {
            let owner = crate::state::lookup(&self.sections, "section", section)?;
            if &owner.project != project {
                return Err(Refusal::bad_request(format!(
                    "section: {section} is not a section of project {project}"
                )));
            }
        }
        let first = record.sections.first().cloned();
        let current = crate::state::lookup(&self.tasks, "task", task)?
            .memberships
            .iter()
            .find(|membership| &membership.project == project)
            .cloned();
        let target = section
            .cloned()
            .or_else(|| current.as_ref().and_then(|current| current.section.clone()))
            .or(first);
        if let Position::Before(anchor) | Position::After(anchor) = position
            && anchor == task
        {
            return Err(Refusal::bad_request(
                "insert_before, insert_after: A task cannot be placed next to itself",
            ));
        }
        // Take the task out of its old place, then put it in the new one; undo on a bad anchor.
        let old_section = current.as_ref().and_then(|current| current.section.clone());
        self.unlist(task, project, old_section.as_ref());
        let inserted = match &target {
            Some(target) => match self.sections.get_mut(target) {
                Some(record) => insert(&mut record.tasks, task.clone(), position, "insert_before"),
                None => Ok(()),
            },
            None => match self.projects.get_mut(project) {
                Some(record) => insert(&mut record.loose, task.clone(), position, "insert_before"),
                None => Ok(()),
            },
        };
        if let Err(refusal) = inserted {
            if current.is_some() {
                self.relist(task, project, old_section.as_ref());
            }
            return Err(refusal);
        }
        let now = self.now();
        if let Some(record) = self.tasks.get_mut(task) {
            record
                .memberships
                .retain(|membership| &membership.project != project);
            record.memberships.push(Membership {
                project: project.clone(),
                section: target.clone(),
            });
            record.modified_at = now;
        }
        if current.is_none() {
            self.emit(Change::added(task, project));
        }
        if old_section != target {
            if let Some(old) = &old_section {
                self.emit(Change::removed(task, old));
            }
            if let Some(new) = &target {
                self.emit(Change::added(task, new));
            }
            if let (Some(old), Some(new)) = (&old_section, &target) {
                self.section_story(task, project, old, new);
            }
        }
        Ok(())
    }

    /// Removes `task` from the list it sits in within `project`.
    fn unlist(&mut self, task: &Id, project: &Id, section: Option<&Id>) {
        match section {
            Some(section) => {
                if let Some(record) = self.sections.get_mut(section) {
                    record.tasks.retain(|other| other != task);
                }
            }
            None => {
                if let Some(record) = self.projects.get_mut(project) {
                    record.loose.retain(|other| other != task);
                }
            }
        }
    }

    /// Puts `task` back at the end of the list it was taken from.
    fn relist(&mut self, task: &Id, project: &Id, section: Option<&Id>) {
        match section {
            Some(section) => {
                if let Some(record) = self.sections.get_mut(section) {
                    record.tasks.push(task.clone());
                }
            }
            None => {
                if let Some(record) = self.projects.get_mut(project) {
                    record.loose.push(task.clone());
                }
            }
        }
    }

    /// Records the system story Asana writes when a task moves between sections.
    fn section_story(&mut self, task: &Id, project: &Id, old: &Id, new: &Id) {
        if self.silent {
            return;
        }
        let name = |gid: &Id| self.name_of(gid).unwrap_or_default().to_owned();
        let text = format!(
            "moved this task from \"{}\" to \"{}\" in {}",
            name(old),
            name(new),
            name(project)
        );
        let story = Story {
            task: task.clone(),
            created_by: Some(self.acting_user()),
            created_at: self.now(),
            resource_subtype: "section_changed".to_owned(),
            text,
            html_text: None,
            pinned: false,
            edited: false,
            old_section: Some(old.clone()),
            new_section: Some(new.clone()),
        };
        self.add_story(story);
    }

    /// Stores a story and emits its `added` event on the task.
    pub(crate) fn add_story(&mut self, story: Story) -> Id {
        let gid = self.new_gid(Kind::Story);
        let task = story.task.clone();
        self.stories.insert(gid.clone(), story);
        self.emit(Change::added(&gid, &task));
        gid
    }

    /// Stores a comment by the acting user.
    pub(crate) fn comment(
        &mut self,
        task: &Id,
        text: Option<String>,
        html_text: Option<String>,
        pinned: bool,
    ) -> Result<Id, Refusal> {
        crate::state::lookup(&self.tasks, "task", task)?;
        let (text, html_text) = match (text, html_text) {
            (_, Some(html)) => (strip_tags(&html), html),
            (Some(text), None) => {
                let html = format!("<body>{}</body>", escape(&text));
                (text, html)
            }
            (None, None) => return Err(Refusal::bad_request("text: Missing input")),
        };
        let story = Story {
            task: task.clone(),
            created_by: Some(self.acting_user()),
            created_at: self.now(),
            resource_subtype: "comment_added".to_owned(),
            text,
            html_text: Some(html_text),
            pinned,
            edited: false,
            old_section: None,
            new_section: None,
        };
        Ok(self.add_story(story))
    }

    /// Takes `task` out of `project`, emitting `removed`.
    pub(crate) fn remove_from_project(&mut self, task: &Id, project: &Id) -> Result<(), Refusal> {
        crate::state::lookup(&self.projects, "project", project)?;
        let record = crate::state::lookup(&self.tasks, "task", task)?;
        let Some(membership) = record
            .memberships
            .iter()
            .find(|membership| &membership.project == project)
            .cloned()
        else {
            return Ok(());
        };
        self.emit(Change::removed(task, project));
        self.unlist(task, project, membership.section.as_ref());
        let now = self.now();
        if let Some(record) = self.tasks.get_mut(task) {
            record.memberships.retain(|other| &other.project != project);
            record.modified_at = now;
        }
        Ok(())
    }

    /// Makes `task` a subtask of `parent` (or of nothing), emitting `removed` from the old parent
    /// and `added` to the new one.
    pub(crate) fn set_parent(
        &mut self,
        task: &Id,
        parent: Option<&Id>,
        position: &Position,
    ) -> Result<(), Refusal> {
        let old = crate::state::lookup(&self.tasks, "task", task)?
            .parent
            .clone();
        if let Some(parent) = parent {
            crate::state::lookup(&self.tasks, "parent", parent)?;
            if self.is_within(parent, task) {
                return Err(Refusal::bad_request(
                    "parent: A task cannot be a subtask of itself or of its own subtask",
                ));
            }
        }
        if let Some(old) = &old
            && let Some(record) = self.tasks.get_mut(old)
        {
            record.subtasks.retain(|other| other != task);
        }
        if let Some(parent) = parent
            && let Some(record) = self.tasks.get_mut(parent)
        {
            insert(
                &mut record.subtasks,
                task.clone(),
                position,
                "insert_before",
            )?;
        }
        let now = self.now();
        if let Some(record) = self.tasks.get_mut(task) {
            record.parent = parent.cloned();
            record.modified_at = now;
        }
        if old.as_ref() != parent {
            if let Some(old) = &old {
                self.emit(Change::removed(task, old));
            }
            if let Some(parent) = parent {
                self.emit(Change::added(task, parent));
            }
        }
        Ok(())
    }

    /// Whether `gid` is `ancestor` or one of its subtasks, at any depth.
    fn is_within(&self, gid: &Id, ancestor: &Id) -> bool {
        let mut current = Some(gid.clone());
        let mut depth = 0;
        while let Some(task) = current {
            if &task == ancestor {
                return true;
            }
            depth += 1;
            if depth > 64 {
                return false;
            }
            current = self.tasks.get(&task).and_then(|task| task.parent.clone());
        }
        false
    }

    /// Deletes `task` and its subtasks, emitting `deleted` for each.
    pub(crate) fn delete_task(&mut self, task: &Id) -> Result<(), Refusal> {
        let record = crate::state::lookup(&self.tasks, "task", task)?.clone();
        for subtask in &record.subtasks {
            self.delete_task(subtask)?;
        }
        self.emit(Change::deleted(task));
        for membership in &record.memberships {
            self.unlist(task, &membership.project, membership.section.as_ref());
        }
        if let Some(parent) = &record.parent
            && let Some(parent) = self.tasks.get_mut(parent)
        {
            parent.subtasks.retain(|other| other != task);
        }
        for other in self.tasks.values_mut() {
            other.dependencies.retain(|dependency| dependency != task);
        }
        self.stories.retain(|_, story| &story.task != task);
        self.tasks.remove(task);
        Ok(())
    }

    /// Checks a custom field value from a request against the field's kind and options, and
    /// against whether the field is on `task` (`fields` are the fields it will have).
    pub(crate) fn check_value(
        &self,
        fields: &[Id],
        field: &str,
        raw: &Value,
    ) -> Result<Assignment, Refusal> {
        let definition = crate::state::lookup(&self.fields, "custom_fields", field)?;
        if !fields.iter().any(|on| on == field) {
            return Err(Refusal::bad_request(format!(
                "custom_fields: Custom field with ID {field} is not on given object"
            )));
        }
        let wrong = |expected: &str| {
            Refusal::bad_request(format!(
                "custom_fields: The value for custom field {field} must be {expected}"
            ))
        };
        if raw.is_null() {
            return Ok((field.to_owned(), None));
        }
        let value = match definition.kind {
            FieldKind::Text => {
                FieldValue::Text(raw.as_str().ok_or_else(|| wrong("a string"))?.to_owned())
            }
            FieldKind::Number => match raw {
                Value::Number(number) => FieldValue::Number(number.clone()),
                Value::String(text) => FieldValue::Number(
                    text.parse::<serde_json::Number>()
                        .map_err(|_| wrong("a number"))?,
                ),
                _ => return Err(wrong("a number")),
            },
            FieldKind::Enum => {
                let option = raw.as_str().ok_or_else(|| wrong("an enum option's GID"))?;
                self.check_option(field, option)?;
                FieldValue::Enum(option.to_owned())
            }
            FieldKind::MultiEnum => {
                let options = gid_list(raw).ok_or_else(|| wrong("a list of enum option GIDs"))?;
                for option in &options {
                    self.check_option(field, option)?;
                }
                if options.is_empty() {
                    return Ok((field.to_owned(), None));
                }
                FieldValue::MultiEnum(options)
            }
            FieldKind::Date => read_date(raw)
                .ok_or_else(|| wrong("{\"date\": \"YYYY-MM-DD\"} or {\"date_time\": …}"))?,
            FieldKind::People => {
                let people = gid_list(raw).ok_or_else(|| wrong("a list of user GIDs"))?;
                for person in &people {
                    crate::state::lookup(&self.users, "custom_fields", person)?;
                }
                if people.is_empty() {
                    return Ok((field.to_owned(), None));
                }
                FieldValue::People(people)
            }
            FieldKind::Reference => {
                let references = gid_list(raw).ok_or_else(|| wrong("a list of GIDs"))?;
                for reference in &references {
                    if !self.exists(reference) {
                        return Err(Refusal::unknown("custom_fields", reference));
                    }
                }
                if references.is_empty() {
                    return Ok((field.to_owned(), None));
                }
                FieldValue::Reference(references)
            }
        };
        Ok((field.to_owned(), Some(value)))
    }

    fn check_option(&self, field: &str, option: &str) -> Result<(), Refusal> {
        let record = crate::state::lookup(&self.options, "custom_fields", option)?;
        if record.field != field {
            return Err(Refusal::bad_request(format!(
                "custom_fields: Enum option {option} does not belong to custom field {field}"
            )));
        }
        if !record.enabled {
            return Err(Refusal::bad_request(format!(
                "custom_fields: Enum option {option} is disabled"
            )));
        }
        Ok(())
    }

    /// Reads a request's `custom_fields` object into checked assignments.
    pub(crate) fn check_values(
        &self,
        fields: &[Id],
        raw: Option<&Value>,
    ) -> Result<Vec<Assignment>, Refusal> {
        match raw {
            None | Some(Value::Null) => Ok(Vec::new()),
            Some(Value::Object(values)) => values
                .iter()
                .map(|(field, value)| self.check_value(fields, field, value))
                .collect(),
            Some(_) => Err(Refusal::bad_request("custom_fields: Not an object")),
        }
    }

    /// Stores checked custom field values on `task`, emitting a `changed` event for each value
    /// that differs.
    pub(crate) fn assign(&mut self, task: &Id, assignments: Vec<Assignment>) {
        for (field, value) in assignments {
            let changed = match self.tasks.get_mut(task) {
                Some(record) => {
                    let before = record.values.get(&field).cloned();
                    match &value {
                        Some(value) => record.values.insert(field.clone(), value.clone()),
                        None => record.values.remove(&field),
                    };
                    before != value
                }
                None => false,
            };
            if changed {
                let new_value = self.field_change_value(task, &field);
                self.emit(Change::changed(task, "custom_fields", Some(new_value)));
            }
        }
    }

    /// The `new_value` of a custom field change: the field, with its new value.
    fn field_change_value(&self, task: &Id, field: &Id) -> Value {
        let mut value = match bare_ref(self, field) {
            Value::Object(value) => value,
            _ => serde_json::Map::new(),
        };
        let kind = self.fields.get(field).map(|field| field.kind);
        if let Some(kind) = kind {
            value.insert(
                "resource_subtype".into(),
                Value::String(kind.subtype().to_owned()),
            );
        }
        let current = self.tasks.get(task).and_then(|task| task.values.get(field));
        let (key, member) = match current {
            Some(FieldValue::Text(text)) => ("text_value", Value::String(text.clone())),
            Some(FieldValue::Number(number)) => ("number_value", Value::Number(number.clone())),
            Some(FieldValue::Enum(option)) => ("enum_value", bare_ref(self, option)),
            Some(FieldValue::MultiEnum(options)) => (
                "multi_enum_values",
                Value::Array(
                    options
                        .iter()
                        .map(|option| bare_ref(self, option))
                        .collect(),
                ),
            ),
            Some(FieldValue::Date { date, date_time }) => (
                "date_value",
                serde_json::json!({ "date": date, "date_time": date_time }),
            ),
            Some(FieldValue::People(people)) => (
                "people_value",
                Value::Array(people.iter().map(|person| bare_ref(self, person)).collect()),
            ),
            Some(FieldValue::Reference(references)) => (
                "reference_value",
                Value::Array(references.iter().map(|gid| bare_ref(self, gid)).collect()),
            ),
            None => match kind {
                Some(FieldKind::Number) => ("number_value", Value::Null),
                Some(FieldKind::Enum) => ("enum_value", Value::Null),
                Some(FieldKind::Date) => ("date_value", Value::Null),
                _ => ("text_value", Value::Null),
            },
        };
        value.insert(key.into(), member);
        Value::Object(value)
    }

    /// The fields `task` would show if it were in `projects` (and its own values).
    pub(crate) fn fields_for(&self, task: Option<&Task>, projects: &[Id]) -> Vec<Id> {
        let mut fields = task
            .map(|task| self.fields_on_task(task))
            .unwrap_or_default();
        for project in projects {
            if let Some(project) = self.projects.get(project) {
                for setting in &project.settings {
                    if let Some(setting) = self.settings.get(setting)
                        && !fields.contains(&setting.field)
                    {
                        fields.push(setting.field.clone());
                    }
                }
            }
        }
        fields
    }
}

/// An array of GIDs (strings, or objects with a `gid`).
fn gid_list(raw: &Value) -> Option<Vec<Id>> {
    match raw {
        Value::Array(items) => items
            .iter()
            .map(|item| match item {
                Value::String(gid) => Some(gid.clone()),
                Value::Object(object) => object.get("gid")?.as_str().map(str::to_owned),
                _ => None,
            })
            .collect(),
        Value::String(gid) => Some(vec![gid.clone()]),
        _ => None,
    }
}

/// A date value: `{"date": "YYYY-MM-DD"}`, `{"date_time": "…"}`, or a bare `"YYYY-MM-DD"`.
fn read_date(raw: &Value) -> Option<FieldValue> {
    let valid_date = |text: &str| {
        time::Date::parse(
            text,
            time::macros::format_description!("[year]-[month]-[day]"),
        )
        .is_ok()
    };
    match raw {
        Value::String(date) if valid_date(date) => Some(FieldValue::Date {
            date: date.clone(),
            date_time: None,
        }),
        Value::Object(object) => {
            if let Some(date_time) = object.get("date_time").and_then(Value::as_str) {
                let parsed = time::OffsetDateTime::parse(
                    date_time,
                    &time::format_description::well_known::Rfc3339,
                )
                .ok()?;
                let date = parsed.to_offset(time::UtcOffset::UTC).date().to_string();
                return Some(FieldValue::Date {
                    date,
                    date_time: Some(date_time.to_owned()),
                });
            }
            let date = object.get("date")?.as_str()?;
            valid_date(date).then(|| FieldValue::Date {
                date: date.to_owned(),
                date_time: None,
            })
        }
        _ => None,
    }
}

/// The plain text of a rich-text body: tags dropped, the common entities decoded.
pub(crate) fn strip_tags(html: &str) -> String {
    let mut text = String::with_capacity(html.len());
    let mut in_tag = false;
    for character in html.chars() {
        match character {
            '<' => in_tag = true,
            '>' => in_tag = false,
            other if !in_tag => text.push(other),
            _ => {}
        }
    }
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}
