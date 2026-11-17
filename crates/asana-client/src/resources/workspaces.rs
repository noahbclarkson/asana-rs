//! Workspaces, and the workspace task search.
//!
//! `client.workspaces()` lists and reads workspaces and searches a workspace's tasks with Asana's
//! advanced search (`GET /workspaces/{workspace_gid}/tasks/search`).

mod search;

use asana_openapi::endpoints::{GetWorkspace, GetWorkspaces, SearchTasksForWorkspace};
use asana_openapi::fields::WorkspaceField;
use asana_openapi::ids::WorkspaceGid;

use super::request::{Request, opt_fields_setter};
use crate::Client;

pub use search::{CustomFieldFilter, SearchTasks};

impl Client {
    /// Workspaces and the task search.
    pub fn workspaces(&self) -> Workspaces<'_> {
        Workspaces { client: self }
    }
}

/// The workspaces handle: `client.workspaces()`.
#[derive(Clone, Copy, Debug)]
pub struct Workspaces<'a> {
    client: &'a Client,
}

opt_fields_setter!(GetWorkspaces => WorkspaceField, GetWorkspace => WorkspaceField);

impl<'a> Workspaces<'a> {
    /// The workspaces the token's user can reach, as a list to stream or page.
    ///
    /// `GET /workspaces` (`getWorkspaces`); scope `workspaces:read`; retried on a server error.
    pub fn list(&self) -> Request<'a, GetWorkspaces> {
        Request::new(self.client, GetWorkspaces::builder().build())
    }

    /// A workspace's full record.
    ///
    /// `GET /workspaces/{workspace_gid}` (`getWorkspace`); scope `workspaces:read`; retried on a
    /// server error.
    pub fn get(&self, workspace: WorkspaceGid) -> Request<'a, GetWorkspace> {
        Request::new(self.client, GetWorkspace::builder(workspace).build())
    }

    /// Searches a workspace's tasks; add filters with the request's setters, then `.send()`.
    ///
    /// `GET /workspaces/{workspace_gid}/tasks/search` (`searchTasksForWorkspace`); scope
    /// `tasks:read`; retried on a server error. Each call takes a token from the client's search
    /// limiter ([`Limits::searches_per_minute`](crate::Limits), under Asana's 60 a minute).
    ///
    /// - **Premium only.** Asana answers a token whose user is on no paid plan in that workspace
    ///   with `402`, which is [`Error::PremiumRequired`](crate::Error::PremiumRequired).
    /// - **Eventually consistent.** A change reaches search 10 to 60 seconds after it is made
    ///   (longer during incidents), so a task just created or moved may be missing or stale. For
    ///   read-your-write, list a project's or section's tasks instead.
    /// - **At most 100 results a call, and no offset.** Results are not in a stable order, so
    ///   Asana offers no `next_page`. To read more, page by time as Asana's documentation
    ///   describes: sort by creation time ascending, select `created_at`, and on the next call pass
    ///   the last task's `created_at` as [`created_at_after`](Request::created_at_after), dropping
    ///   any task already seen (Asana does not say whether the bound is inclusive).
    ///
    /// ```no_run
    /// # async fn run(client: asana_client::Client, workspace: asana_client::openapi::ids::WorkspaceGid,
    /// #     project: asana_client::openapi::ids::ProjectGid) -> Result<(), asana_client::Error> {
    /// use asana_client::fields;
    /// use asana_client::openapi::endpoints::SearchTasksForWorkspaceSortBy;
    ///
    /// let open = client
    ///     .workspaces()
    ///     .search_tasks(workspace)
    ///     .text("invoice")
    ///     .projects_any([project])
    ///     .completed(false)
    ///     .sort_by(SearchTasksForWorkspaceSortBy::CreatedAt)
    ///     .sort_ascending(true)
    ///     .fields(fields![Task: name, created_at])
    ///     .send()
    ///     .await?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn search_tasks(&self, workspace: WorkspaceGid) -> Request<'a, SearchTasks> {
        Request::new(
            self.client,
            SearchTasks::new(SearchTasksForWorkspace::builder(workspace).build()),
        )
    }
}
