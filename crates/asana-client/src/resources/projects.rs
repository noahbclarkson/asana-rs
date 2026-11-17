//! Projects: read, list, and their sections and tasks.
//!
//! `client.projects()` returns a [`Projects`] handle. A project's sections and tasks are
//! reachable from here too, and delegate to [`Sections`](crate::resources::sections::Sections)
//! and [`Tasks`](crate::resources::tasks::Tasks).

use asana_openapi::endpoints::{
    GetCustomFieldSettingsForProject, GetProject, GetProjects, GetSectionsForProject,
    GetTaskCountsForProject,
};
use asana_openapi::fields::{CustomFieldSettingField, Fields, ProjectField, TaskCountField};
use asana_openapi::ids::{ProjectGid, TeamGid, WorkspaceGid};

use crate::Client;
use crate::resources::request::{Request, opt_fields_setter};
use crate::resources::tasks::TaskList;

impl Client {
    /// Projects: read and list them, and reach their sections, tasks and custom fields.
    pub fn projects(&self) -> Projects<'_> {
        Projects { client: self }
    }
}

/// The project operations, from [`Client::projects`].
#[derive(Clone, Copy, Debug)]
pub struct Projects<'a> {
    client: &'a Client,
}

opt_fields_setter!(
    GetProject => ProjectField,
    GetProjects => ProjectField,
    GetCustomFieldSettingsForProject => CustomFieldSettingField,
    GetTaskCountsForProject => TaskCountField,
);

impl<'a> Projects<'a> {
    /// One project.
    ///
    /// Sends `getProject` (`GET /projects/{project_gid}`), scope `projects:read`. A read:
    /// retried on a 5xx or a lost connection.
    pub fn get(&self, project: ProjectGid) -> Request<'a, GetProject> {
        Request::new(self.client, GetProject::builder(project).build())
    }

    /// Projects in a workspace or a team; name one of them on the request. Read it with
    /// `.stream()`.
    ///
    /// Sends `getProjects` (`GET /projects`), scope `projects:read`. Retried.
    pub fn list(&self) -> Request<'a, GetProjects> {
        Request::new(self.client, GetProjects::builder().build())
    }

    /// A project's sections, as [`Sections::list`](crate::resources::sections::Sections::list).
    pub fn sections(&self, project: ProjectGid) -> Request<'a, GetSectionsForProject> {
        self.client.sections().list(project)
    }

    /// A project's tasks, as [`Tasks::list`](crate::resources::tasks::Tasks::list) with the
    /// project named. Add `.completed_since(..)` or `.modified_since(..)` to narrow it.
    pub fn tasks(&self, project: ProjectGid) -> TaskList<'a> {
        self.client.tasks().list().project(project)
    }

    /// The custom fields a project has, each as a setting that joins the field to the project.
    /// Ask for `custom_field.name` and `custom_field.resource_subtype` (or more) to read the
    /// fields themselves.
    ///
    /// Sends `getCustomFieldSettingsForProject`
    /// (`GET /projects/{project_gid}/custom_field_settings`), scope `projects:read`. Retried.
    pub fn custom_field_settings(
        &self,
        project: ProjectGid,
    ) -> Request<'a, GetCustomFieldSettingsForProject> {
        Request::new(
            self.client,
            GetCustomFieldSettingsForProject::builder(project).build(),
        )
    }

    /// How many tasks and milestones a project has, complete and incomplete. Every count is
    /// asked for unless the request chooses fewer with `.fields(..)`, because Asana returns none
    /// unless asked.
    ///
    /// Sends `getTaskCountsForProject` (`GET /projects/{project_gid}/task_counts`), scope
    /// `projects:read`. Retried.
    pub fn task_counts(&self, project: ProjectGid) -> Request<'a, GetTaskCountsForProject> {
        Request::new(
            self.client,
            GetTaskCountsForProject::builder(project)
                .opt_fields(TaskCountField::ALL.iter().copied().collect::<Fields<_>>())
                .build(),
        )
    }
}

impl Request<'_, GetProjects> {
    /// Projects in this workspace.
    pub fn workspace(mut self, workspace: WorkspaceGid) -> Self {
        self.endpoint_mut().workspace = Some(workspace);
        self
    }

    /// Projects in this team.
    pub fn team(mut self, team: TeamGid) -> Self {
        self.endpoint_mut().team = Some(team);
        self
    }

    /// Only archived projects (`true`) or only those not archived (`false`). Without it, both.
    pub fn archived(mut self, archived: bool) -> Self {
        self.endpoint_mut().archived = Some(archived);
        self
    }
}
