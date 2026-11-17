//! Webhook filters: which events a webhook delivers, and Asana's rules about them.
//!
//! A filter is sent in the `filters` array when a webhook is created, as
//! `{resource_type, resource_subtype, action, fields}`. An event is delivered when it passes **any**
//! of the webhook's filters, and every event passes when there are none.
//!
//! Asana's rules, which [`validate_filters`] checks before a request is spent on them:
//!
//! - A webhook on a **workspace, team, portfolio or goal** (a "higher-level" webhook) must have at
//!   least one filter, and each must name a `resource_type` from this table with an `action` it
//!   allows (or no action). Task, subtask and story events never reach these webhooks, and their
//!   filters cannot list `fields`.
//!
//!   | `resource_type` | actions |
//!   |---|---|
//!   | `goal` | added, removed, deleted, undeleted, changed |
//!   | `portfolio` | added, removed, deleted |
//!   | `project` | added, removed, deleted, undeleted, changed |
//!   | `project_template` | added, removed, deleted, undeleted, changed |
//!   | `team_membership` | added, removed |
//!   | `workspace_membership` | added, removed |
//!
//! - Anywhere, `fields` is only valid on a filter whose `action` is `changed`.
//!
//! Webhooks on a project, section, task or any other resource see everything below them and need
//! no filter.

use serde::{Deserialize, Serialize};

use crate::events::{EventAction, ResourceType};

/// One webhook filter, built with the shortcut constructors and the chained setters.
///
/// ```
/// use asana_webhooks::{EventAction, Filter};
///
/// let filters = [
///     Filter::task().action(EventAction::Added),
///     Filter::task().action(EventAction::Changed).fields(["due_on", "assignee"]),
/// ];
/// assert_eq!(
///     serde_json::to_string(&filters[1]).unwrap(),
///     r#"{"resource_type":"task","action":"changed","fields":["due_on","assignee"]}"#,
/// );
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Filter {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    resource_type: Option<ResourceType>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    resource_subtype: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    action: Option<EventAction>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    fields: Vec<String>,
}

impl Filter {
    /// A filter on events from resources of `resource_type`.
    #[must_use]
    pub fn new(resource_type: ResourceType) -> Self {
        Self {
            resource_type: Some(resource_type),
            ..Self::default()
        }
    }

    /// A filter that names no `resource_type`: it matches on the action and fields alone. Refused
    /// on higher-level webhooks.
    #[must_use]
    pub fn any_resource() -> Self {
        Self::default()
    }

    /// A filter on task events (subtasks and milestones included, unless a subtype is set).
    #[must_use]
    pub fn task() -> Self {
        Self::new(ResourceType::Task)
    }

    /// A filter on section events.
    #[must_use]
    pub fn section() -> Self {
        Self::new(ResourceType::Section)
    }

    /// A filter on project events.
    #[must_use]
    pub fn project() -> Self {
        Self::new(ResourceType::Project)
    }

    /// A filter on story events (comments and activity entries).
    #[must_use]
    pub fn story() -> Self {
        Self::new(ResourceType::Story)
    }

    /// A filter on tag events.
    #[must_use]
    pub fn tag() -> Self {
        Self::new(ResourceType::Tag)
    }

    /// Only resources with this `resource_subtype` (for a task, `default_task`, `milestone`,
    /// `approval`; for a story, `comment_added`, …).
    #[must_use]
    pub fn subtype(mut self, resource_subtype: impl Into<String>) -> Self {
        self.resource_subtype = Some(resource_subtype.into());
        self
    }

    /// Only events with this action.
    #[must_use]
    pub fn action(mut self, action: EventAction) -> Self {
        self.action = Some(action);
        self
    }

    /// Only `changed` events on this field; may be called more than once.
    #[must_use]
    pub fn field(mut self, field: impl Into<String>) -> Self {
        self.fields.push(field.into());
        self
    }

    /// Only `changed` events on these fields.
    #[must_use]
    pub fn fields<I: IntoIterator<Item = S>, S: Into<String>>(mut self, fields: I) -> Self {
        self.fields.extend(fields.into_iter().map(Into::into));
        self
    }

    /// The `resource_type` filtered on, if any.
    #[must_use]
    pub fn resource_type(&self) -> Option<&ResourceType> {
        self.resource_type.as_ref()
    }

    /// The `resource_subtype` filtered on, if any.
    #[must_use]
    pub fn resource_subtype(&self) -> Option<&str> {
        self.resource_subtype.as_deref()
    }

    /// The action filtered on, if any.
    #[must_use]
    pub fn action_filter(&self) -> Option<&EventAction> {
        self.action.as_ref()
    }

    /// The fields filtered on; empty for none.
    #[must_use]
    pub fn field_list(&self) -> &[String] {
        &self.fields
    }

    /// Checks this one filter against Asana's rules for a webhook on a resource of kind `target`.
    ///
    /// # Errors
    ///
    /// [`FilterError`] naming the rule broken.
    pub fn validate_for(&self, target: &ResourceType) -> Result<(), FilterError> {
        if !self.fields.is_empty() && self.action != Some(EventAction::Changed) {
            return Err(FilterError::FieldsNeedChanged);
        }
        if !is_higher_level(target) {
            return Ok(());
        }
        if !self.fields.is_empty() {
            return Err(FilterError::FieldsOnHigherLevel);
        }
        let Some(resource_type) = &self.resource_type else {
            return Err(FilterError::ResourceTypeRequired);
        };
        let allowed: &[EventAction] = match resource_type {
            ResourceType::Task | ResourceType::Story => {
                return Err(FilterError::NeverPropagated(resource_type.clone()));
            }
            ResourceType::Goal | ResourceType::Project | ResourceType::ProjectTemplate => &[
                EventAction::Added,
                EventAction::Removed,
                EventAction::Deleted,
                EventAction::Undeleted,
                EventAction::Changed,
            ],
            ResourceType::Portfolio => &[
                EventAction::Added,
                EventAction::Removed,
                EventAction::Deleted,
            ],
            ResourceType::TeamMembership | ResourceType::WorkspaceMembership => {
                &[EventAction::Added, EventAction::Removed]
            }
            _ => &[],
        };
        let action_allowed = match &self.action {
            None => !allowed.is_empty(),
            Some(action) => allowed.contains(action),
        };
        if action_allowed {
            Ok(())
        } else {
            Err(FilterError::NotAllowedOnHigherLevel {
                resource_type: resource_type.clone(),
                action: self.action.clone(),
            })
        }
    }
}

/// Whether a webhook on `target` is a higher-level webhook, which must have filters and never
/// sees task, subtask or story events.
#[must_use]
pub fn is_higher_level(target: &ResourceType) -> bool {
    matches!(
        target,
        ResourceType::Workspace | ResourceType::Team | ResourceType::Portfolio | ResourceType::Goal
    )
}

/// Checks a webhook's whole filter list against Asana's rules for a webhook on `target`.
///
/// # Errors
///
/// [`FilterError::FiltersRequired`] when a higher-level webhook has none, or the first filter's
/// [`FilterError`].
pub fn validate_filters(target: &ResourceType, filters: &[Filter]) -> Result<(), FilterError> {
    if filters.is_empty() && is_higher_level(target) {
        return Err(FilterError::FiltersRequired(target.clone()));
    }
    filters
        .iter()
        .try_for_each(|filter| filter.validate_for(target))
}

/// A webhook filter Asana would refuse, or one that could never match.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum FilterError {
    /// Webhooks on workspaces, teams, portfolios and goals must have at least one filter.
    #[error("a webhook on a {0} must have at least one filter")]
    FiltersRequired(ResourceType),
    /// `fields` is only valid with `action: changed`.
    #[error("a filter can list fields only when its action is `changed`")]
    FieldsNeedChanged,
    /// Higher-level webhooks do not support `fields`.
    #[error("a filter on a workspace, team, portfolio or goal webhook cannot list fields")]
    FieldsOnHigherLevel,
    /// Higher-level webhooks need each filter to name a `resource_type`.
    #[error("a filter on a workspace, team, portfolio or goal webhook must name a resource type")]
    ResourceTypeRequired,
    /// Task, subtask and story events never reach a higher-level webhook.
    #[error("{0} events never reach a workspace, team, portfolio or goal webhook")]
    NeverPropagated(ResourceType),
    /// The resource type and action are not a combination higher-level webhooks support.
    #[error(
        "a workspace, team, portfolio or goal webhook cannot filter on this resource type and action"
    )]
    NotAllowedOnHigherLevel {
        /// The filter's resource type.
        resource_type: ResourceType,
        /// The filter's action, if it named one.
        action: Option<EventAction>,
    },
}
