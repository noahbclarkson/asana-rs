//! Acting in the fake as a person would in Asana's web app, so a test can cause the changes its
//! code is meant to react to.
//!
//! Unlike seeding, each action makes the events (and stories) Asana would, attributed to the
//! person: they queue on every webhook that can see them, for
//! [`FakeAsana::deliver`](crate::FakeAsana::deliver) to send.

use asana_client::CustomFieldValue;
use asana_openapi::ids::{CustomFieldGid, ProjectGid, SectionGid, StoryGid, TaskGid, UserGid};
use serde_json::Value;

use crate::FakeAsana;
use crate::answer::Refusal;
use crate::changes::Change;
use crate::model::Position;
use crate::select::bare_ref;
use crate::state::{Id, Kind, State, lookup};

impl FakeAsana {
    /// Acts as `user`.
    pub fn as_user(&self, user: &UserGid) -> Actor<'_> {
        Actor {
            fake: self,
            user: user.to_string(),
        }
    }
}

/// A person acting in the fake, from [`FakeAsana::as_user`].
///
/// Each action takes effect at once. An action Asana would refuse is a bug in the test, so it
/// panics with Asana's message.
#[derive(Clone, Debug)]
pub struct Actor<'a> {
    fake: &'a FakeAsana,
    user: Id,
}

impl Actor<'_> {
    fn act<T>(&self, what: &str, action: impl FnOnce(&mut State) -> Result<T, Refusal>) -> T {
        let result = {
            let mut state = self.fake.lock();
            let known = state.users.contains_key(&self.user);
            state.actor = Some(self.user.clone());
            let result = if known {
                action(&mut state)
            } else {
                Err(Refusal::unknown("user", &self.user))
            };
            state.actor = None;
            result
        };
        match result {
            Ok(value) => value,
            Err(refusal) => panic!("asana-fake: {what} was refused: {}", refusal.message),
        }
    }

    /// Drags `task` into `section` (to the top, as a drop does by default), adding it to the
    /// section's project if it was not in it. Emits `removed` from the old section, `added` to
    /// the new one, and a `section_changed` story.
    pub fn move_task(&self, task: &TaskGid, section: &SectionGid) {
        self.act("moving a task", |state| {
            let section = section.to_string();
            let project = lookup(&state.sections, "section", &section)?
                .project
                .clone();
            state.place(
                &task.to_string(),
                &project,
                Some(&section),
                &Position::Start,
            )
        });
    }

    /// Adds `task` to `project` (at the end), emitting `added`.
    pub fn add_to_project(&self, task: &TaskGid, project: &ProjectGid) {
        self.act("adding a task to a project", |state| {
            state.place(
                &task.to_string(),
                &project.to_string(),
                None,
                &Position::End,
            )
        });
    }

    /// Creates a task at the end of `section`, emitting `added` for its project and section.
    pub fn create_task(&self, section: &SectionGid, name: &str) -> TaskGid {
        let gid = self.act("creating a task", |state| {
            let section = section.to_string();
            let project = lookup(&state.sections, "section", &section)?
                .project
                .clone();
            let workspace = lookup(&state.projects, "project", &project)?
                .workspace
                .clone();
            let task = state.blank_task(workspace, name.to_owned());
            let gid = state.new_gid(Kind::Task);
            state.tasks.insert(gid.clone(), task);
            state.place(&gid, &project, Some(&section), &Position::End)?;
            Ok(gid)
        });
        crate::typed(&gid)
    }

    /// Renames `task`, emitting `changed` for `name`.
    pub fn rename_task(&self, task: &TaskGid, name: &str) {
        self.edit(task, "name", |record| {
            record.name = name.to_owned();
        });
    }

    /// Marks `task` complete or incomplete, emitting `changed` for `completed`.
    pub fn complete_task(&self, task: &TaskGid, completed: bool) {
        self.act("completing a task", |state| {
            let now = state.now();
            let gid = task.to_string();
            let record = crate::state::lookup_mut(&mut state.tasks, "task", &gid)?;
            if record.completed == completed {
                return Ok(());
            }
            record.completed = completed;
            record.completed_at = completed.then_some(now);
            record.modified_at = now;
            state.emit(Change::changed(&gid, "completed", None));
            Ok(())
        });
    }

    /// Assigns `task` to `assignee` (or unassigns it), emitting `changed` for `assignee`.
    pub fn assign(&self, task: &TaskGid, assignee: Option<&UserGid>) {
        self.act("assigning a task", |state| {
            let gid = task.to_string();
            let assignee = assignee.map(ToString::to_string);
            if let Some(user) = &assignee {
                lookup(&state.users, "assignee", user)?;
            }
            let now = state.now();
            let record = crate::state::lookup_mut(&mut state.tasks, "task", &gid)?;
            if record.assignee == assignee {
                return Ok(());
            }
            record.assignee.clone_from(&assignee);
            record.modified_at = now;
            let value = assignee.map_or(Value::Null, |user| bare_ref(state, &user));
            state.emit(Change::changed(&gid, "assignee", Some(value)));
            Ok(())
        });
    }

    /// Sets a custom field on `task`, emitting `changed` for `custom_fields`.
    pub fn set_custom_field(
        &self,
        task: &TaskGid,
        field: &CustomFieldGid,
        value: &CustomFieldValue,
    ) {
        let raw = value.to_request_json();
        self.act("setting a custom field", |state| {
            let gid = task.to_string();
            let record = lookup(&state.tasks, "task", &gid)?;
            let fields = state.fields_on_task(record);
            let assignment = state.check_value(&fields, &field.to_string(), &raw)?;
            state.assign(&gid, vec![assignment]);
            Ok(())
        });
    }

    /// Comments on `task`, emitting `added` for the story.
    pub fn comment(&self, task: &TaskGid, text: &str) -> StoryGid {
        let gid = self.act("commenting", |state| {
            state.comment(&task.to_string(), Some(text.to_owned()), None, false)
        });
        crate::typed(&gid)
    }

    /// Deletes `task` and its subtasks, emitting `deleted`.
    pub fn delete_task(&self, task: &TaskGid) {
        self.act("deleting a task", |state| {
            state.delete_task(&task.to_string())
        });
    }

    fn edit(
        &self,
        task: &TaskGid,
        field: &'static str,
        change: impl FnOnce(&mut crate::state::Task),
    ) {
        self.act("editing a task", |state| {
            let gid = task.to_string();
            let now = state.now();
            let record = crate::state::lookup_mut(&mut state.tasks, "task", &gid)?;
            change(record);
            record.modified_at = now;
            state.emit(Change::changed(&gid, field, None));
            Ok(())
        });
    }
}
