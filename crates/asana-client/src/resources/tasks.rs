//! Tasks: read, list, create, update, delete, subtasks, projects, sections, followers and tags.
//!
//! `client.tasks()` returns a [`Tasks`] handle. Each method builds the generated descriptor for one
//! operation and returns it as a [`Request`]; the setters for that operation are methods of the
//! request, and it is sent with `.send().await` (or read with `.stream()` for a list).
//!
//! ```no_run
//! # async fn run(client: asana_client::Client, task: asana_client::openapi::ids::TaskGid,
//! #     section: asana_client::openapi::ids::SectionGid) -> Result<(), asana_client::Error> {
//! use asana_client::fields;
//! use asana_client::resources::tasks::TaskExt;
//!
//! let read = client
//!     .tasks()
//!     .get(task.clone())
//!     .fields(fields![Task: name, due_on, memberships.project.name, memberships.section.name])
//!     .send()
//!     .await?;
//! if !read.is_in_section(&section) {
//!     client.sections().add_task(section, task.clone()).send().await?;
//! }
//! client.tasks().update(task).clear_due_on().send().await?;
//! # Ok(()) }
//! ```

mod body;
mod ext;
mod list;

use asana_openapi::endpoints::{
    AddDependenciesForTask, AddDependentsForTask, AddFollowersForTask, AddProjectForTask,
    AddTagForTask, CreateSubtaskForTask, CreateTask, DeleteTask, DuplicateTask,
    GetDependenciesForTask, GetDependentsForTask, GetSubtasksForTask, GetTask,
    RemoveDependenciesForTask, RemoveDependentsForTask, RemoveFollowerForTask,
    RemoveProjectForTask, RemoveTagForTask, SetParentForTask, UpdateTask,
};
use asana_openapi::fields::{Fields, JobField, TaskField};
use asana_openapi::ids::{ProjectGid, SectionGid, TagGid, TaskGid, UserRef};
use asana_openapi::models::{
    Clearable, ModifyDependenciesRequest, ModifyDependentsRequest, TaskAddFollowersRequest,
    TaskAddProjectRequest, TaskAddTagRequest, TaskCreateRequest, TaskDuplicateRequest,
    TaskRemoveFollowersRequest, TaskRemoveProjectRequest, TaskRemoveTagRequest,
    TaskSetParentRequest, TaskUpdateRequest,
};

use crate::Client;
use crate::resources::jobs::StartJob;
use crate::resources::request::{Request, opt_fields_setter};

pub use ext::TaskExt;
pub use list::TaskList;

impl Client {
    /// Tasks: read, list, create, update, and move between projects, sections and parents.
    pub fn tasks(&self) -> Tasks<'_> {
        Tasks { client: self }
    }
}

/// The task operations, from [`Client::tasks`].
#[derive(Clone, Copy, Debug)]
pub struct Tasks<'a> {
    client: &'a Client,
}

opt_fields_setter!(
    GetTask => TaskField,
    CreateTask => TaskField,
    UpdateTask => TaskField,
    GetSubtasksForTask => TaskField,
    CreateSubtaskForTask => TaskField,
    AddFollowersForTask => TaskField,
    RemoveFollowerForTask => TaskField,
    SetParentForTask => TaskField,
    GetDependenciesForTask => TaskField,
    GetDependentsForTask => TaskField,
);

impl<'a> Tasks<'a> {
    /// One task.
    ///
    /// Sends `getTask` (`GET /tasks/{task_gid}`), scope `tasks:read`. A read: retried on a 5xx
    /// or a lost connection.
    pub fn get(&self, task: TaskGid) -> Request<'a, GetTask> {
        Request::new(self.client, GetTask::builder(task).build())
    }

    /// Tasks in a project, a section, a tag, or assigned to a user in a workspace. Name exactly
    /// one of those on the [`TaskList`] before reading it.
    ///
    /// Sends `getTasks` (`GET /tasks`), or `getTasksForTag` (`GET /tags/{tag_gid}/tasks`) for a
    /// tag, scope `tasks:read`. Reads: retried.
    pub fn list(&self) -> TaskList<'a> {
        TaskList::new(self.client)
    }

    /// Creates a task. Name it, and give it somewhere to live: `.workspace(..)`, `.project(..)`,
    /// or `.section(project, section)` to put it straight into a section.
    ///
    /// Sends `createTask` (`POST /tasks`), scope `tasks:write`. Not retried after a 5xx or a
    /// lost answer, because sending it twice would make two tasks.
    pub fn create(&self) -> Request<'a, CreateTask> {
        Request::new(
            self.client,
            CreateTask::builder()
                .data(TaskCreateRequest::default())
                .build(),
        )
    }

    /// Changes a task: only the fields set on the request are sent, and a `clear_*` setter sends
    /// `null` for its field.
    ///
    /// Sends `updateTask` (`PUT /tasks/{task_gid}`), scope `tasks:write`. A `PUT`: retried.
    pub fn update(&self, task: TaskGid) -> Request<'a, UpdateTask> {
        Request::new(
            self.client,
            UpdateTask::builder(task)
                .data(TaskUpdateRequest::default())
                .build(),
        )
    }

    /// Deletes a task (to the trash, from which it can be restored for 30 days).
    ///
    /// Sends `deleteTask` (`DELETE /tasks/{task_gid}`), scope `tasks:delete`. Retried.
    pub fn delete(&self, task: TaskGid) -> Request<'a, DeleteTask> {
        Request::new(self.client, DeleteTask::builder(task).build())
    }

    /// A task's subtasks.
    ///
    /// Sends `getSubtasksForTask` (`GET /tasks/{task_gid}/subtasks`), scope `tasks:read`.
    /// Retried.
    pub fn subtasks(&self, task: TaskGid) -> Request<'a, GetSubtasksForTask> {
        Request::new(self.client, GetSubtasksForTask::builder(task).build())
    }

    /// Creates a subtask of `parent`, with the same setters as [`create`](Self::create).
    ///
    /// Sends `createSubtaskForTask` (`POST /tasks/{task_gid}/subtasks`), scope `tasks:write`.
    /// Not retried after a 5xx or a lost answer.
    pub fn create_subtask(&self, parent: TaskGid) -> Request<'a, CreateSubtaskForTask> {
        Request::new(
            self.client,
            CreateSubtaskForTask::builder(parent)
                .data(TaskCreateRequest::default())
                .build(),
        )
    }

    /// Adds a task to a project, at the end of the project unless the request places it: in a
    /// section (`.section(..)`), before or after another task (`.insert_before(..)`,
    /// `.insert_after(..)`), or at either end (`.at_start()`, `.at_end()`).
    ///
    /// Adding a task to a project it is already in moves it. To move a task between sections of
    /// one project, [`Sections::add_task`](crate::resources::sections::Sections::add_task) reads
    /// better.
    ///
    /// Sends `addProjectForTask` (`POST /tasks/{task_gid}/addProject`), scope `tasks:write`.
    /// Idempotent: retried.
    pub fn add_project(
        &self,
        task: TaskGid,
        project: ProjectGid,
    ) -> Request<'a, AddProjectForTask> {
        let mut data = TaskAddProjectRequest::default();
        data.project = Some(project);
        Request::new(
            self.client,
            AddProjectForTask::builder(task).data(data).build(),
        )
    }

    /// Removes a task from a project. The task itself stays.
    ///
    /// Sends `removeProjectForTask` (`POST /tasks/{task_gid}/removeProject`), scope
    /// `tasks:write`. Idempotent: retried.
    pub fn remove_project(
        &self,
        task: TaskGid,
        project: ProjectGid,
    ) -> Request<'a, RemoveProjectForTask> {
        let mut data = TaskRemoveProjectRequest::default();
        data.project = Some(project);
        Request::new(
            self.client,
            RemoveProjectForTask::builder(task).data(data).build(),
        )
    }

    /// Adds followers to a task, by GID, email address or [`UserRef::Me`]. Returns the task.
    ///
    /// Sends `addFollowersForTask` (`POST /tasks/{task_gid}/addFollowers`), scope
    /// `tasks:write`. Idempotent: retried.
    pub fn add_followers<U: Into<UserRef>>(
        &self,
        task: TaskGid,
        followers: impl IntoIterator<Item = U>,
    ) -> Request<'a, AddFollowersForTask> {
        let mut data = TaskAddFollowersRequest::default();
        data.followers = Some(followers.into_iter().map(Into::into).collect());
        Request::new(
            self.client,
            AddFollowersForTask::builder(task).data(data).build(),
        )
    }

    /// Removes followers from a task. Returns the task.
    ///
    /// Sends `removeFollowerForTask` (`POST /tasks/{task_gid}/removeFollowers`), scope
    /// `tasks:write`. Idempotent: retried.
    pub fn remove_followers<U: Into<UserRef>>(
        &self,
        task: TaskGid,
        followers: impl IntoIterator<Item = U>,
    ) -> Request<'a, RemoveFollowerForTask> {
        let mut data = TaskRemoveFollowersRequest::default();
        data.followers = Some(followers.into_iter().map(Into::into).collect());
        Request::new(
            self.client,
            RemoveFollowerForTask::builder(task).data(data).build(),
        )
    }

    /// Adds a tag to a task.
    ///
    /// Sends `addTagForTask` (`POST /tasks/{task_gid}/addTag`), scope `tasks:write`.
    /// Idempotent: retried.
    pub fn add_tag(&self, task: TaskGid, tag: TagGid) -> Request<'a, AddTagForTask> {
        let mut data = TaskAddTagRequest::default();
        data.tag = Some(tag);
        Request::new(self.client, AddTagForTask::builder(task).data(data).build())
    }

    /// Removes a tag from a task.
    ///
    /// Sends `removeTagForTask` (`POST /tasks/{task_gid}/removeTag`), scope `tasks:write`.
    /// Idempotent: retried.
    pub fn remove_tag(&self, task: TaskGid, tag: TagGid) -> Request<'a, RemoveTagForTask> {
        let mut data = TaskRemoveTagRequest::default();
        data.tag = Some(tag);
        Request::new(
            self.client,
            RemoveTagForTask::builder(task).data(data).build(),
        )
    }

    /// Makes a task a subtask of `parent`, at the end of its subtasks unless the request places
    /// it before or after a sibling (`.insert_before(..)`, `.insert_after(..)`).
    /// Returns the task.
    ///
    /// Sends `setParentForTask` (`POST /tasks/{task_gid}/setParent`), scope `tasks:write`.
    /// Idempotent: retried.
    pub fn set_parent(&self, task: TaskGid, parent: TaskGid) -> Request<'a, SetParentForTask> {
        let mut data = TaskSetParentRequest::default();
        data.parent = Some(Clearable::Set(parent.into()));
        Request::new(
            self.client,
            SetParentForTask::builder(task).data(data).build(),
        )
    }

    /// Makes a subtask a top-level task again (`parent: null`). Returns the task.
    ///
    /// Sends `setParentForTask` (`POST /tasks/{task_gid}/setParent`), scope `tasks:write`.
    /// Idempotent: retried.
    pub fn remove_parent(&self, task: TaskGid) -> Request<'a, SetParentForTask> {
        let mut data = TaskSetParentRequest::default();
        data.parent = Some(Clearable::Clear);
        Request::new(
            self.client,
            SetParentForTask::builder(task).data(data).build(),
        )
    }

    /// Copies a task under a new name. Asana does the copying in the background: the answer is
    /// a [`JobHandle`](crate::resources::jobs::JobHandle); `wait_for_task` on it returns the copy
    /// once Asana has made it.
    ///
    /// Sends `duplicateTask` (`POST /tasks/{task_gid}/duplicate`), scope `tasks:write`. Not
    /// retried after a 5xx or a lost answer, because sending it twice would make two copies.
    pub fn duplicate(&self, task: TaskGid, name: impl Into<String>) -> StartJob<'a, DuplicateTask> {
        let mut data = TaskDuplicateRequest::default();
        data.name = Some(name.into());
        StartJob::new(Request::new(
            self.client,
            DuplicateTask::builder(task).data(data).build(),
        ))
    }

    /// The tasks this task depends on.
    ///
    /// Sends `getDependenciesForTask` (`GET /tasks/{task_gid}/dependencies`), scope
    /// `tasks:read`. Retried.
    pub fn dependencies(&self, task: TaskGid) -> Request<'a, GetDependenciesForTask> {
        Request::new(self.client, GetDependenciesForTask::builder(task).build())
    }

    /// Marks this task as depending on others (they block it).
    ///
    /// Sends `addDependenciesForTask` (`POST /tasks/{task_gid}/addDependencies`), scope
    /// `tasks:write`. Idempotent: retried.
    pub fn add_dependencies(
        &self,
        task: TaskGid,
        dependencies: impl IntoIterator<Item = TaskGid>,
    ) -> Request<'a, AddDependenciesForTask> {
        let mut data = ModifyDependenciesRequest::default();
        data.dependencies = Some(gid_strings(dependencies));
        Request::new(
            self.client,
            AddDependenciesForTask::builder(task).data(data).build(),
        )
    }

    /// Removes dependencies from this task.
    ///
    /// Sends `removeDependenciesForTask` (`POST /tasks/{task_gid}/removeDependencies`), scope
    /// `tasks:write`. Idempotent: retried.
    pub fn remove_dependencies(
        &self,
        task: TaskGid,
        dependencies: impl IntoIterator<Item = TaskGid>,
    ) -> Request<'a, RemoveDependenciesForTask> {
        let mut data = ModifyDependenciesRequest::default();
        data.dependencies = Some(gid_strings(dependencies));
        Request::new(
            self.client,
            RemoveDependenciesForTask::builder(task).data(data).build(),
        )
    }

    /// The tasks that depend on this one.
    ///
    /// Sends `getDependentsForTask` (`GET /tasks/{task_gid}/dependents`), scope `tasks:read`.
    /// Retried.
    pub fn dependents(&self, task: TaskGid) -> Request<'a, GetDependentsForTask> {
        Request::new(self.client, GetDependentsForTask::builder(task).build())
    }

    /// Marks other tasks as depending on this one (it blocks them).
    ///
    /// Sends `addDependentsForTask` (`POST /tasks/{task_gid}/addDependents`), scope
    /// `tasks:write`. Idempotent: retried.
    pub fn add_dependents(
        &self,
        task: TaskGid,
        dependents: impl IntoIterator<Item = TaskGid>,
    ) -> Request<'a, AddDependentsForTask> {
        let mut data = ModifyDependentsRequest::default();
        data.dependents = Some(gid_strings(dependents));
        Request::new(
            self.client,
            AddDependentsForTask::builder(task).data(data).build(),
        )
    }

    /// Removes dependents from this task.
    ///
    /// Sends `removeDependentsForTask` (`POST /tasks/{task_gid}/removeDependents`), scope
    /// `tasks:write`. Idempotent: retried.
    pub fn remove_dependents(
        &self,
        task: TaskGid,
        dependents: impl IntoIterator<Item = TaskGid>,
    ) -> Request<'a, RemoveDependentsForTask> {
        let mut data = ModifyDependentsRequest::default();
        data.dependents = Some(gid_strings(dependents));
        Request::new(
            self.client,
            RemoveDependentsForTask::builder(task).data(data).build(),
        )
    }
}

/// GIDs as the plain strings some request bodies are typed with.
fn gid_strings(gids: impl IntoIterator<Item = TaskGid>) -> Vec<String> {
    gids.into_iter().map(String::from).collect()
}

impl Request<'_, AddProjectForTask> {
    /// Puts the task in this section of the project. Without a position it goes to the end of
    /// the section.
    pub fn section(mut self, section: SectionGid) -> Self {
        self.endpoint_mut().data.section = Some(Clearable::Set(section));
        self
    }

    /// Puts the task just before `task`, which must be in the project (and in the section, when
    /// one is given).
    pub fn insert_before(mut self, task: TaskGid) -> Self {
        self.endpoint_mut().data.insert_before = Some(Clearable::Set(task.into()));
        self
    }

    /// Puts the task just after `task`, which must be in the project (and in the section, when
    /// one is given).
    pub fn insert_after(mut self, task: TaskGid) -> Self {
        self.endpoint_mut().data.insert_after = Some(Clearable::Set(task.into()));
        self
    }

    /// Puts the task first in the project, or first in the section when one is given
    /// (`insert_after: null`).
    pub fn at_start(mut self) -> Self {
        self.endpoint_mut().data.insert_after = Some(Clearable::Clear);
        self
    }

    /// Puts the task last in the project, or last in the section when one is given
    /// (`insert_before: null`).
    pub fn at_end(mut self) -> Self {
        self.endpoint_mut().data.insert_before = Some(Clearable::Clear);
        self
    }
}

impl Request<'_, SetParentForTask> {
    /// Puts the task just before `sibling`, a subtask of the new parent.
    pub fn insert_before(mut self, sibling: TaskGid) -> Self {
        self.endpoint_mut().data.insert_before = Some(Clearable::Set(sibling.into()));
        self
    }

    /// Puts the task just after `sibling`, a subtask of the new parent.
    pub fn insert_after(mut self, sibling: TaskGid) -> Self {
        self.endpoint_mut().data.insert_after = Some(Clearable::Set(sibling.into()));
        self
    }
}

/// What a duplicated task copies from the original besides its name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DuplicateInclude {
    /// The assignee.
    Assignee,
    /// The attachments.
    Attachments,
    /// The start and due dates.
    Dates,
    /// The dependencies.
    Dependencies,
    /// The followers.
    Followers,
    /// The description.
    Notes,
    /// The parent task.
    Parent,
    /// The projects the task is in.
    Projects,
    /// The subtasks.
    Subtasks,
    /// The tags.
    Tags,
}

impl DuplicateInclude {
    /// The name Asana's `include` list uses.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Assignee => "assignee",
            Self::Attachments => "attachments",
            Self::Dates => "dates",
            Self::Dependencies => "dependencies",
            Self::Followers => "followers",
            Self::Notes => "notes",
            Self::Parent => "parent",
            Self::Projects => "projects",
            Self::Subtasks => "subtasks",
            Self::Tags => "tags",
        }
    }
}

impl StartJob<'_, DuplicateTask> {
    /// What to copy besides the name. Without it Asana copies nothing else.
    pub fn include(self, parts: impl IntoIterator<Item = DuplicateInclude>) -> Self {
        let list = parts
            .into_iter()
            .map(DuplicateInclude::as_str)
            .collect::<Vec<_>>()
            .join(",");
        self.map(|endpoint| endpoint.data.include = Some(list))
    }

    /// Chooses the job's fields Asana returns (`opt_fields`).
    pub fn fields(self, fields: Fields<JobField>) -> Self {
        self.map(|endpoint| endpoint.opt_fields = fields)
    }
}
