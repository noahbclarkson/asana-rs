//! Sections: list, read, create, insert, rename, delete, and moving a task into one.

use asana_openapi::endpoints::{
    AddTaskForSection, CreateSectionForProject, DeleteSection, GetSection, GetSectionsForProject,
    InsertSectionForProject, UpdateSection,
};

use crate::answer::{Answer, Call, Refusal, Reply};
use crate::changes::Change;
use crate::model::{Position, insert};
use crate::paging::page;
use crate::router::{Route, serve};
use crate::state::{Id, Kind, Section, State, lookup};

pub(crate) fn routes() -> Vec<Route> {
    vec![
        serve::<GetSectionsForProject>(list),
        serve::<CreateSectionForProject>(create),
        serve::<InsertSectionForProject>(reorder),
        serve::<GetSection>(get),
        serve::<UpdateSection>(update),
        serve::<DeleteSection>(delete),
        serve::<AddTaskForSection>(add_task),
    ]
}

fn list(state: &mut State, call: &Call) -> Reply {
    let sections = lookup(&state.projects, "project", call.param(0))?
        .sections
        .clone();
    page(state, call, &sections)
}

fn get(state: &mut State, call: &Call) -> Reply {
    lookup(&state.sections, "section", call.param(0))?;
    Ok(Answer::data(call.selection.one(state, call.param(0))))
}

/// Adds a section named `name` to `project` at `position`.
pub(crate) fn add_section(
    state: &mut State,
    project: &Id,
    name: String,
    position: &Position,
) -> Result<Id, Refusal> {
    lookup(&state.projects, "project", project)?;
    let gid = state.new_gid(Kind::Section);
    if let Some(record) = state.projects.get_mut(project) {
        insert(&mut record.sections, gid.clone(), position, "insert_before")?;
    }
    let created_at = state.now();
    state.sections.insert(
        gid.clone(),
        Section {
            name,
            project: project.clone(),
            created_at,
            tasks: Vec::new(),
        },
    );
    state.emit(Change::added(&gid, project));
    Ok(gid)
}

fn create(state: &mut State, call: &Call) -> Reply {
    let project = call.param(0).to_owned();
    let name = call.required("name")?;
    let position = Position::read(call, Position::End)?;
    let gid = add_section(state, &project, name, &position)?;
    Ok(Answer::created(call.selection.one(state, &gid)))
}

fn reorder(state: &mut State, call: &Call) -> Reply {
    let project = call.param(0).to_owned();
    lookup(&state.projects, "project", &project)?;
    let section = call.required("section")?;
    let owner = lookup(&state.sections, "section", &section)?;
    if owner.project != project {
        return Err(Refusal::bad_request(format!(
            "section: {section} is not a section of project {project}"
        )));
    }
    let position = match (call.text("before_section")?, call.text("after_section")?) {
        (Some(before), None) => Position::Before(before),
        (None, Some(after)) => Position::After(after),
        _ => {
            return Err(Refusal::bad_request(
                "before_section, after_section: Exactly one must be given",
            ));
        }
    };
    if let Some(record) = state.projects.get_mut(&project) {
        let old = record.sections.clone();
        record.sections.retain(|other| other != &section);
        if let Err(refusal) = insert(&mut record.sections, section, &position, "before_section") {
            record.sections = old;
            return Err(refusal);
        }
    }
    Ok(Answer::empty())
}

fn update(state: &mut State, call: &Call) -> Reply {
    let gid = call.param(0).to_owned();
    lookup(&state.sections, "section", &gid)?;
    if let Some(name) = call.text("name")? {
        let changed = match state.sections.get_mut(&gid) {
            Some(section) if section.name != name => {
                section.name = name;
                true
            }
            _ => false,
        };
        if changed {
            state.emit(Change::changed(&gid, "name", None));
        }
    }
    Ok(Answer::data(call.selection.one(state, &gid)))
}

fn delete(state: &mut State, call: &Call) -> Reply {
    let gid = call.param(0).to_owned();
    let section = lookup(&state.sections, "section", &gid)?;
    if !section.tasks.is_empty() {
        return Err(Refusal::bad_request(
            "Sections must be empty to be deleted.",
        ));
    }
    let project = section.project.clone();
    if state
        .projects
        .get(&project)
        .is_some_and(|project| project.sections.len() == 1)
    {
        return Err(Refusal::bad_request(
            "The last remaining section of a project cannot be deleted.",
        ));
    }
    state.emit(Change::deleted(&gid));
    if let Some(project) = state.projects.get_mut(&project) {
        project.sections.retain(|other| other != &gid);
    }
    state.sections.remove(&gid);
    Ok(Answer::empty())
}

fn add_task(state: &mut State, call: &Call) -> Reply {
    let section = call.param(0).to_owned();
    let project = lookup(&state.sections, "section", &section)?
        .project
        .clone();
    let task = call.required("task")?;
    lookup(&state.tasks, "task", &task)?;
    let position = Position::read(call, Position::Start)?;
    if let Position::Before(anchor) | Position::After(anchor) = &position {
        let in_section = state
            .sections
            .get(&section)
            .is_some_and(|record| record.tasks.contains(anchor));
        if !in_section {
            return Err(Refusal::bad_request(format!(
                "insert_before, insert_after: Task {anchor} is not in section {section}"
            )));
        }
    }
    state.place(&task, &project, Some(&section), &position)?;
    Ok(Answer::empty())
}
