//! The setters for a task's fields, on the create, create-subtask and update requests.
//!
//! A setter puts its value in the generated request body. A field nobody set is left out of the
//! request, so Asana keeps what it has; a `clear_*` setter sends `null`, which empties the field.

use asana_openapi::endpoints::{CreateSubtaskForTask, CreateTask, UpdateTask};
use asana_openapi::ids::{
    CustomFieldGid, ProjectGid, SectionGid, TagGid, TaskGid, UserRef, WorkspaceGid,
};
use asana_openapi::models::{Clearable, TaskCreateRequestMemberships};
use time::{Date, OffsetDateTime};

use crate::custom_field_value::CustomFieldValue;
use crate::resources::request::Request;

/// The setters every request that writes a task's own fields has.
macro_rules! task_field_setters {
    ($($endpoint:ty),+ $(,)?) => { $(
        impl Request<'_, $endpoint> {
            /// The task's name.
            pub fn name(mut self, name: impl Into<String>) -> Self {
                self.endpoint_mut().data.name = Some(name.into());
                self
            }

            /// The description, as plain text. Use [`html_notes`](Self::html_notes) instead for
            /// formatting; not both.
            pub fn notes(mut self, notes: impl Into<String>) -> Self {
                self.endpoint_mut().data.notes = Some(notes.into());
                self
            }

            /// The description as Asana's rich text, wrapped in `<body>`.
            pub fn html_notes(mut self, html: impl Into<String>) -> Self {
                self.endpoint_mut().data.html_notes = Some(html.into());
                self
            }

            /// Marks the task complete or incomplete.
            pub fn completed(mut self, completed: bool) -> Self {
                self.endpoint_mut().data.completed = Some(completed);
                self
            }

            /// The day the task is due. Not together with [`due_at`](Self::due_at).
            pub fn due_on(mut self, date: Date) -> Self {
                self.endpoint_mut().data.due_on = Some(Clearable::Set(date));
                self
            }

            /// Sends `due_on: null`, removing the due date.
            pub fn clear_due_on(mut self) -> Self {
                self.endpoint_mut().data.due_on = Some(Clearable::Clear);
                self
            }

            /// The moment the task is due. Not together with [`due_on`](Self::due_on).
            pub fn due_at(mut self, at: OffsetDateTime) -> Self {
                self.endpoint_mut().data.due_at = Some(Clearable::Set(at));
                self
            }

            /// Sends `due_at: null`, removing the due time.
            pub fn clear_due_at(mut self) -> Self {
                self.endpoint_mut().data.due_at = Some(Clearable::Clear);
                self
            }

            /// The day work on the task starts. Asana needs a due date beside it.
            pub fn start_on(mut self, date: Date) -> Self {
                self.endpoint_mut().data.start_on = Some(Clearable::Set(date));
                self
            }

            /// Sends `start_on: null`, removing the start date.
            pub fn clear_start_on(mut self) -> Self {
                self.endpoint_mut().data.start_on = Some(Clearable::Clear);
                self
            }

            /// The moment work on the task starts.
            pub fn start_at(mut self, at: OffsetDateTime) -> Self {
                self.endpoint_mut().data.start_at = Some(Clearable::Set(at));
                self
            }

            /// Sends `start_at: null`, removing the start time.
            pub fn clear_start_at(mut self) -> Self {
                self.endpoint_mut().data.start_at = Some(Clearable::Clear);
                self
            }

            /// Assigns the task, by GID, email address or [`UserRef::Me`].
            pub fn assignee(mut self, user: impl Into<UserRef>) -> Self {
                self.endpoint_mut().data.assignee = Some(Clearable::Set(user.into()));
                self
            }

            /// Sends `assignee: null`, leaving the task unassigned.
            pub fn unassign(mut self) -> Self {
                self.endpoint_mut().data.assignee = Some(Clearable::Clear);
                self
            }

            /// Sets one custom field. Call it once per field; a later value for the same field
            /// replaces an earlier one. The field must be on one of the task's projects.
            pub fn custom_field(mut self, field: CustomFieldGid, value: CustomFieldValue) -> Self {
                self.endpoint_mut()
                    .data
                    .custom_fields
                    .get_or_insert_with(Default::default)
                    .insert(field, value.to_request_json());
                self
            }
        }
    )+ };
}

task_field_setters!(CreateTask, CreateSubtaskForTask, UpdateTask);

/// The setters only a new task has: where it goes and who follows it.
macro_rules! task_create_setters {
    ($($endpoint:ty),+ $(,)?) => { $(
        impl Request<'_, $endpoint> {
            /// The workspace to make the task in. Not needed when a project or section is given.
            pub fn workspace(mut self, workspace: WorkspaceGid) -> Self {
                self.endpoint_mut().data.workspace = Some(workspace);
                self
            }

            /// Adds the task to a project, at the end of it. Call it again for more projects.
            pub fn project(mut self, project: ProjectGid) -> Self {
                self.endpoint_mut()
                    .data
                    .projects
                    .get_or_insert_with(Vec::new)
                    .push(project);
                self
            }

            /// Adds the task to a project and puts it in `section` of that project, at the end
            /// of the section. Call it again for more projects.
            pub fn section(mut self, project: ProjectGid, section: SectionGid) -> Self {
                let mut membership = TaskCreateRequestMemberships::default();
                membership.project = Some(project);
                membership.section = Some(section);
                self.endpoint_mut()
                    .data
                    .memberships
                    .get_or_insert_with(Vec::new)
                    .push(membership);
                self
            }

            /// Adds a follower, by GID, email address or [`UserRef::Me`]. Call it again for more.
            pub fn follower(mut self, user: impl Into<UserRef>) -> Self {
                self.endpoint_mut()
                    .data
                    .followers
                    .get_or_insert_with(Vec::new)
                    .push(user.into());
                self
            }

            /// Adds a tag. Call it again for more.
            pub fn tag(mut self, tag: TagGid) -> Self {
                self.endpoint_mut()
                    .data
                    .tags
                    .get_or_insert_with(Vec::new)
                    .push(tag);
                self
            }
        }
    )+ };
}

task_create_setters!(CreateTask, CreateSubtaskForTask);

impl Request<'_, CreateTask> {
    /// Makes the task a subtask of `parent`. The same as
    /// [`Tasks::create_subtask`](super::Tasks::create_subtask).
    pub fn parent(mut self, parent: TaskGid) -> Self {
        self.endpoint_mut().data.parent = Some(Clearable::Set(parent.into()));
        self
    }
}
