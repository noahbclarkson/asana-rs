//! Task and project templates: listing them and making a task or project from one.
//!
//! Instantiating a template starts a [job](crate::resources::jobs): the request answers at once
//! with a [`JobHandle`], which can report the new object's GID straight away or wait for the job
//! to finish.
//!
//! ```no_run
//! # async fn run(client: asana_client::Client, template: asana_client::openapi::ids::TaskTemplateGid)
//! #     -> Result<(), asana_client::Error> {
//! use std::time::Duration;
//!
//! let job = client.task_templates().instantiate(template).name("Acme: onboarding").send().await?;
//! let task = job.wait_for_task(&client, Duration::from_secs(30)).await?;
//! # let _ = task; Ok(()) }
//! ```

use asana_openapi::endpoints::{
    GetProjectTemplate, GetProjectTemplates, GetProjectTemplatesForTeam, GetTaskTemplate,
    GetTaskTemplates, InstantiateProject, InstantiateTask,
};
use asana_openapi::fields::{JobField, ProjectTemplateField, TaskTemplateField};
use asana_openapi::ids::{ProjectGid, ProjectTemplateGid, TaskTemplateGid, TeamGid, WorkspaceGid};
use asana_openapi::models::{
    ProjectTemplateInstantiateProjectRequest, TaskTemplateInstantiateTaskRequest,
};

use crate::Client;
use crate::resources::jobs::StartJob;
use crate::resources::request::{Request, opt_fields_setter};

#[cfg(doc)]
use crate::resources::jobs::JobHandle;

impl Client {
    /// Task templates: list them and make a task from one.
    pub fn task_templates(&self) -> TaskTemplates<'_> {
        TaskTemplates { client: self }
    }

    /// Project templates: list them and make a project from one.
    pub fn project_templates(&self) -> ProjectTemplates<'_> {
        ProjectTemplates { client: self }
    }
}

opt_fields_setter!(
    GetTaskTemplates => TaskTemplateField,
    GetTaskTemplate => TaskTemplateField,
    GetProjectTemplates => ProjectTemplateField,
    GetProjectTemplatesForTeam => ProjectTemplateField,
    GetProjectTemplate => ProjectTemplateField,
);

/// The task template operations, from [`Client::task_templates`].
#[derive(Clone, Copy, Debug)]
pub struct TaskTemplates<'a> {
    client: &'a Client,
}

impl<'a> TaskTemplates<'a> {
    /// The task templates of a project, as a list (`.stream()`, `.collect_max(n)`).
    ///
    /// Sends `getTaskTemplates` (`GET /task_templates?project=…`); scope `task_templates:read`;
    /// retried like every read.
    pub fn list(&self, project: ProjectGid) -> Request<'a, GetTaskTemplates> {
        Request::new(
            self.client,
            GetTaskTemplates::builder().project(project).build(),
        )
    }

    /// One task template.
    ///
    /// Sends `getTaskTemplate` (`GET /task_templates/{task_template_gid}`); scope
    /// `task_templates:read`; retried like every read.
    pub fn get(&self, template: TaskTemplateGid) -> Request<'a, GetTaskTemplate> {
        Request::new(self.client, GetTaskTemplate::builder(template).build())
    }

    /// Makes a task from a template. Sent, it returns a [`JobHandle`]; the task is named after
    /// the template unless [`name`](StartJob::name) is set.
    ///
    /// Sends `instantiateTask` (`POST /task_templates/{task_template_gid}/instantiateTask`); the
    /// specification names no granular scope; never retried after a failure that leaves the
    /// outcome unknown, because a second send would make a second task.
    pub fn instantiate(&self, template: TaskTemplateGid) -> StartJob<'a, InstantiateTask> {
        StartJob::new(Request::new(
            self.client,
            InstantiateTask::builder(template).build(),
        ))
    }
}

impl StartJob<'_, InstantiateTask> {
    /// The new task's name.
    pub fn name(self, name: impl Into<String>) -> Self {
        let name = name.into();
        self.map(|endpoint| {
            endpoint
                .data
                .get_or_insert_with(TaskTemplateInstantiateTaskRequest::default)
                .name = Some(name);
        })
    }

    /// Chooses the job's fields Asana returns (`opt_fields`).
    pub fn fields(self, fields: asana_openapi::fields::Fields<JobField>) -> Self {
        self.map(|endpoint| endpoint.opt_fields = fields)
    }
}

/// The project template operations, from [`Client::project_templates`].
#[derive(Clone, Copy, Debug)]
pub struct ProjectTemplates<'a> {
    client: &'a Client,
}

impl<'a> ProjectTemplates<'a> {
    /// The project templates of a workspace, as a list.
    ///
    /// Sends `getProjectTemplates` (`GET /project_templates?workspace=…`); scope
    /// `project_templates:read`; retried like every read.
    pub fn list(&self, workspace: WorkspaceGid) -> Request<'a, GetProjectTemplates> {
        Request::new(
            self.client,
            GetProjectTemplates::builder().workspace(workspace).build(),
        )
    }

    /// The project templates of a team, as a list.
    ///
    /// Sends `getProjectTemplatesForTeam` (`GET /teams/{team_gid}/project_templates`); scope
    /// `project_templates:read`; retried like every read.
    pub fn list_for_team(&self, team: TeamGid) -> Request<'a, GetProjectTemplatesForTeam> {
        Request::new(
            self.client,
            GetProjectTemplatesForTeam::builder(team).build(),
        )
    }

    /// One project template.
    ///
    /// Sends `getProjectTemplate` (`GET /project_templates/{project_template_gid}`); scope
    /// `project_templates:read`; retried like every read.
    pub fn get(&self, template: ProjectTemplateGid) -> Request<'a, GetProjectTemplate> {
        Request::new(self.client, GetProjectTemplate::builder(template).build())
    }

    /// Makes a project from a template. Sent, it returns a [`JobHandle`].
    ///
    /// Sends `instantiateProject`
    /// (`POST /project_templates/{project_template_gid}/instantiateProject`); scope
    /// `projects:write`; never retried after a failure that leaves the outcome unknown.
    pub fn instantiate(&self, template: ProjectTemplateGid) -> StartJob<'a, InstantiateProject> {
        StartJob::new(Request::new(
            self.client,
            InstantiateProject::builder(template).build(),
        ))
    }
}

impl StartJob<'_, InstantiateProject> {
    /// The new project's name.
    pub fn name(self, name: impl Into<String>) -> Self {
        let name = name.into();
        self.map(|endpoint| project_request(endpoint).name = Some(name))
    }

    /// The team the new project belongs to.
    pub fn team(self, team: TeamGid) -> Self {
        self.map(|endpoint| project_request(endpoint).team = Some(team))
    }

    /// Chooses the job's fields Asana returns (`opt_fields`).
    pub fn fields(self, fields: asana_openapi::fields::Fields<JobField>) -> Self {
        self.map(|endpoint| endpoint.opt_fields = fields)
    }
}

fn project_request(
    endpoint: &mut InstantiateProject,
) -> &mut ProjectTemplateInstantiateProjectRequest {
    endpoint
        .data
        .get_or_insert_with(ProjectTemplateInstantiateProjectRequest::default)
}
