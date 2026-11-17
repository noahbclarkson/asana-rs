//! Task templates, duplicating a task, and the jobs both start.
//!
//! Instantiating a template or duplicating a task makes the new task at once and answers with a
//! job naming it, as Asana does. The job answers `in_progress` to its first
//! [`FakeAsana::set_job_polls`](crate::FakeAsana::set_job_polls) reads (none by default) and
//! `succeeded` after that.

use asana_openapi::endpoints::{
    DeleteTaskTemplate, DuplicateTask, GetJob, GetTaskTemplate, GetTaskTemplates, InstantiateTask,
};
use serde_json::Value;

use crate::answer::{Answer, Call, Refusal, Reply};
use crate::model::Position;
use crate::paging::page;
use crate::router::{Route, serve};
use crate::state::{Id, Job, JobStatus, Kind, State, TaskTemplate, lookup};

pub(crate) fn routes() -> Vec<Route> {
    vec![
        serve::<GetTaskTemplates>(list),
        serve::<GetTaskTemplate>(get),
        serve::<DeleteTaskTemplate>(delete),
        serve::<InstantiateTask>(instantiate),
        serve::<DuplicateTask>(duplicate),
        serve::<GetJob>(job),
    ]
}

/// Adds a task template to `project`; the tasks it makes are named `task_name`.
pub(crate) fn add_template(
    state: &mut State,
    project: &Id,
    name: String,
    task_name: String,
    description: String,
) -> Result<Id, Refusal> {
    lookup(&state.projects, "project", project)?;
    let gid = state.new_gid(Kind::TaskTemplate);
    let created_at = state.now();
    let created_by = state.me.clone();
    state.templates.insert(
        gid.clone(),
        TaskTemplate {
            name,
            project: project.clone(),
            task_name,
            description,
            created_at,
            created_by,
        },
    );
    Ok(gid)
}

fn list(state: &mut State, call: &Call) -> Reply {
    let project = call
        .query("project")
        .ok_or_else(|| Refusal::bad_request("project: Missing input"))?;
    lookup(&state.projects, "project", project)?;
    let templates: Vec<Id> = state
        .templates
        .iter()
        .filter(|(_, template)| template.project == project)
        .map(|(gid, _)| gid.clone())
        .collect();
    page(state, call, &templates)
}

fn get(state: &mut State, call: &Call) -> Reply {
    lookup(&state.templates, "task_template", call.param(0))?;
    Ok(Answer::data(call.selection.one(state, call.param(0))))
}

fn delete(state: &mut State, call: &Call) -> Reply {
    lookup(&state.templates, "task_template", call.param(0))?;
    state.templates.remove(call.param(0));
    Ok(Answer::empty())
}

/// Starts a job that has made `task`.
fn start_job(state: &mut State, subtype: &'static str, task: Id) -> Id {
    let gid = state.new_gid(Kind::Job);
    let polls_left = state.job_polls;
    state.jobs.insert(
        gid.clone(),
        Job {
            subtype,
            status: JobStatus::InProgress,
            polls_left,
            new_task: Some(task),
        },
    );
    gid
}

fn instantiate(state: &mut State, call: &Call) -> Reply {
    let template = lookup(&state.templates, "task_template", call.param(0))?.clone();
    let name = call.text("name")?.unwrap_or(template.task_name);
    let workspace = lookup(&state.projects, "project", &template.project)?
        .workspace
        .clone();
    let mut task = state.blank_task(workspace, name);
    task.notes = template.description;
    let gid = state.new_gid(Kind::Task);
    state.tasks.insert(gid.clone(), task);
    state.place(&gid, &template.project, None, &Position::End)?;
    let job = start_job(state, "instantiate_task", gid);
    Ok(Answer::created(call.selection.one(state, &job)))
}

fn duplicate(state: &mut State, call: &Call) -> Reply {
    let source = call.param(0).to_owned();
    let original = lookup(&state.tasks, "task", &source)?.clone();
    let name = call.required("name")?;
    let include: Vec<String> = match call.data().get("include") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::String(text)) => text.split(',').map(|item| item.trim().to_owned()).collect(),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        Some(_) => return Err(Refusal::bad_request("include: Not a list")),
    };
    let wants = |what: &str| include.iter().any(|item| item == what);
    let mut task = state.blank_task(original.workspace.clone(), name);
    if wants("notes") {
        task.notes.clone_from(&original.notes);
        task.html_notes.clone_from(&original.html_notes);
    }
    if wants("assignee") {
        task.assignee.clone_from(&original.assignee);
    }
    if wants("dates") {
        task.due_on.clone_from(&original.due_on);
        task.due_at.clone_from(&original.due_at);
        task.start_on.clone_from(&original.start_on);
        task.start_at.clone_from(&original.start_at);
    }
    if wants("followers") {
        for follower in &original.followers {
            if !task.followers.contains(follower) {
                task.followers.push(follower.clone());
            }
        }
    }
    if wants("tags") {
        task.tags.clone_from(&original.tags);
    }
    if wants("projects") {
        task.values.clone_from(&original.values);
    }
    let gid = state.new_gid(Kind::Task);
    state.tasks.insert(gid.clone(), task);
    if wants("projects") {
        for membership in &original.memberships {
            state.place(
                &gid,
                &membership.project,
                membership.section.as_ref(),
                &Position::After(source.clone()),
            )?;
        }
    }
    if wants("parent")
        && let Some(parent) = &original.parent
    {
        state.set_parent(&gid, Some(parent), &Position::After(source.clone()))?;
    }
    if wants("subtasks") {
        for subtask in &original.subtasks {
            let Some(name) = state.tasks.get(subtask).map(|task| task.name.clone()) else {
                continue;
            };
            let copy = state.blank_task(original.workspace.clone(), name);
            let copy_gid = state.new_gid(Kind::Task);
            state.tasks.insert(copy_gid.clone(), copy);
            state.set_parent(&copy_gid, Some(&gid), &Position::End)?;
        }
    }
    let job = start_job(state, "duplicate_task", gid);
    Ok(Answer::created(call.selection.one(state, &job)))
}

fn job(state: &mut State, call: &Call) -> Reply {
    let gid = call.param(0).to_owned();
    let job = crate::state::lookup_mut(&mut state.jobs, "job", &gid)?;
    if job.status == JobStatus::InProgress {
        if job.polls_left == 0 {
            job.status = JobStatus::Succeeded;
        } else {
            job.polls_left -= 1;
        }
    }
    Ok(Answer::data(call.selection.one(state, &gid)))
}
