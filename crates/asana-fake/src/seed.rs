//! Seeding: builders that put workspaces, people, projects, sections, tasks, custom fields, tags
//! and templates into the fake before a test runs.
//!
//! Seeding is silent: it makes no events and no stories, so a webhook created afterwards starts
//! with nothing queued. Each builder method takes effect at once. A seeding mistake (a section of
//! another project, a value of the wrong kind) is a bug in the test, so it panics with the message
//! Asana would have answered.

use asana_client::CustomFieldValue;
use asana_openapi::ids::{
    CustomFieldGid, EnumOptionGid, ProjectGid, SectionGid, TagGid, TaskGid, TaskTemplateGid,
    UserGid, WorkspaceGid,
};
use serde_json::Value;

use crate::FakeAsana;
use crate::answer::Refusal;
use crate::model::Position;
use crate::ops::directory::add_tag;
use crate::ops::fields::{add_field, add_setting};
use crate::ops::projects::add_project;
use crate::ops::sections::add_section;
use crate::ops::templates::add_template;
use crate::state::{FieldKind, Id, Kind, State, User, Workspace, lookup};

impl FakeAsana {
    /// Seeds a workspace named `name` (on a paid plan, so search works) and returns its builder.
    /// The token's user is a member of every workspace.
    pub fn workspace(&self, name: &str) -> WorkspaceSeed<'_> {
        let gid = self.seed("a workspace", |state| {
            let gid = state.new_gid(Kind::Workspace);
            let mut users = Vec::new();
            if let Some(me) = state.me.clone() {
                users.push(me.clone());
                if let Some(user) = state.users.get_mut(&me) {
                    user.workspaces.push(gid.clone());
                }
            }
            state.workspaces.insert(
                gid.clone(),
                Workspace {
                    name: name.to_owned(),
                    premium: true,
                    users,
                },
            );
            Ok(gid)
        });
        WorkspaceSeed { fake: self, gid }
    }

    /// Runs one seeding change silently, panicking with Asana's message if it is refused.
    pub(crate) fn seed<T>(
        &self,
        what: &str,
        change: impl FnOnce(&mut State) -> Result<T, Refusal>,
    ) -> T {
        let result = {
            let mut state = self.lock();
            state.silent = true;
            let result = change(&mut state);
            state.silent = false;
            result
        };
        match result {
            Ok(value) => value,
            Err(refusal) => panic!("asana-fake could not seed {what}: {}", refusal.message),
        }
    }
}

/// A seeded workspace, from [`FakeAsana::workspace`].
#[derive(Clone, Debug)]
pub struct WorkspaceSeed<'a> {
    fake: &'a FakeAsana,
    gid: Id,
}

impl<'a> WorkspaceSeed<'a> {
    /// The workspace's GID.
    pub fn gid(&self) -> WorkspaceGid {
        crate::typed(&self.gid)
    }

    /// Puts the workspace on the free plan, so task search answers `402 Payment Required`.
    #[must_use]
    pub fn free(self) -> Self {
        self.fake.seed("a free plan", |state| {
            if let Some(workspace) = state.workspaces.get_mut(&self.gid) {
                workspace.premium = false;
            }
            Ok(())
        });
        self
    }

    /// Seeds a member. A user with the same email address already seeded is reused and added to
    /// this workspace. The first user seeded is the token's user unless
    /// [`FakeAsana::sign_in_as`] says otherwise.
    pub fn user(&self, name: &str, email: &str) -> UserGid {
        let gid = self.fake.seed("a user", |state| {
            let existing = state
                .users
                .iter()
                .find(|(_, user)| user.email.eq_ignore_ascii_case(email))
                .map(|(gid, _)| gid.clone());
            let gid = match existing {
                Some(gid) => gid,
                None => {
                    let gid = state.new_gid(Kind::User);
                    state.users.insert(
                        gid.clone(),
                        User {
                            name: name.to_owned(),
                            email: email.to_owned(),
                            workspaces: Vec::new(),
                        },
                    );
                    gid
                }
            };
            join(state, &gid, &self.gid);
            if state.me.is_none() {
                state.me = Some(gid.clone());
                let workspaces: Vec<Id> = state.workspaces.keys().cloned().collect();
                for workspace in &workspaces {
                    join(state, &gid, workspace);
                }
            }
            Ok(gid)
        });
        crate::typed(&gid)
    }

    /// Seeds a project and returns its builder.
    pub fn project(&self, name: &str) -> ProjectSeed<'a> {
        let gid = self.fake.seed("a project", |state| {
            add_project(state, &self.gid, name.to_owned())
        });
        ProjectSeed {
            fake: self.fake,
            gid,
        }
    }

    /// Seeds a tag.
    pub fn tag(&self, name: &str) -> TagGid {
        let gid = self.fake.seed("a tag", |state| {
            add_tag(state, &self.gid, name.to_owned(), None, String::new())
        });
        crate::typed(&gid)
    }

    fn field(&self, name: &str, kind: FieldKind, options: &[&str]) -> FieldSeed<'a> {
        let options: Vec<String> = options.iter().map(|option| (*option).to_owned()).collect();
        let gid = self.fake.seed("a custom field", |state| {
            add_field(state, &self.gid, name.to_owned(), kind, &options)
        });
        FieldSeed {
            fake: self.fake,
            gid,
        }
    }

    /// Seeds a text custom field.
    pub fn text_field(&self, name: &str) -> FieldSeed<'a> {
        self.field(name, FieldKind::Text, &[])
    }

    /// Seeds a number custom field shown with `precision` decimal places.
    pub fn number_field(&self, name: &str, precision: u32) -> FieldSeed<'a> {
        let seed = self.field(name, FieldKind::Number, &[]);
        seed.fake.seed("a precision", |state| {
            if let Some(field) = state.fields.get_mut(&seed.gid) {
                field.precision = precision;
            }
            Ok(())
        });
        seed
    }

    /// Seeds a single-choice custom field with `options` in order.
    pub fn enum_field(&self, name: &str, options: &[&str]) -> FieldSeed<'a> {
        self.field(name, FieldKind::Enum, options)
    }

    /// Seeds a multiple-choice custom field with `options` in order.
    pub fn multi_enum_field(&self, name: &str, options: &[&str]) -> FieldSeed<'a> {
        self.field(name, FieldKind::MultiEnum, options)
    }

    /// Seeds a date custom field.
    pub fn date_field(&self, name: &str) -> FieldSeed<'a> {
        self.field(name, FieldKind::Date, &[])
    }

    /// Seeds a people custom field.
    pub fn people_field(&self, name: &str) -> FieldSeed<'a> {
        self.field(name, FieldKind::People, &[])
    }
}

/// Adds `user` to `workspace` (both ways), once.
fn join(state: &mut State, user: &Id, workspace: &Id) {
    if let Some(record) = state.workspaces.get_mut(workspace)
        && !record.users.contains(user)
    {
        record.users.push(user.clone());
    }
    if let Some(record) = state.users.get_mut(user)
        && !record.workspaces.contains(workspace)
    {
        record.workspaces.push(workspace.clone());
    }
}

/// A seeded custom field, from the `…_field` methods of [`WorkspaceSeed`].
#[derive(Clone, Debug)]
pub struct FieldSeed<'a> {
    fake: &'a FakeAsana,
    gid: Id,
}

impl FieldSeed<'_> {
    /// The field's GID.
    pub fn gid(&self) -> CustomFieldGid {
        crate::typed(&self.gid)
    }

    /// The GID of the option named `name`.
    ///
    /// # Panics
    ///
    /// When the field has no option of that name.
    pub fn option(&self, name: &str) -> EnumOptionGid {
        let state = self.fake.lock();
        let found = state.fields.get(&self.gid).and_then(|field| {
            field
                .options
                .iter()
                .find(|option| state.options.get(*option).is_some_and(|o| o.name == name))
                .cloned()
        });
        drop(state);
        match found {
            Some(gid) => crate::typed(&gid),
            None => panic!("asana-fake: the custom field has no option named {name:?}"),
        }
    }
}

/// A seeded project, from [`WorkspaceSeed::project`].
#[derive(Clone, Debug)]
pub struct ProjectSeed<'a> {
    fake: &'a FakeAsana,
    gid: Id,
}

impl<'a> ProjectSeed<'a> {
    /// The project's GID.
    pub fn gid(&self) -> ProjectGid {
        crate::typed(&self.gid)
    }

    /// Seeds a section at the end of the project.
    pub fn section(&self, name: &str) -> SectionGid {
        let gid = self.fake.seed("a section", |state| {
            add_section(state, &self.gid, name.to_owned(), &Position::End)
        });
        crate::typed(&gid)
    }

    /// Seeds several sections in order.
    pub fn sections<const N: usize>(&self, names: [&str; N]) -> [SectionGid; N] {
        names.map(|name| self.section(name))
    }

    /// Seeds a task at the end of the project's first section (or of the project, while it has
    /// no sections) and returns its builder.
    pub fn task(&self, name: &str) -> TaskSeed<'a> {
        let gid = self.fake.seed("a task", |state| {
            let workspace = lookup(&state.projects, "project", &self.gid)?
                .workspace
                .clone();
            let task = state.blank_task(workspace, name.to_owned());
            let gid = state.new_gid(Kind::Task);
            state.tasks.insert(gid.clone(), task);
            state.place(&gid, &self.gid, None, &Position::End)?;
            Ok(gid)
        });
        TaskSeed {
            fake: self.fake,
            gid,
        }
    }

    /// Adds the custom field `field` to the project.
    pub fn with_field(&self, field: &CustomFieldGid) -> &Self {
        self.fake.seed("a custom field setting", |state| {
            add_setting(state, &self.gid, &field.to_string(), false, &Position::End)
        });
        self
    }

    /// Seeds a task template whose tasks are named `task_name` and carry `description`.
    pub fn template(&self, name: &str, task_name: &str, description: &str) -> TaskTemplateGid {
        let gid = self.fake.seed("a task template", |state| {
            add_template(
                state,
                &self.gid,
                name.to_owned(),
                task_name.to_owned(),
                description.to_owned(),
            )
        });
        crate::typed(&gid)
    }

    /// Archives the project.
    #[must_use]
    pub fn archived(self) -> Self {
        self.fake.seed("an archived project", |state| {
            if let Some(project) = state.projects.get_mut(&self.gid) {
                project.archived = true;
            }
            Ok(())
        });
        self
    }
}

/// A seeded task, from [`ProjectSeed::task`] or [`TaskSeed::subtask`].
#[derive(Clone, Debug)]
pub struct TaskSeed<'a> {
    fake: &'a FakeAsana,
    gid: Id,
}

impl<'a> TaskSeed<'a> {
    /// The task's GID.
    pub fn gid(&self) -> TaskGid {
        crate::typed(&self.gid)
    }

    fn change(
        self,
        what: &str,
        change: impl FnOnce(&mut State, &Id) -> Result<(), Refusal>,
    ) -> Self {
        self.fake.seed(what, |state| change(state, &self.gid));
        self
    }

    /// Moves the task to the end of `section`, adding it to the section's project if needed.
    #[must_use]
    pub fn in_section(self, section: &SectionGid) -> Self {
        self.change("a section move", |state, task| {
            let section = section.to_string();
            let project = lookup(&state.sections, "section", &section)?
                .project
                .clone();
            state.place(task, &project, Some(&section), &Position::End)
        })
    }

    /// Adds the task to another project too.
    #[must_use]
    pub fn also_in(self, project: &ProjectGid) -> Self {
        self.change("a second project", |state, task| {
            state.place(task, &project.to_string(), None, &Position::End)
        })
    }

    /// Sets the task's notes.
    #[must_use]
    pub fn notes(self, notes: &str) -> Self {
        self.change("notes", |state, task| {
            if let Some(task) = state.tasks.get_mut(task) {
                task.notes = notes.to_owned();
                task.html_notes = None;
            }
            Ok(())
        })
    }

    /// Marks the task complete or incomplete.
    #[must_use]
    pub fn completed(self, completed: bool) -> Self {
        self.change("completion", |state, task| {
            let now = state.now();
            if let Some(task) = state.tasks.get_mut(task) {
                task.completed = completed;
                task.completed_at = completed.then_some(now);
            }
            Ok(())
        })
    }

    /// Sets the task's due date.
    #[must_use]
    pub fn due_on(self, date: time::Date) -> Self {
        self.change("a due date", |state, task| {
            if let Some(task) = state.tasks.get_mut(task) {
                task.due_on = Some(date.to_string());
                task.due_at = None;
            }
            Ok(())
        })
    }

    /// Assigns the task.
    #[must_use]
    pub fn assignee(self, user: &UserGid) -> Self {
        self.change("an assignee", |state, task| {
            let user = user.to_string();
            lookup(&state.users, "assignee", &user)?;
            if let Some(task) = state.tasks.get_mut(task) {
                task.assignee = Some(user);
            }
            Ok(())
        })
    }

    /// Adds a follower.
    #[must_use]
    pub fn follower(self, user: &UserGid) -> Self {
        self.change("a follower", |state, task| {
            let user = user.to_string();
            lookup(&state.users, "followers", &user)?;
            if let Some(task) = state.tasks.get_mut(task)
                && !task.followers.contains(&user)
            {
                task.followers.push(user);
            }
            Ok(())
        })
    }

    /// Adds a tag.
    #[must_use]
    pub fn tag(self, tag: &TagGid) -> Self {
        self.change("a tag", |state, task| {
            let tag = tag.to_string();
            lookup(&state.tags, "tag", &tag)?;
            if let Some(task) = state.tasks.get_mut(task)
                && !task.tags.contains(&tag)
            {
                task.tags.push(tag);
            }
            Ok(())
        })
    }

    /// Sets a custom field's value. The field must be on one of the task's projects, as Asana
    /// requires.
    #[must_use]
    pub fn custom_field(self, field: &CustomFieldGid, value: &CustomFieldValue) -> Self {
        let raw: Value = value.to_request_json();
        self.change("a custom field value", |state, task| {
            let record = lookup(&state.tasks, "task", task)?;
            let fields = state.fields_on_task(record);
            let assignment = state.check_value(&fields, &field.to_string(), &raw)?;
            state.assign(task, vec![assignment]);
            Ok(())
        })
    }

    /// Seeds a subtask at the end of this task's subtasks and returns its builder.
    pub fn subtask(&self, name: &str) -> TaskSeed<'a> {
        let gid = self.fake.seed("a subtask", |state| {
            let workspace = lookup(&state.tasks, "task", &self.gid)?.workspace.clone();
            let task = state.blank_task(workspace, name.to_owned());
            let gid = state.new_gid(Kind::Task);
            state.tasks.insert(gid.clone(), task);
            state.set_parent(&gid, Some(&self.gid), &Position::End)?;
            Ok(gid)
        });
        TaskSeed {
            fake: self.fake,
            gid,
        }
    }
}
