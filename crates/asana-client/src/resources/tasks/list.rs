//! Listing tasks by project, section, tag or assignee, choosing the operation that serves the
//! filter.

use asana_openapi::endpoint::Paginated;
use asana_openapi::endpoints::{GetTasks, GetTasksForTag};
use asana_openapi::fields::{Fields, TaskField};
use asana_openapi::ids::{ProjectGid, SectionGid, TagGid, UserRef, WorkspaceGid};
use asana_openapi::models::Task;
use futures_util::StreamExt;
use futures_util::stream;
use time::OffsetDateTime;

use crate::{Client, Error, Items, Page, Pages};

/// A list of tasks, from [`Tasks::list`](super::Tasks::list). Name exactly one of a project, a
/// section, a tag or an assignee, then read it with [`stream`](Self::stream),
/// [`pages`](Self::pages), [`collect_max`](Self::collect_max) or
/// [`first_page`](Self::first_page).
///
/// A project, a section or an assignee is listed with `getTasks` (`GET /tasks`); a tag with
/// `getTasksForTag` (`GET /tags/{tag_gid}/tasks`), which cannot filter by time. Naming none of
/// them, more than one, or a time filter beside a tag, fails with [`Error::InvalidRequest`] before
/// anything is sent.
#[must_use = "a list does nothing until it is read"]
#[derive(Debug)]
pub struct TaskList<'a> {
    client: &'a Client,
    project: Option<ProjectGid>,
    section: Option<SectionGid>,
    tag: Option<TagGid>,
    assignee: Option<(UserRef, WorkspaceGid)>,
    completed_since: Option<OffsetDateTime>,
    modified_since: Option<OffsetDateTime>,
    fields: Fields<TaskField>,
}

/// The operation a [`TaskList`] resolves to.
enum Resolved {
    Tasks(GetTasks),
    Tag(GetTasksForTag),
}

impl<'a> TaskList<'a> {
    pub(super) fn new(client: &'a Client) -> Self {
        Self {
            client,
            project: None,
            section: None,
            tag: None,
            assignee: None,
            completed_since: None,
            modified_since: None,
            fields: Fields::none(),
        }
    }

    /// The tasks in a project.
    pub fn project(mut self, project: ProjectGid) -> Self {
        self.project = Some(project);
        self
    }

    /// The tasks in a section.
    pub fn section(mut self, section: SectionGid) -> Self {
        self.section = Some(section);
        self
    }

    /// The tasks with a tag.
    pub fn tag(mut self, tag: TagGid) -> Self {
        self.tag = Some(tag);
        self
    }

    /// The tasks assigned to a user in a workspace (Asana needs both).
    pub fn assignee(mut self, user: impl Into<UserRef>, workspace: WorkspaceGid) -> Self {
        self.assignee = Some((user.into(), workspace));
        self
    }

    /// Only tasks that are incomplete or were completed since this moment. Pass the present
    /// moment for incomplete tasks only.
    pub fn completed_since(mut self, since: OffsetDateTime) -> Self {
        self.completed_since = Some(since);
        self
    }

    /// Only tasks changed since this moment. Asana counts a story or a like as a change.
    pub fn modified_since(mut self, since: OffsetDateTime) -> Self {
        self.modified_since = Some(since);
        self
    }

    /// Chooses the fields Asana returns (`opt_fields`), for example `fields![Task: name, due_on]`.
    pub fn fields(mut self, fields: Fields<TaskField>) -> Self {
        self.fields = fields;
        self
    }

    /// Every task in the list, fetched 100 at a time as the stream is read.
    pub fn stream(self) -> Items<Task> {
        let client = self.client.clone();
        match self.resolve() {
            Ok(Resolved::Tasks(endpoint)) => client.stream(endpoint),
            Ok(Resolved::Tag(endpoint)) => client.stream(endpoint),
            Err(error) => stream::once(async move { Err(error) }).boxed(),
        }
    }

    /// Every page of the list, 100 tasks to a page.
    pub fn pages(self) -> Pages<Task> {
        let client = self.client.clone();
        match self.resolve() {
            Ok(Resolved::Tasks(endpoint)) => client.pages(endpoint),
            Ok(Resolved::Tag(endpoint)) => client.pages(endpoint),
            Err(error) => stream::once(async move { Err(error) }).boxed(),
        }
    }

    /// The first `max` tasks, fetching no more pages than that needs.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidRequest`] for a list that names no project, section, tag or assignee, or
    /// more than one; otherwise the first error any page met, as [`Client::send`].
    pub async fn collect_max(self, max: usize) -> Result<Vec<Task>, Error> {
        let client = self.client;
        match self.resolve()? {
            Resolved::Tasks(endpoint) => client.collect_max(endpoint, max).await,
            Resolved::Tag(endpoint) => client.collect_max(endpoint, max).await,
        }
    }

    /// One page of up to 100 tasks, with the offset of the next.
    ///
    /// # Errors
    ///
    /// As [`collect_max`](Self::collect_max).
    pub async fn first_page(self) -> Result<Page<Task>, Error> {
        let client = self.client;
        match self.resolve()? {
            Resolved::Tasks(mut endpoint) => {
                endpoint.set_limit(asana_openapi::endpoint::PageLimit::MAX);
                client.send_page(endpoint).await
            }
            Resolved::Tag(mut endpoint) => {
                endpoint.set_limit(asana_openapi::endpoint::PageLimit::MAX);
                client.send_page(endpoint).await
            }
        }
    }

    /// Picks the operation for the filter, refusing a filter Asana would refuse.
    fn resolve(self) -> Result<Resolved, Error> {
        let named = usize::from(self.project.is_some())
            + usize::from(self.section.is_some())
            + usize::from(self.tag.is_some())
            + usize::from(self.assignee.is_some());
        if named != 1 {
            return Err(Error::InvalidRequest {
                operation: "getTasks",
                reason: "a task list names exactly one of a project, a section, a tag or an assignee",
            });
        }
        if let Some(tag) = self.tag {
            if self.completed_since.is_some() || self.modified_since.is_some() {
                return Err(Error::InvalidRequest {
                    operation: "getTasksForTag",
                    reason: "a tag's tasks cannot be filtered by completion or modification time",
                });
            }
            let mut endpoint = GetTasksForTag::builder(tag).build();
            endpoint.opt_fields = self.fields;
            return Ok(Resolved::Tag(endpoint));
        }
        let mut endpoint = GetTasks::builder().build();
        endpoint.project = self.project;
        endpoint.section = self.section;
        if let Some((user, workspace)) = self.assignee {
            endpoint.assignee = Some(user);
            endpoint.workspace = Some(workspace);
        }
        endpoint.completed_since = self.completed_since;
        endpoint.modified_since = self.modified_since;
        endpoint.opt_fields = self.fields;
        Ok(Resolved::Tasks(endpoint))
    }
}
