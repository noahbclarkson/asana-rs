//! Tags: create and find by name in a workspace.
//!
//! `client.tags()` makes, reads and lists a workspace's tags, and finds one by name. Adding a tag
//! to a task is the tasks handle's.

use asana_openapi::endpoints::{CreateTagForWorkspace, GetTag, GetTagsForWorkspace};
use asana_openapi::fields;
use asana_openapi::fields::TagField;
use asana_openapi::ids::{TagGid, WorkspaceGid};
use asana_openapi::models::{Tag, TagColor, TagCreateTagForWorkspaceRequest};
use futures_util::StreamExt;

use super::custom_fields::NameMatch;
use super::request::{Request, opt_fields_setter};
use crate::{Client, Error};

impl Client {
    /// Tags.
    pub fn tags(&self) -> Tags<'_> {
        Tags { client: self }
    }
}

/// The tags handle: `client.tags()`.
#[derive(Clone, Copy, Debug)]
pub struct Tags<'a> {
    client: &'a Client,
}

opt_fields_setter!(
    CreateTagForWorkspace => TagField,
    GetTag => TagField,
    GetTagsForWorkspace => TagField,
);

impl<'a> Tags<'a> {
    /// Makes a tag named `name` in a workspace.
    ///
    /// `POST /workspaces/{workspace_gid}/tags` (`createTagForWorkspace`); scope `tags:write`;
    /// never retried on a server error, because it creates.
    pub fn create(
        &self,
        workspace: WorkspaceGid,
        name: impl Into<String>,
    ) -> Request<'a, CreateTagForWorkspace> {
        Request::new(
            self.client,
            CreateTagForWorkspace::builder(workspace)
                .data(
                    TagCreateTagForWorkspaceRequest::builder()
                        .name(name)
                        .build(),
                )
                .build(),
        )
    }

    /// A tag's full record.
    ///
    /// `GET /tags/{tag_gid}` (`getTag`); scope `tags:read`; retried on a server error.
    pub fn get(&self, tag: TagGid) -> Request<'a, GetTag> {
        Request::new(self.client, GetTag::builder(tag).build())
    }

    /// A workspace's tags, as a list to stream or page.
    ///
    /// `GET /workspaces/{workspace_gid}/tags` (`getTagsForWorkspace`); scope `tags:read`;
    /// retried on a server error.
    pub fn list(&self, workspace: WorkspaceGid) -> Request<'a, GetTagsForWorkspace> {
        Request::new(self.client, GetTagsForWorkspace::builder(workspace).build())
    }

    /// The workspace's tags named `name`: compared exactly, case included, after trimming spaces
    /// at both ends (as section lookups do).
    ///
    /// Reads every tag of the workspace, 100 to a page (`getTagsForWorkspace`, scope
    /// `tags:read`, retried on a server error), so it costs one request per hundred tags. Asana
    /// lets several tags share a name, which [`NameMatch::Several`] reports.
    ///
    /// # Errors
    ///
    /// The first error any page met, as [`Client::send`].
    pub async fn find_by_name(
        &self,
        workspace: &WorkspaceGid,
        name: &str,
    ) -> Result<NameMatch<Tag>, Error> {
        let mut tags = self
            .list(workspace.clone())
            .fields(fields![Tag: name])
            .stream();
        let mut matches = Vec::new();
        while let Some(tag) = tags.next().await {
            let tag = tag?;
            if tag
                .name
                .as_deref()
                .is_some_and(|own| own.trim() == name.trim())
            {
                matches.push(tag);
            }
        }
        Ok(NameMatch::from_matches(matches))
    }

    /// The tag named `name`, made if the workspace has none. When several share the name, the
    /// first in Asana's order is returned.
    ///
    /// Reads the tags as [`find_by_name`](Self::find_by_name) does, then creates one when none
    /// matched (`createTagForWorkspace`, scope `tags:write`, never retried). When the creation
    /// fails for any reason, the tags are read again and a tag of that name found then is
    /// returned instead of the error. Asana does not refuse a second tag of the same name, so two
    /// callers racing can each make one; later calls return the first.
    ///
    /// # Errors
    ///
    /// The error of the first read; or the creation's error when the second read finds no tag of
    /// that name either.
    pub async fn ensure(&self, workspace: &WorkspaceGid, name: &str) -> Result<Tag, Error> {
        if let Some(found) = self.find_by_name(workspace, name).await?.first() {
            return Ok(found);
        }
        match self.create(workspace.clone(), name).send().await {
            Ok(created) => Ok(created),
            Err(error) => match self.find_by_name(workspace, name).await {
                Ok(found) => found.first().ok_or(error),
                Err(_) => Err(error),
            },
        }
    }
}

impl Request<'_, CreateTagForWorkspace> {
    /// The tag's colour.
    pub fn color(self, color: TagColor) -> Self {
        self.map(|endpoint| endpoint.data.color = Some(color.into()))
    }

    /// The tag's notes (its description).
    pub fn notes(self, notes: impl Into<String>) -> Self {
        self.map(|endpoint| endpoint.data.notes = Some(notes.into()))
    }
}
