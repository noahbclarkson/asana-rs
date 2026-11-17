//! The resources an event names: what kind each is, and its GID typed by that kind.

use std::fmt;

use asana_openapi::ids::{
    AttachmentGid, CustomFieldGid, EnumOptionGid, Gid, GoalGid, PortfolioGid, ProjectGid,
    SectionGid, StoryGid, TagGid, TaskGid, TeamGid, UserGid, WebhookGid, WorkspaceGid,
};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// An Asana `resource_type`, as used in events and in webhook filters.
///
/// The kinds this crate has a use for are named; any other is kept as written in
/// [`Other`](Self::Other), never refused.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ResourceType {
    /// `task` (subtasks and milestones too; `resource_subtype` tells them apart).
    Task,
    /// `section`.
    Section,
    /// `project`.
    Project,
    /// `story`: comments and the system's activity entries.
    Story,
    /// `tag`.
    Tag,
    /// `user`.
    User,
    /// `workspace`.
    Workspace,
    /// `custom_field`.
    CustomField,
    /// `enum_option`.
    EnumOption,
    /// `attachment`.
    Attachment,
    /// `portfolio`.
    Portfolio,
    /// `goal`.
    Goal,
    /// `team`.
    Team,
    /// `webhook`.
    Webhook,
    /// `project_template`.
    ProjectTemplate,
    /// `project_membership`.
    ProjectMembership,
    /// `team_membership`.
    TeamMembership,
    /// `workspace_membership`.
    WorkspaceMembership,
    /// Any other `resource_type`, as Asana wrote it.
    Other(String),
}

impl ResourceType {
    /// The `resource_type` as Asana writes it.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Task => "task",
            Self::Section => "section",
            Self::Project => "project",
            Self::Story => "story",
            Self::Tag => "tag",
            Self::User => "user",
            Self::Workspace => "workspace",
            Self::CustomField => "custom_field",
            Self::EnumOption => "enum_option",
            Self::Attachment => "attachment",
            Self::Portfolio => "portfolio",
            Self::Goal => "goal",
            Self::Team => "team",
            Self::Webhook => "webhook",
            Self::ProjectTemplate => "project_template",
            Self::ProjectMembership => "project_membership",
            Self::TeamMembership => "team_membership",
            Self::WorkspaceMembership => "workspace_membership",
            Self::Other(other) => other,
        }
    }

    /// Reads a `resource_type`; an unknown one becomes [`Other`](Self::Other).
    #[must_use]
    pub fn parse(text: &str) -> Self {
        match text {
            "task" => Self::Task,
            "section" => Self::Section,
            "project" => Self::Project,
            "story" => Self::Story,
            "tag" => Self::Tag,
            "user" => Self::User,
            "workspace" => Self::Workspace,
            "custom_field" => Self::CustomField,
            "enum_option" => Self::EnumOption,
            "attachment" => Self::Attachment,
            "portfolio" => Self::Portfolio,
            "goal" => Self::Goal,
            "team" => Self::Team,
            "webhook" => Self::Webhook,
            "project_template" => Self::ProjectTemplate,
            "project_membership" => Self::ProjectMembership,
            "team_membership" => Self::TeamMembership,
            "workspace_membership" => Self::WorkspaceMembership,
            other => Self::Other(other.to_owned()),
        }
    }
}

impl fmt::Display for ResourceType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for ResourceType {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ResourceType {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Ok(Self::parse(&text))
    }
}

/// A resource's GID, typed by its `resource_type`.
///
/// A kind without a GID type of its own is kept in [`Other`](Self::Other) with both strings as
/// Asana sent them.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ResourceGid {
    /// A task, subtask or milestone.
    Task(TaskGid),
    /// A section of a project.
    Section(SectionGid),
    /// A project.
    Project(ProjectGid),
    /// A story (a comment, or an activity entry).
    Story(StoryGid),
    /// A tag.
    Tag(TagGid),
    /// A user.
    User(UserGid),
    /// A workspace or organisation.
    Workspace(WorkspaceGid),
    /// A custom field.
    CustomField(CustomFieldGid),
    /// An option of an enum custom field.
    EnumOption(EnumOptionGid),
    /// An attachment.
    Attachment(AttachmentGid),
    /// A portfolio.
    Portfolio(PortfolioGid),
    /// A goal.
    Goal(GoalGid),
    /// A team.
    Team(TeamGid),
    /// A webhook.
    Webhook(WebhookGid),
    /// Any other kind of resource.
    Other {
        /// The `resource_type`, as Asana wrote it.
        resource_type: String,
        /// The GID, as Asana wrote it (unvalidated).
        gid: String,
    },
}

impl ResourceGid {
    /// Types `gid` by `resource_type`.
    ///
    /// # Errors
    ///
    /// [`asana_openapi::ids::GidError`] when the kind is one with a GID type and `gid` is not a
    /// valid GID. An unknown kind's GID is kept unvalidated.
    pub fn new(resource_type: &str, gid: &str) -> Result<Self, asana_openapi::ids::GidError> {
        Ok(match ResourceType::parse(resource_type) {
            ResourceType::Task => Self::Task(TaskGid::parse(gid)?),
            ResourceType::Section => Self::Section(SectionGid::parse(gid)?),
            ResourceType::Project => Self::Project(ProjectGid::parse(gid)?),
            ResourceType::Story => Self::Story(StoryGid::parse(gid)?),
            ResourceType::Tag => Self::Tag(TagGid::parse(gid)?),
            ResourceType::User => Self::User(UserGid::parse(gid)?),
            ResourceType::Workspace => Self::Workspace(WorkspaceGid::parse(gid)?),
            ResourceType::CustomField => Self::CustomField(CustomFieldGid::parse(gid)?),
            ResourceType::EnumOption => Self::EnumOption(EnumOptionGid::parse(gid)?),
            ResourceType::Attachment => Self::Attachment(AttachmentGid::parse(gid)?),
            ResourceType::Portfolio => Self::Portfolio(PortfolioGid::parse(gid)?),
            ResourceType::Goal => Self::Goal(GoalGid::parse(gid)?),
            ResourceType::Team => Self::Team(TeamGid::parse(gid)?),
            ResourceType::Webhook => Self::Webhook(WebhookGid::parse(gid)?),
            _ => Self::Other {
                resource_type: resource_type.to_owned(),
                gid: gid.to_owned(),
            },
        })
    }

    /// The `resource_type`, as Asana writes it.
    #[must_use]
    pub fn resource_type(&self) -> &str {
        match self {
            Self::Task(_) => TaskGid::RESOURCE_TYPE,
            Self::Section(_) => SectionGid::RESOURCE_TYPE,
            Self::Project(_) => ProjectGid::RESOURCE_TYPE,
            Self::Story(_) => StoryGid::RESOURCE_TYPE,
            Self::Tag(_) => TagGid::RESOURCE_TYPE,
            Self::User(_) => UserGid::RESOURCE_TYPE,
            Self::Workspace(_) => WorkspaceGid::RESOURCE_TYPE,
            Self::CustomField(_) => CustomFieldGid::RESOURCE_TYPE,
            Self::EnumOption(_) => EnumOptionGid::RESOURCE_TYPE,
            Self::Attachment(_) => AttachmentGid::RESOURCE_TYPE,
            Self::Portfolio(_) => PortfolioGid::RESOURCE_TYPE,
            Self::Goal(_) => GoalGid::RESOURCE_TYPE,
            Self::Team(_) => TeamGid::RESOURCE_TYPE,
            Self::Webhook(_) => WebhookGid::RESOURCE_TYPE,
            Self::Other { resource_type, .. } => resource_type,
        }
    }

    /// The kind of resource.
    #[must_use]
    pub fn kind(&self) -> ResourceType {
        ResourceType::parse(self.resource_type())
    }

    /// The GID as Asana wrote it.
    #[must_use]
    pub fn gid_str(&self) -> &str {
        match self {
            Self::Task(gid) => gid.as_str(),
            Self::Section(gid) => gid.as_str(),
            Self::Project(gid) => gid.as_str(),
            Self::Story(gid) => gid.as_str(),
            Self::Tag(gid) => gid.as_str(),
            Self::User(gid) => gid.as_str(),
            Self::Workspace(gid) => gid.as_str(),
            Self::CustomField(gid) => gid.as_str(),
            Self::EnumOption(gid) => gid.as_str(),
            Self::Attachment(gid) => gid.as_str(),
            Self::Portfolio(gid) => gid.as_str(),
            Self::Goal(gid) => gid.as_str(),
            Self::Team(gid) => gid.as_str(),
            Self::Webhook(gid) => gid.as_str(),
            Self::Other { gid, .. } => gid,
        }
    }

    /// The GID as `G` when this resource is of `G`'s kind, for example
    /// `resource.gid_as::<TaskGid>()`.
    #[must_use]
    pub fn gid_as<G: Gid>(&self) -> Option<G> {
        if self.resource_type() == G::RESOURCE_TYPE {
            G::parse(self.gid_str()).ok()
        } else {
            None
        }
    }
}

macro_rules! from_gid {
    ($($gid:ident => $variant:ident),* $(,)?) => {$(
        impl From<$gid> for ResourceGid {
            fn from(gid: $gid) -> Self {
                Self::$variant(gid)
            }
        }

        impl From<$gid> for ResourceRef {
            fn from(gid: $gid) -> Self {
                ResourceGid::$variant(gid).into()
            }
        }
    )*};
}

from_gid!(
    TaskGid => Task,
    SectionGid => Section,
    ProjectGid => Project,
    StoryGid => Story,
    TagGid => Tag,
    UserGid => User,
    WorkspaceGid => Workspace,
    CustomFieldGid => CustomField,
    EnumOptionGid => EnumOption,
    AttachmentGid => Attachment,
    PortfolioGid => Portfolio,
    GoalGid => Goal,
    TeamGid => Team,
    WebhookGid => Webhook,
);

/// A resource as an event names it: its typed GID, and the subtype and name when Asana sends them.
///
/// Events are compact: re-read the resource for anything more, because it may have changed again
/// since the event.
///
/// Build one from its GID with `ResourceRef::from(gid)` and set the rest.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct ResourceRef {
    /// The resource's kind and GID.
    pub gid: ResourceGid,
    /// `resource_subtype` (for a task, `default_task`, `milestone`, …; for a story, `comment_added`,
    /// `section_changed`, …), when sent.
    pub resource_subtype: Option<String>,
    /// `name`, when sent. Webhook events usually leave it out.
    pub name: Option<String>,
}

impl ResourceRef {
    /// The resource's kind.
    #[must_use]
    pub fn kind(&self) -> ResourceType {
        self.gid.kind()
    }

    /// The GID as `G` when the resource is of `G`'s kind.
    #[must_use]
    pub fn gid_as<G: Gid>(&self) -> Option<G> {
        self.gid.gid_as()
    }
}

impl From<ResourceGid> for ResourceRef {
    fn from(gid: ResourceGid) -> Self {
        Self {
            gid,
            resource_subtype: None,
            name: None,
        }
    }
}

/// The user who caused an event. Absent (`null`) for changes the system made itself.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct EventUser {
    /// The user's GID.
    pub gid: UserGid,
    /// The user's name, when sent.
    pub name: Option<String>,
}

impl EventUser {
    /// The user with this GID, without a name.
    #[must_use]
    pub fn new(gid: UserGid) -> Self {
        Self { gid, name: None }
    }
}
