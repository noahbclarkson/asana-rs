//! Jobs: the asynchronous work Asana starts for templates and duplication.
//!
//! Instantiating a template, duplicating a task or project and exporting all answer at once with
//! a *job* and carry on in the background. The job already names the object it is making
//! (`new_task`, `new_project`), but that object may not be fully built until the job succeeds.
//! [`JobHandle::wait`] polls the job until it ends.
//!
//! Asana runs at most five such jobs at a time per user, the user's own work in the web app
//! included.

use std::time::Duration;

use asana_openapi::endpoint::Endpoint;
use asana_openapi::endpoints::GetJob;
use asana_openapi::fields::{Fields, JobField};
use asana_openapi::ids::{JobGid, ProjectGid, TaskGid};
use asana_openapi::models::{Job, JobStatus, Project, Task};
use tokio::time::Instant;

use crate::resources::request::{Request, opt_fields_setter};
use crate::{Client, Error, Require};

/// The first wait between polls; it doubles after each poll that finds the job running.
const FIRST_POLL_WAIT: Duration = Duration::from_millis(500);
/// The longest wait between polls.
const MAX_POLL_WAIT: Duration = Duration::from_secs(5);

impl Client {
    /// Jobs: read the state of an asynchronous job.
    pub fn jobs(&self) -> Jobs<'_> {
        Jobs { client: self }
    }
}

/// The job operations, from [`Client::jobs`].
#[derive(Clone, Copy, Debug)]
pub struct Jobs<'a> {
    client: &'a Client,
}

opt_fields_setter!(GetJob => JobField);

impl<'a> Jobs<'a> {
    /// One job's current state.
    ///
    /// Sends `getJob` (`GET /jobs/{job_gid}`); scope `jobs:read`; retried like every read.
    pub fn get(&self, job: JobGid) -> Request<'a, GetJob> {
        Request::new(self.client, GetJob::builder(job).build())
    }
}

/// A request that starts a job (an instantiation, a duplication): sent, it answers with a
/// [`JobHandle`] instead of the bare [`Job`].
///
/// The operation's own setters are methods of this type, and [`map`](Self::map) reaches any
/// parameter they do not cover.
#[must_use = "a request does nothing until it is sent"]
#[derive(Debug)]
pub struct StartJob<'a, E> {
    request: Request<'a, E>,
}

impl<'a, E: Endpoint<Output = Job>> StartJob<'a, E> {
    /// Wraps a request whose operation answers with a job.
    pub fn new(request: Request<'a, E>) -> Self {
        Self { request }
    }

    /// Changes the descriptor in place, for a parameter no setter covers.
    pub fn map(mut self, change: impl FnOnce(&mut E)) -> Self {
        change(self.request.endpoint_mut());
        self
    }

    /// The descriptor alone, for example to put it in a batch.
    pub fn into_endpoint(self) -> E {
        self.request.into_endpoint()
    }

    /// Sends the request and returns the job it started. The job is not waited on.
    ///
    /// # Errors
    ///
    /// As [`Client::send`], and [`Error::MissingField`] when the answer names no job.
    pub async fn send(self) -> Result<JobHandle, Error> {
        JobHandle::from_job(self.request.send().await?)
    }
}

/// A job Asana has started, to wait on or to read the object it is making.
#[derive(Clone, Debug, PartialEq)]
pub struct JobHandle {
    gid: JobGid,
    job: Job,
}

impl JobHandle {
    /// Wraps the job an operation answered with (an instantiation, a duplication).
    ///
    /// # Errors
    ///
    /// [`Error::MissingField`] when the job has no `gid`.
    pub fn from_job(job: Job) -> Result<Self, Error> {
        let gid = job.gid.clone().require("gid")?;
        Ok(Self { gid, job })
    }

    /// The job's GID.
    pub fn gid(&self) -> &JobGid {
        &self.gid
    }

    /// The job as Asana first returned it.
    pub fn job(&self) -> &Job {
        &self.job
    }

    /// The GID of the task the job is making, as Asana reported it when the job started, without
    /// waiting.
    ///
    /// The task exists under this GID at once, but until the job succeeds it may lack its
    /// subtasks, dependencies, attachments or custom field values. Use [`wait`](Self::wait) when
    /// the task must be complete.
    pub fn new_task_gid(&self) -> Option<&TaskGid> {
        self.job.new_task.as_ref()?.gid.as_ref()
    }

    /// The GID of the project the job is making, as Asana reported it when the job started.
    /// The same caveat as [`new_task_gid`](Self::new_task_gid) applies.
    pub fn new_project_gid(&self) -> Option<&ProjectGid> {
        self.job.new_project.as_ref()?.gid.as_ref()
    }

    /// Polls the job until it succeeds or fails, or until `timeout` has passed, and returns its
    /// final record.
    ///
    /// Each poll is `getJob` (`GET /jobs/{job_gid}`, scope `jobs:read`, retried like every read).
    /// The first is sent at once; the waits between them start at half a second and double up to
    /// five seconds, on tokio's clock (so a test can pause time). A status this crate does not
    /// know is treated as still running.
    ///
    /// # Errors
    ///
    /// [`Error::JobFailed`] when the job ends `failed`; [`Error::JobTimedOut`] when it is still
    /// running at `timeout` (it may yet finish, and [`wait`](Self::wait) can be called again);
    /// otherwise any error reading the job.
    pub async fn wait(&self, client: &Client, timeout: Duration) -> Result<Job, Error> {
        let started = Instant::now();
        let mut pause = FIRST_POLL_WAIT;
        loop {
            let job = client.send(self.poll()).await?;
            match job.status {
                Some(JobStatus::Succeeded) => return Ok(job),
                Some(JobStatus::Failed) => {
                    return Err(Error::JobFailed {
                        job: self.gid.clone(),
                    });
                }
                _ => {}
            }
            let waited = started.elapsed();
            if waited + pause > timeout {
                return Err(Error::JobTimedOut {
                    job: self.gid.clone(),
                    waited,
                });
            }
            tokio::time::sleep(pause).await;
            pause = (pause * 2).min(MAX_POLL_WAIT);
        }
    }

    /// Waits as [`wait`](Self::wait) does and returns the task the job made (its compact record:
    /// GID, name and type).
    ///
    /// # Errors
    ///
    /// As [`wait`](Self::wait), and [`Error::MissingField`] when the finished job names no task.
    pub async fn wait_for_task(&self, client: &Client, timeout: Duration) -> Result<Task, Error> {
        self.wait(client, timeout)
            .await?
            .new_task
            .map(|task| *task)
            .require("new_task")
    }

    /// Waits as [`wait`](Self::wait) does and returns the project the job made (its compact
    /// record).
    ///
    /// # Errors
    ///
    /// As [`wait`](Self::wait), and [`Error::MissingField`] when the finished job names no project.
    pub async fn wait_for_project(
        &self,
        client: &Client,
        timeout: Duration,
    ) -> Result<Project, Error> {
        self.wait(client, timeout)
            .await?
            .new_project
            .map(|project| *project)
            .require("new_project")
    }

    fn poll(&self) -> GetJob {
        GetJob::builder(self.gid.clone())
            .opt_fields(
                Fields::none()
                    .with(JobField::Status)
                    .with(JobField::ResourceSubtype)
                    .with(JobField::NewTask)
                    .with(JobField::NewTaskName)
                    .with(JobField::NewProject)
                    .with(JobField::NewProjectName),
            )
            .build()
    }
}
