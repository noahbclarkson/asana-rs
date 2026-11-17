//! Projects: list, read, create, update, delete and task counts.

use asana_openapi::endpoints::{
    CreateProject, CreateProjectForWorkspace, DeleteProject, GetProject, GetProjects,
    GetProjectsForWorkspace, GetTaskCountsForProject, UpdateProject,
};
use serde_json::Value;

use crate::answer::{Answer, Call, Refusal, Reply};
use crate::changes::Change;
use crate::ops::tasks::project_tasks;
use crate::paging::page;
use crate::router::{Route, serve};
use crate::select::Node;
use crate::state::{Id, Kind, Project, State, lookup};

pub(crate) fn routes() -> Vec<Route> {
    vec![
        serve::<GetProjects>(list),
        serve::<GetProjectsForWorkspace>(for_workspace),
        serve::<GetProject>(get),
        serve::<CreateProject>(create),
        serve::<CreateProjectForWorkspace>(create_in_workspace),
        serve::<UpdateProject>(update),
        serve::<DeleteProject>(delete),
        serve::<GetTaskCountsForProject>(task_counts),
    ]
}

/// Adds a project named `name` to `workspace`.
pub(crate) fn add_project(state: &mut State, workspace: &Id, name: String) -> Result<Id, Refusal> {
    lookup(&state.workspaces, "workspace", workspace)?;
    let gid = state.new_gid(Kind::Project);
    let now = state.now();
    state.projects.insert(
        gid.clone(),
        Project {
            name,
            workspace: workspace.clone(),
            notes: String::new(),
            archived: false,
            created_at: now,
            modified_at: now,
            sections: Vec::new(),
            loose: Vec::new(),
            settings: Vec::new(),
        },
    );
    state.emit(Change::added(&gid, workspace));
    Ok(gid)
}

/// The projects of `workspace`, keeping archived ones only when `archived` allows.
fn projects_of(state: &State, workspace: &str, archived: Option<bool>) -> Vec<Id> {
    state
        .projects
        .iter()
        .filter(|(_, project)| project.workspace == workspace)
        .filter(|(_, project)| archived.is_none_or(|archived| project.archived == archived))
        .map(|(gid, _)| gid.clone())
        .collect()
}

fn archived(call: &Call) -> Result<Option<bool>, Refusal> {
    match call.query("archived") {
        None => Ok(None),
        Some("true") => Ok(Some(true)),
        Some("false") => Ok(Some(false)),
        Some(_) => Err(Refusal::bad_request("archived: Must be true or false")),
    }
}

fn list(state: &mut State, call: &Call) -> Reply {
    let archived = archived(call)?;
    let projects = match (call.query("workspace"), call.query("team")) {
        (Some(workspace), _) => {
            lookup(&state.workspaces, "workspace", workspace)?;
            projects_of(state, workspace, archived)
        }
        (None, Some(team)) => return Err(Refusal::not_found("team", team)),
        (None, None) => {
            return Err(Refusal::bad_request(
                "workspace: Must specify a workspace or a team",
            ));
        }
    };
    page(state, call, &projects)
}

fn for_workspace(state: &mut State, call: &Call) -> Reply {
    lookup(&state.workspaces, "workspace", call.param(0))?;
    let archived = archived(call)?;
    let projects = projects_of(state, call.param(0), archived);
    page(state, call, &projects)
}

fn get(state: &mut State, call: &Call) -> Reply {
    lookup(&state.projects, "project", call.param(0))?;
    Ok(Answer::data(call.selection.one(state, call.param(0))))
}

fn create(state: &mut State, call: &Call) -> Reply {
    let workspace = call.required("workspace")?;
    created(state, call, &workspace)
}

fn create_in_workspace(state: &mut State, call: &Call) -> Reply {
    let workspace = call.param(0).to_owned();
    created(state, call, &workspace)
}

fn created(state: &mut State, call: &Call, workspace: &Id) -> Reply {
    let name = call.text("name")?.unwrap_or_default();
    let gid = add_project(state, workspace, name)?;
    if let Some(record) = state.projects.get_mut(&gid) {
        record.notes = call.text("notes")?.unwrap_or_default();
    }
    Ok(Answer::created(call.selection.one(state, &gid)))
}

fn update(state: &mut State, call: &Call) -> Reply {
    let gid = call.param(0).to_owned();
    lookup(&state.projects, "project", &gid)?;
    let name = call.text("name")?;
    let notes = call.text("notes")?;
    let archived = call.flag("archived")?;
    let now = state.now();
    let mut changed = Vec::new();
    if let Some(project) = state.projects.get_mut(&gid) {
        if let Some(name) = name.filter(|name| name != &project.name) {
            project.name = name;
            changed.push("name");
        }
        if let Some(notes) = notes.filter(|notes| notes != &project.notes) {
            project.notes = notes;
            changed.push("notes");
        }
        if let Some(archived) = archived.filter(|archived| *archived != project.archived) {
            project.archived = archived;
            changed.push("archived");
        }
        if !changed.is_empty() {
            project.modified_at = now;
        }
    }
    for field in changed {
        state.emit(Change::changed(&gid, field, None));
    }
    Ok(Answer::data(call.selection.one(state, &gid)))
}

fn delete(state: &mut State, call: &Call) -> Reply {
    let gid = call.param(0).to_owned();
    lookup(&state.projects, "project", &gid)?;
    for task in project_tasks(state, &gid) {
        state.remove_from_project(&task, &gid)?;
    }
    state.emit(Change::deleted(&gid));
    if let Some(project) = state.projects.remove(&gid) {
        for section in project.sections {
            state.sections.remove(&section);
        }
        for setting in project.settings {
            state.settings.remove(&setting);
        }
    }
    Ok(Answer::empty())
}

fn task_counts(state: &mut State, call: &Call) -> Reply {
    lookup(&state.projects, "project", call.param(0))?;
    let tasks = project_tasks(state, call.param(0));
    let completed = tasks
        .iter()
        .filter(|gid| state.tasks.get(*gid).is_some_and(|task| task.completed))
        .count();
    let milestones: Vec<&Id> = tasks
        .iter()
        .filter(|gid| {
            state
                .tasks
                .get(*gid)
                .is_some_and(|task| task.resource_subtype == "milestone")
        })
        .collect();
    let completed_milestones = milestones
        .iter()
        .filter(|gid| state.tasks.get(**gid).is_some_and(|task| task.completed))
        .count();
    let count = |number: usize| Node::Value(Value::from(number));
    let fields = vec![
        ("num_tasks", count(tasks.len())),
        ("num_completed_tasks", count(completed)),
        ("num_incomplete_tasks", count(tasks.len() - completed)),
        ("num_milestones", count(milestones.len())),
        ("num_completed_milestones", count(completed_milestones)),
        (
            "num_incomplete_milestones",
            count(milestones.len() - completed_milestones),
        ),
    ];
    // Asana answers task counts only for the fields asked for; with none it answers `{}`.
    let counts = if call.selection.is_set() {
        call.selection.object(state, &fields)
    } else {
        Value::Object(serde_json::Map::new())
    };
    Ok(Answer::data(counts))
}
