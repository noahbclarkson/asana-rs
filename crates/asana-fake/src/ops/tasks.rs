//! Tasks: read, list, create, update, delete, and subtasks.

use asana_openapi::endpoints::{
    CreateSubtaskForTask, CreateTask, DeleteTask, GetSubtasksForTask, GetTask, GetTasks,
    GetTasksForProject, GetTasksForSection, GetTasksForTag, UpdateTask,
};
use serde_json::Value;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::answer::{Answer, Call, Refusal, Reply, stamp};
use crate::changes::Change;
use crate::model::{Position, strip_tags};
use crate::paging::page;
use crate::router::{Route, serve};
use crate::select::bare_ref;
use crate::state::{Id, Kind, State, Task, lookup};

pub(crate) fn routes() -> Vec<Route> {
    vec![
        serve::<GetTask>(get),
        serve::<GetTasks>(list),
        serve::<GetTasksForProject>(for_project),
        serve::<GetTasksForSection>(for_section),
        serve::<GetTasksForTag>(for_tag),
        serve::<CreateTask>(create),
        serve::<UpdateTask>(update),
        serve::<DeleteTask>(delete),
        serve::<GetSubtasksForTask>(subtasks),
        serve::<CreateSubtaskForTask>(create_subtask),
    ]
}

fn get(state: &mut State, call: &Call) -> Reply {
    lookup(&state.tasks, "task", call.param(0))?;
    Ok(Answer::data(call.selection.one(state, call.param(0))))
}

/// The tasks of a project in the order Asana lists them: those in no section, then each
/// section's in turn.
pub(crate) fn project_tasks(state: &State, project: &str) -> Vec<Id> {
    let Some(record) = state.projects.get(project) else {
        return Vec::new();
    };
    let mut tasks = record.loose.clone();
    for section in &record.sections {
        if let Some(section) = state.sections.get(section) {
            tasks.extend(section.tasks.iter().cloned());
        }
    }
    tasks
}

/// Keeps the tasks `completed_since` and `modified_since` allow.
fn filter_times(state: &State, call: &Call, tasks: Vec<Id>) -> Result<Vec<Id>, Refusal> {
    let completed_since = match call.query("completed_since") {
        None => None,
        Some("now") => Some(state.now()),
        Some(text) => Some(
            OffsetDateTime::parse(text, &Rfc3339)
                .map_err(|_| Refusal::bad_request("completed_since: Not a valid date-time"))?,
        ),
    };
    let modified_since = match call.query("modified_since") {
        None => None,
        Some(text) => Some(
            OffsetDateTime::parse(text, &Rfc3339)
                .map_err(|_| Refusal::bad_request("modified_since: Not a valid date-time"))?,
        ),
    };
    Ok(tasks
        .into_iter()
        .filter(|gid| {
            let Some(task) = state.tasks.get(gid) else {
                return false;
            };
            let open = completed_since.is_none_or(|since| {
                !task.completed || task.completed_at.is_some_and(|at| at >= since)
            });
            let recent = modified_since.is_none_or(|since| task.modified_at >= since);
            open && recent
        })
        .collect())
}

fn list(state: &mut State, call: &Call) -> Reply {
    let project = call.query("project");
    let section = call.query("section");
    let tag = call.query("tag");
    let assignee = call.query("assignee");
    let workspace = call.query("workspace");
    let named = [project, section, tag, assignee.or(workspace)]
        .iter()
        .filter(|filter| filter.is_some())
        .count();
    if named != 1 || assignee.is_some() != workspace.is_some() {
        return Err(Refusal::bad_request(
            "Must specify exactly one of project, tag, section, user task list, or assignee + workspace",
        ));
    }
    let tasks = if let Some(project) = project {
        lookup(&state.projects, "project", project)?;
        project_tasks(state, project)
    } else if let Some(section) = section {
        lookup(&state.sections, "section", section)?.tasks.clone()
    } else if let Some(tag) = tag {
        tagged(state, tag)?
    } else {
        let workspace = workspace.unwrap_or_default();
        lookup(&state.workspaces, "workspace", workspace)?;
        let user = state.resolve_user("assignee", assignee.unwrap_or_default())?;
        state
            .tasks
            .iter()
            .filter(|(_, task)| {
                task.workspace == workspace && task.assignee.as_ref() == Some(&user)
            })
            .map(|(gid, _)| gid.clone())
            .collect()
    };
    let tasks = filter_times(state, call, tasks)?;
    page(state, call, &tasks)
}

fn tagged(state: &State, tag: &str) -> Result<Vec<Id>, Refusal> {
    lookup(&state.tags, "tag", tag)?;
    Ok(state
        .tasks
        .iter()
        .filter(|(_, task)| task.tags.iter().any(|other| other == tag))
        .map(|(gid, _)| gid.clone())
        .collect())
}

fn for_project(state: &mut State, call: &Call) -> Reply {
    lookup(&state.projects, "project", call.param(0))?;
    let tasks = filter_times(state, call, project_tasks(state, call.param(0)))?;
    page(state, call, &tasks)
}

fn for_section(state: &mut State, call: &Call) -> Reply {
    let tasks = lookup(&state.sections, "section", call.param(0))?
        .tasks
        .clone();
    let tasks = filter_times(state, call, tasks)?;
    page(state, call, &tasks)
}

fn for_tag(state: &mut State, call: &Call) -> Reply {
    let tasks = tagged(state, call.param(0))?;
    page(state, call, &tasks)
}

fn subtasks(state: &mut State, call: &Call) -> Reply {
    let tasks = lookup(&state.tasks, "task", call.param(0))?
        .subtasks
        .clone();
    page(state, call, &tasks)
}

fn create(state: &mut State, call: &Call) -> Reply {
    let parent = call.text("parent")?;
    create_task(state, call, parent)
}

fn create_subtask(state: &mut State, call: &Call) -> Reply {
    lookup(&state.tasks, "parent", call.param(0))?;
    create_task(state, call, Some(call.param(0).to_owned()))
}

/// Where a new task goes: its projects and, for each, a section if one was named.
fn placements(state: &State, call: &Call) -> Result<Vec<(Id, Option<Id>)>, Refusal> {
    let mut placements: Vec<(Id, Option<Id>)> = Vec::new();
    for project in call.gids("projects")?.unwrap_or_default() {
        lookup(&state.projects, "projects", &project)?;
        placements.push((project, None));
    }
    if let Some(memberships) = call.data().get("memberships") {
        let Some(memberships) = memberships.as_array() else {
            return Err(Refusal::bad_request("memberships: Not a list"));
        };
        for membership in memberships {
            let gid_of = |key: &str| {
                membership.get(key).and_then(|value| match value {
                    Value::String(gid) => Some(gid.clone()),
                    Value::Object(object) => object.get("gid")?.as_str().map(str::to_owned),
                    _ => None,
                })
            };
            let project = gid_of("project")
                .ok_or_else(|| Refusal::bad_request("memberships: Each needs a project"))?;
            lookup(&state.projects, "memberships", &project)?;
            let section = gid_of("section");
            if let Some(section) = &section {
                let record = lookup(&state.sections, "memberships", section)?;
                if record.project != project {
                    return Err(Refusal::bad_request(format!(
                        "memberships: Section {section} is not in project {project}"
                    )));
                }
            }
            placements.retain(|(other, _)| other != &project);
            placements.push((project, section));
        }
    }
    Ok(placements)
}

/// Creates a task from a request's `data`, as `createTask` and `createSubtaskForTask` do.
fn create_task(state: &mut State, call: &Call, parent: Option<Id>) -> Reply {
    let placements = placements(state, call)?;
    if let Some(parent) = &parent {
        lookup(&state.tasks, "parent", parent)?;
    }
    let workspace = match call.text("workspace")? {
        Some(workspace) => workspace,
        None => placements
            .first()
            .and_then(|(project, _)| state.projects.get(project))
            .map(|project| project.workspace.clone())
            .or_else(|| {
                parent
                    .as_ref()
                    .and_then(|parent| state.tasks.get(parent))
                    .map(|parent| parent.workspace.clone())
            })
            .ok_or_else(|| Refusal::bad_request("workspace: Missing input"))?,
    };
    lookup(&state.workspaces, "workspace", &workspace)?;
    for (project, _) in &placements {
        if state.projects.get(project).map(|p| &p.workspace) != Some(&workspace) {
            return Err(Refusal::bad_request(format!(
                "projects: Project {project} is not in workspace {workspace}"
            )));
        }
    }
    let edits = Edits::read(state, call)?;
    let followers = users(state, call, "followers")?;
    let tags = call.gids("tags")?.unwrap_or_default();
    for tag in &tags {
        lookup(&state.tags, "tags", tag)?;
    }
    let projects: Vec<Id> = placements
        .iter()
        .map(|(project, _)| project.clone())
        .collect();
    let fields = state.fields_for(None, &projects);
    let values = state.check_values(&fields, call.data().get("custom_fields"))?;

    let now = state.now();
    let mut task = state.blank_task(workspace, String::new());
    for follower in followers {
        if !task.followers.contains(&follower) {
            task.followers.push(follower);
        }
    }
    task.tags = tags;
    task.values = values
        .into_iter()
        .filter_map(|(field, value)| value.map(|value| (field, value)))
        .collect();
    let gid = state.new_gid(Kind::Task);
    edits.apply(&mut task, now);
    state.tasks.insert(gid.clone(), task);
    for (project, section) in &placements {
        state.place(&gid, project, section.as_ref(), &Position::End)?;
    }
    if let Some(parent) = &parent {
        state.set_parent(&gid, Some(parent), &Position::End)?;
    }
    Ok(Answer::created(call.selection.one(state, &gid)))
}

/// Users named in `data[name]`, as GIDs, email addresses or `me`.
pub(crate) fn users(state: &mut State, call: &Call, name: &str) -> Result<Vec<Id>, Refusal> {
    let mut users = Vec::new();
    for reference in call.gids(name)?.unwrap_or_default() {
        let user = state.resolve_user(name, &reference)?;
        if !users.contains(&user) {
            users.push(user);
        }
    }
    Ok(users)
}

fn update(state: &mut State, call: &Call) -> Reply {
    let gid = call.param(0).to_owned();
    let task = lookup(&state.tasks, "task", &gid)?.clone();
    for refused in [
        "projects",
        "memberships",
        "parent",
        "workspace",
        "tags",
        "followers",
    ] {
        if call.has(refused) {
            return Err(Refusal::bad_request(format!(
                "{refused}: This field cannot be changed with updateTask; use the dedicated endpoint"
            )));
        }
    }
    let edits = Edits::read(state, call)?;
    let fields = state.fields_on_task(&task);
    let values = state.check_values(&fields, call.data().get("custom_fields"))?;
    let now = state.now();
    let mut updated = task.clone();
    edits.apply(&mut updated, now);
    let changed = changed_fields(&task, &updated);
    if !changed.is_empty() {
        updated.modified_at = now;
    }
    state.tasks.insert(gid.clone(), updated);
    for field in changed {
        let new_value = match field {
            "assignee" => Some(
                state
                    .tasks
                    .get(&gid)
                    .and_then(|task| task.assignee.clone())
                    .map_or(Value::Null, |user| bare_ref(state, &user)),
            ),
            _ => None,
        };
        state.emit(Change::changed(&gid, field, new_value));
    }
    state.assign(&gid, values);
    Ok(Answer::data(call.selection.one(state, &gid)))
}

/// The fields whose values differ between two versions of a task, by Asana's names.
fn changed_fields(before: &Task, after: &Task) -> Vec<&'static str> {
    let mut changed = Vec::new();
    if before.name != after.name {
        changed.push("name");
    }
    if before.notes != after.notes || before.html_notes != after.html_notes {
        changed.push("notes");
    }
    if before.completed != after.completed {
        changed.push("completed");
    }
    if before.assignee != after.assignee {
        changed.push("assignee");
    }
    if before.due_on != after.due_on || before.due_at != after.due_at {
        changed.push("due_on");
    }
    if before.start_on != after.start_on || before.start_at != after.start_at {
        changed.push("start_on");
    }
    if before.resource_subtype != after.resource_subtype {
        changed.push("resource_subtype");
    }
    changed
}

fn delete(state: &mut State, call: &Call) -> Reply {
    state.delete_task(&call.param(0).to_owned())?;
    Ok(Answer::empty())
}

/// The plain fields of a create or update request, checked.
#[derive(Default)]
pub(crate) struct Edits {
    name: Option<String>,
    notes: Option<String>,
    html_notes: Option<String>,
    completed: Option<bool>,
    assignee: Option<Option<Id>>,
    due_on: Option<Option<String>>,
    due_at: Option<Option<String>>,
    start_on: Option<Option<String>>,
    start_at: Option<Option<String>>,
    resource_subtype: Option<String>,
}

impl Edits {
    pub(crate) fn read(state: &mut State, call: &Call) -> Result<Self, Refusal> {
        let assignee = if call.has("assignee") {
            match call.text("assignee")? {
                None => Some(None),
                Some(reference) => Some(Some(state.resolve_user("assignee", &reference)?)),
            }
        } else {
            None
        };
        let resource_subtype = call.text("resource_subtype")?;
        if let Some(subtype) = &resource_subtype
            && !matches!(
                subtype.as_str(),
                "default_task" | "milestone" | "section" | "approval"
            )
        {
            return Err(Refusal::bad_request(format!(
                "resource_subtype: Unknown task type: {subtype}"
            )));
        }
        Ok(Self {
            name: call.text("name")?,
            notes: call.text("notes")?,
            html_notes: call.text("html_notes")?,
            completed: call.flag("completed")?,
            assignee,
            due_on: date(call, "due_on")?,
            due_at: date_time(call, "due_at")?,
            start_on: date(call, "start_on")?,
            start_at: date_time(call, "start_at")?,
            resource_subtype,
        })
    }

    pub(crate) fn apply(self, task: &mut Task, now: OffsetDateTime) {
        if let Some(name) = self.name {
            task.name = name;
        }
        if let Some(notes) = self.notes {
            task.notes = notes;
            task.html_notes = None;
        }
        if let Some(html) = self.html_notes {
            task.notes = strip_tags(&html);
            task.html_notes = Some(html);
        }
        if let Some(completed) = self.completed {
            if completed && !task.completed {
                task.completed_at = Some(now);
            }
            if !completed {
                task.completed_at = None;
            }
            task.completed = completed;
        }
        if let Some(assignee) = self.assignee {
            task.assignee = assignee;
        }
        if let Some(due_on) = self.due_on {
            task.due_on = due_on;
            task.due_at = None;
        }
        if let Some(due_at) = self.due_at {
            task.due_on = due_at
                .as_ref()
                .map(|at| at.get(..10).unwrap_or(at).to_owned());
            task.due_at = due_at;
        }
        if let Some(start_on) = self.start_on {
            task.start_on = start_on;
            task.start_at = None;
        }
        if let Some(start_at) = self.start_at {
            task.start_on = start_at
                .as_ref()
                .map(|at| at.get(..10).unwrap_or(at).to_owned());
            task.start_at = start_at;
        }
        if let Some(subtype) = self.resource_subtype {
            task.resource_subtype = subtype;
        }
    }
}

/// A clearable `YYYY-MM-DD` member.
fn date(call: &Call, name: &str) -> Result<Option<Option<String>>, Refusal> {
    if !call.has(name) {
        return Ok(None);
    }
    let Some(text) = call.text(name)? else {
        return Ok(Some(None));
    };
    time::Date::parse(
        &text,
        time::macros::format_description!("[year]-[month]-[day]"),
    )
    .map_err(|_| Refusal::bad_request(format!("{name}: Must be a date as YYYY-MM-DD")))?;
    Ok(Some(Some(text)))
}

/// A clearable date-time member, normalised to Asana's own format.
fn date_time(call: &Call, name: &str) -> Result<Option<Option<String>>, Refusal> {
    if !call.has(name) {
        return Ok(None);
    }
    let Some(text) = call.text(name)? else {
        return Ok(Some(None));
    };
    let parsed = OffsetDateTime::parse(&text, &Rfc3339)
        .map_err(|_| Refusal::bad_request(format!("{name}: Must be an ISO 8601 date-time")))?;
    Ok(Some(Some(stamp(parsed))))
}
