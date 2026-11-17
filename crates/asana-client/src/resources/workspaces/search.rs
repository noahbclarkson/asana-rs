//! The workspace task search: its descriptor, which adds the custom field filters the generated
//! one cannot express, and the request's typed filters.

use std::borrow::Cow;

use asana_openapi::endpoint::{Endpoint, Method, PageLimit, QueryPair, RequestBody};
use asana_openapi::endpoints::{
    SearchTasksForWorkspace, SearchTasksForWorkspaceResourceSubtype, SearchTasksForWorkspaceSortBy,
};
use asana_openapi::fields::{Fields, TaskField};
use asana_openapi::ids::{CustomFieldGid, Gid, ProjectGid, SectionGid, TagGid, UserRef};
use asana_openapi::models::Task;
use asana_openapi::scopes::Scope;
use time::{Date, OffsetDateTime};

use crate::resources::request::Request;

/// `GET /workspaces/{workspace_gid}/tasks/search`: the generated descriptor plus query parameters
/// it has no field for, such as `custom_fields.{gid}.value`.
///
/// Its [`OPERATION_ID`](Endpoint::OPERATION_ID) is the generated one's, so the client's search
/// limiter applies to it.
#[derive(Clone, Debug)]
pub struct SearchTasks {
    search: SearchTasksForWorkspace,
    extra: Vec<QueryPair>,
}

impl SearchTasks {
    /// Wraps a generated search descriptor, with no extra parameters.
    pub fn new(search: SearchTasksForWorkspace) -> Self {
        Self {
            search,
            extra: Vec::new(),
        }
    }

    /// The generated descriptor, for a filter no setter covers.
    pub fn search_mut(&mut self) -> &mut SearchTasksForWorkspace {
        &mut self.search
    }

    /// The parameters added beyond the generated descriptor's, in the order they were added.
    pub fn extra(&self) -> &[QueryPair] {
        &self.extra
    }

    /// Adds a query parameter as written, for example
    /// `("custom_types.1201234567890001.custom_type_status_option.gid", "1201234567890002")`.
    pub fn push(&mut self, name: impl Into<Cow<'static, str>>, value: impl Into<String>) {
        self.extra.push((name.into(), value.into()));
    }
}

impl Endpoint for SearchTasks {
    type Output = Vec<Task>;
    const OPERATION_ID: &'static str = <SearchTasksForWorkspace as Endpoint>::OPERATION_ID;
    const METHOD: Method = <SearchTasksForWorkspace as Endpoint>::METHOD;
    const ROUTE: &'static str = <SearchTasksForWorkspace as Endpoint>::ROUTE;
    const SCOPE: Option<Scope> = <SearchTasksForWorkspace as Endpoint>::SCOPE;
    const IDEMPOTENT: bool = <SearchTasksForWorkspace as Endpoint>::IDEMPOTENT;

    fn path(&self) -> String {
        self.search.path()
    }

    fn query(&self) -> Vec<QueryPair> {
        let mut query = self.search.query();
        query.extend(self.extra.iter().cloned());
        query
    }

    fn body(&self) -> Result<RequestBody, serde_json::Error> {
        Ok(RequestBody::Empty)
    }
}

/// How a custom field filter compares, the `{op}` in `custom_fields.{gid}.{op}`.
///
/// Asana's table: `is_set` for any kind (`true`/`false`); `value` for text, number, enum (an
/// option's GID) and date (`YYYY-MM-DD`); `starts_with`, `ends_with` and `contains` for text;
/// `less_than` and `greater_than` for numbers; `before` and `after` for dates. Multi-enum fields
/// cannot be searched, nor several exact values of one field at once.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum CustomFieldFilter {
    /// `is_set`: whether the field has a value.
    IsSet,
    /// `value`: an exact value.
    Value,
    /// `starts_with`, text only.
    StartsWith,
    /// `ends_with`, text only.
    EndsWith,
    /// `contains`, text only.
    Contains,
    /// `less_than`, numbers only.
    LessThan,
    /// `greater_than`, numbers only.
    GreaterThan,
    /// `before`, dates only.
    Before,
    /// `after`, dates only.
    After,
}

impl CustomFieldFilter {
    /// The operator as Asana writes it.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::IsSet => "is_set",
            Self::Value => "value",
            Self::StartsWith => "starts_with",
            Self::EndsWith => "ends_with",
            Self::Contains => "contains",
            Self::LessThan => "less_than",
            Self::GreaterThan => "greater_than",
            Self::Before => "before",
            Self::After => "after",
        }
    }
}

fn joined<G: Gid>(gids: impl IntoIterator<Item = G>) -> String {
    gids.into_iter()
        .map(|gid| gid.as_str().to_owned())
        .collect::<Vec<_>>()
        .join(",")
}

fn users<U: Into<UserRef>>(users: impl IntoIterator<Item = U>) -> String {
    users
        .into_iter()
        .map(|user| user.into().to_string())
        .collect::<Vec<_>>()
        .join(",")
}

/// Defines a setter that sets one field of the generated search descriptor.
macro_rules! filter {
    ($(#[$doc:meta])* $name:ident($($arg:ident: $ty:ty),*) => |$search:ident| $set:expr) => {
        $(#[$doc])*
        pub fn $name(self, $($arg: $ty),*) -> Self {
            self.map(|endpoint| {
                let $search = endpoint.search_mut();
                $set;
            })
        }
    };
}

impl Request<'_, SearchTasks> {
    filter!(
        /// Full-text search on the task's name and description (`text`).
        text(text: impl Into<String>) => |search| search.text = Some(text.into())
    );
    filter!(
        /// Tasks in any of these projects, directly or through a parent task (`projects.any`).
        projects_any(projects: impl IntoIterator<Item = ProjectGid>) =>
            |search| search.projects_any = Some(joined(projects))
    );
    filter!(
        /// Tasks in all of these projects (`projects.all`).
        projects_all(projects: impl IntoIterator<Item = ProjectGid>) =>
            |search| search.projects_all = Some(joined(projects))
    );
    filter!(
        /// Tasks in none of these projects (`projects.not`).
        projects_not(projects: impl IntoIterator<Item = ProjectGid>) =>
            |search| search.projects_not = Some(joined(projects))
    );
    filter!(
        /// Tasks in any of these sections (`sections.any`). With
        /// [`projects_any`](Self::projects_any) as well, Asana returns the projects' tasks *and*
        /// the sections' tasks; leave it out to get only the sections'.
        sections_any(sections: impl IntoIterator<Item = SectionGid>) =>
            |search| search.sections_any = Some(joined(sections))
    );
    filter!(
        /// Tasks in none of these sections (`sections.not`).
        sections_not(sections: impl IntoIterator<Item = SectionGid>) =>
            |search| search.sections_not = Some(joined(sections))
    );
    filter!(
        /// Tasks with any of these tags (`tags.any`).
        tags_any(tags: impl IntoIterator<Item = TagGid>) =>
            |search| search.tags_any = Some(joined(tags))
    );
    filter!(
        /// Tasks assigned to any of these users (`assignee.any`): GIDs, email addresses or `me`.
        assignee_any(assignees: impl IntoIterator<Item = impl Into<UserRef>>) =>
            |search| search.assignee_any = Some(users(assignees))
    );
    filter!(
        /// Tasks assigned to none of these users (`assignee.not`).
        assignee_not(assignees: impl IntoIterator<Item = impl Into<UserRef>>) =>
            |search| search.assignee_not = Some(users(assignees))
    );
    filter!(
        /// Only completed (`true`) or only incomplete (`false`) tasks (`completed`).
        completed(completed: bool) => |search| search.completed = Some(completed)
    );
    filter!(
        /// Only subtasks (`true`) or only top-level tasks (`false`) (`is_subtask`).
        is_subtask(is_subtask: bool) => |search| search.is_subtask = Some(is_subtask)
    );
    filter!(
        /// Only tasks of this subtype, such as milestones (`resource_subtype`).
        resource_subtype(subtype: SearchTasksForWorkspaceResourceSubtype) =>
            |search| search.resource_subtype = Some(subtype)
    );
    filter!(
        /// Tasks modified after this moment (`modified_at.after`).
        modified_at_after(at: OffsetDateTime) => |search| search.modified_at_after = Some(at)
    );
    filter!(
        /// Tasks modified before this moment (`modified_at.before`).
        modified_at_before(at: OffsetDateTime) => |search| search.modified_at_before = Some(at)
    );
    filter!(
        /// Tasks modified after this day (`modified_on.after`).
        modified_on_after(day: Date) => |search| search.modified_on_after = Some(day)
    );
    filter!(
        /// Tasks modified before this day (`modified_on.before`).
        modified_on_before(day: Date) => |search| search.modified_on_before = Some(day)
    );
    filter!(
        /// Tasks created after this moment (`created_at.after`): the bound for paging by time.
        created_at_after(at: OffsetDateTime) => |search| search.created_at_after = Some(at)
    );
    filter!(
        /// Tasks created before this moment (`created_at.before`).
        created_at_before(at: OffsetDateTime) => |search| search.created_at_before = Some(at)
    );
    filter!(
        /// Tasks created after this day (`created_on.after`).
        created_on_after(day: Date) => |search| search.created_on_after = Some(day)
    );
    filter!(
        /// Tasks created before this day (`created_on.before`).
        created_on_before(day: Date) => |search| search.created_on_before = Some(day)
    );
    filter!(
        /// Tasks due after this day (`due_on.after`).
        due_on_after(day: Date) => |search| search.due_on_after = Some(day)
    );
    filter!(
        /// Tasks due before this day (`due_on.before`).
        due_on_before(day: Date) => |search| search.due_on_before = Some(day)
    );
    filter!(
        /// Tasks due on this day, or with no due date when `None` (`due_on`).
        due_on(day: Option<Date>) => |search| search.due_on = Some(day)
    );
    filter!(
        /// Tasks due after this moment (`due_at.after`).
        due_at_after(at: OffsetDateTime) => |search| search.due_at_after = Some(at)
    );
    filter!(
        /// Tasks due before this moment (`due_at.before`).
        due_at_before(at: OffsetDateTime) => |search| search.due_at_before = Some(at)
    );
    filter!(
        /// The order of the results (`sort_by`); Asana's default is `modified_at`.
        sort_by(sort: SearchTasksForWorkspaceSortBy) => |search| search.sort_by = Some(sort)
    );
    filter!(
        /// Ascending (`true`) or descending (`false`, Asana's default) (`sort_ascending`).
        sort_ascending(ascending: bool) => |search| search.sort_ascending = Some(ascending)
    );
    filter!(
        /// How many tasks one call returns, 1 to 100 (`limit`); larger values are capped at 100.
        limit(limit: u8) => |search| search.limit = Some(PageLimit::new(limit))
    );
    filter!(
        /// Chooses the fields Asana returns (`opt_fields`), for example
        /// `fields![Task: name, created_at]`.
        fields(fields: Fields<TaskField>) => |search| search.opt_fields = fields
    );

    /// A custom field filter, `custom_fields.{field}.{filter}={value}`, with the value written
    /// as Asana's table asks: `true`/`false`, text, a number, an enum option's GID or a
    /// `YYYY-MM-DD` day.
    pub fn custom_field(
        self,
        field: &CustomFieldGid,
        filter: CustomFieldFilter,
        value: impl Into<String>,
    ) -> Self {
        let name = format!("custom_fields.{}.{}", field.as_str(), filter.as_str());
        self.param(name, value)
    }

    /// A query parameter as written, for a filter with no setter.
    pub fn param(self, name: impl Into<Cow<'static, str>>, value: impl Into<String>) -> Self {
        self.map(|endpoint| endpoint.push(name, value))
    }
}
