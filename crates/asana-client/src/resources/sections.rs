//! Sections of a project: list, find by name, create, and moving a task in.
//!
//! `client.sections()` returns a [`Sections`] handle. Moving a task into a section is
//! [`Sections::add_task`]; finding a section by the name people see is
//! [`Sections::find_by_name`]; and a receiver that looks a section up on every webhook delivery
//! keeps a [`SectionDirectory`] rather than listing the project each time.

mod directory;

use asana_openapi::endpoints::{
    AddTaskForSection, CreateSectionForProject, DeleteSection, GetSection, GetSectionsForProject,
    UpdateSection,
};
use asana_openapi::fields::SectionField;
use asana_openapi::ids::{ProjectGid, SectionGid, TaskGid};
use asana_openapi::models::{Section, SectionRequest, SectionTaskInsertRequest};
use futures_util::TryStreamExt;

use crate::resources::request::{Request, opt_fields_setter};
use crate::{Client, Error, NameMatch};

pub use directory::{Clock, SectionDirectory, SystemClock};

impl Client {
    /// Sections of projects: list, find by name, create, and move tasks into them.
    pub fn sections(&self) -> Sections<'_> {
        Sections { client: self }
    }
}

/// The section operations, from [`Client::sections`].
#[derive(Clone, Copy, Debug)]
pub struct Sections<'a> {
    client: &'a Client,
}

opt_fields_setter!(
    GetSectionsForProject => SectionField,
    GetSection => SectionField,
    CreateSectionForProject => SectionField,
    UpdateSection => SectionField,
);

impl<'a> Sections<'a> {
    /// A project's sections, in the order the project shows them. Read it with `.stream()`.
    ///
    /// Sends `getSectionsForProject` (`GET /projects/{project_gid}/sections`), scope
    /// `projects:read`. A read: retried on a 5xx or a lost connection.
    pub fn list(&self, project: ProjectGid) -> Request<'a, GetSectionsForProject> {
        Request::new(self.client, GetSectionsForProject::builder(project).build())
    }

    /// One section.
    ///
    /// Sends `getSection` (`GET /sections/{section_gid}`), scope `projects:read`. Retried.
    pub fn get(&self, section: SectionGid) -> Request<'a, GetSection> {
        Request::new(self.client, GetSection::builder(section).build())
    }

    /// Creates a section in a project, at the end unless the request places it before or after
    /// another section.
    ///
    /// Sends `createSectionForProject` (`POST /projects/{project_gid}/sections`), scope
    /// `projects:write`. Not retried after a 5xx or a lost answer, because sending it twice
    /// would make two sections.
    pub fn create(
        &self,
        project: ProjectGid,
        name: impl Into<String>,
    ) -> Request<'a, CreateSectionForProject> {
        let mut data = SectionRequest::default();
        data.name = Some(name.into());
        Request::new(
            self.client,
            CreateSectionForProject::builder(project).data(data).build(),
        )
    }

    /// Changes a section: its name, or where it sits.
    ///
    /// Sends `updateSection` (`PUT /sections/{section_gid}`), scope `projects:write`. Retried.
    pub fn update(&self, section: SectionGid) -> Request<'a, UpdateSection> {
        Request::new(
            self.client,
            UpdateSection::builder(section)
                .data(SectionRequest::default())
                .build(),
        )
    }

    /// Deletes a section. Asana refuses unless the section is empty and not the project's last.
    ///
    /// Sends `deleteSection` (`DELETE /sections/{section_gid}`), scope `projects:write`.
    /// Retried; a `404` to a retry after an attempt whose answer was lost counts as deleted.
    pub fn delete(&self, section: SectionGid) -> Request<'a, DeleteSection> {
        Request::new(self.client, DeleteSection::builder(section).build())
    }

    /// Moves a task into a section, at the end of it unless the request places it before or
    /// after another task of the section. The task leaves whatever section of the same project
    /// it was in; it must already be in the section's project, or this adds it.
    ///
    /// Sends `addTaskForSection` (`POST /sections/{section_gid}/addTask`), scope `tasks:write`.
    /// Moving a task where it already is changes nothing, so this is retried safely.
    pub fn add_task(&self, section: SectionGid, task: TaskGid) -> Request<'a, AddTaskForSection> {
        let mut data = SectionTaskInsertRequest::default();
        data.task = Some(task);
        Request::new(
            self.client,
            AddTaskForSection::builder(section).data(data).build(),
        )
    }

    /// Finds a project's section by the name people see: an exact match after trimming spaces at
    /// both ends, case-sensitive unless the request says
    /// [`ignore_case`](FindSection::ignore_case).
    ///
    /// Lists the project's sections (`getSectionsForProject`, scope `projects:read`, retried) and
    /// compares names; to look sections up often, keep a [`SectionDirectory`].
    pub fn find_by_name(&self, project: ProjectGid, name: impl Into<String>) -> FindSection<'a> {
        FindSection {
            client: self.client,
            project,
            name: name.into(),
            ignore_case: false,
        }
    }

    /// A cache of a project's sections by name and GID that lists the project again once
    /// `ttl` has passed. See [`SectionDirectory`].
    pub fn directory(&self, project: ProjectGid, ttl: std::time::Duration) -> SectionDirectory {
        SectionDirectory::new(self.client.clone(), project, ttl)
    }
}

impl Request<'_, CreateSectionForProject> {
    /// Puts the new section just before `section`.
    pub fn insert_before(mut self, section: SectionGid) -> Self {
        self.endpoint_mut()
            .data
            .get_or_insert_with(Default::default)
            .insert_before = Some(section.into());
        self
    }

    /// Puts the new section just after `section`.
    pub fn insert_after(mut self, section: SectionGid) -> Self {
        self.endpoint_mut()
            .data
            .get_or_insert_with(Default::default)
            .insert_after = Some(section.into());
        self
    }
}

impl Request<'_, UpdateSection> {
    /// The section's new name. It cannot be empty.
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.endpoint_mut()
            .data
            .get_or_insert_with(Default::default)
            .name = Some(name.into());
        self
    }
}

impl Request<'_, AddTaskForSection> {
    /// Puts the task just before `task`, which must be in the section.
    pub fn insert_before(mut self, task: TaskGid) -> Self {
        self.endpoint_mut()
            .data
            .get_or_insert_with(Default::default)
            .insert_before = Some(task.into());
        self
    }

    /// Puts the task just after `task`, which must be in the section.
    pub fn insert_after(mut self, task: TaskGid) -> Self {
        self.endpoint_mut()
            .data
            .get_or_insert_with(Default::default)
            .insert_after = Some(task.into());
        self
    }
}

/// Whether a section's name is the name looked for: trimmed at both ends, and compared exactly
/// or ignoring case.
fn same_name(candidate: &str, wanted: &str, ignore_case: bool) -> bool {
    let (candidate, wanted) = (candidate.trim(), wanted.trim());
    if ignore_case {
        candidate.to_lowercase() == wanted.to_lowercase()
    } else {
        candidate == wanted
    }
}

/// A search of a project's sections by name, from [`Sections::find_by_name`].
#[must_use = "a search does nothing until it is sent"]
#[derive(Debug)]
pub struct FindSection<'a> {
    client: &'a Client,
    project: ProjectGid,
    name: String,
    ignore_case: bool,
}

impl FindSection<'_> {
    /// Compares names ignoring case ("send documents" finds "Send Documents").
    pub fn ignore_case(mut self) -> Self {
        self.ignore_case = true;
        self
    }

    /// Lists the project's sections and returns the ones with the name: none, one or several.
    ///
    /// # Errors
    ///
    /// As [`Client::send`], for any page of the listing.
    pub async fn send(self) -> Result<NameMatch<Section>, Error> {
        let sections: Vec<Section> = self
            .client
            .sections()
            .list(self.project)
            .fields(asana_openapi::fields![Section: name])
            .stream()
            .try_collect()
            .await?;
        let found = sections
            .into_iter()
            .filter(|section| {
                section
                    .name
                    .as_deref()
                    .is_some_and(|name| same_name(name, &self.name, self.ignore_case))
            })
            .collect();
        Ok(NameMatch::from_matches(found))
    }
}
