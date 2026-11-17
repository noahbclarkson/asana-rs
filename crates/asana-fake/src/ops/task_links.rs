//! What a task is linked to: projects, followers, tags, its parent and its dependencies.

use asana_openapi::endpoints::{
    AddDependenciesForTask, AddDependentsForTask, AddFollowersForTask, AddProjectForTask,
    AddTagForTask, GetDependenciesForTask, GetDependentsForTask, GetProjectsForTask,
    GetTagsForTask, RemoveDependenciesForTask, RemoveDependentsForTask, RemoveFollowerForTask,
    RemoveProjectForTask, RemoveTagForTask, SetParentForTask,
};

use crate::answer::{Answer, Call, Refusal, Reply};
use crate::changes::Change;
use crate::model::Position;
use crate::ops::tasks::users;
use crate::paging::page;
use crate::router::{Route, serve};
use crate::select::bare_ref;
use crate::state::{Id, State, lookup};

pub(crate) fn routes() -> Vec<Route> {
    vec![
        serve::<AddProjectForTask>(add_project),
        serve::<RemoveProjectForTask>(remove_project),
        serve::<GetProjectsForTask>(projects),
        serve::<AddFollowersForTask>(add_followers),
        serve::<RemoveFollowerForTask>(remove_followers),
        serve::<AddTagForTask>(add_tag),
        serve::<RemoveTagForTask>(remove_tag),
        serve::<GetTagsForTask>(tags),
        serve::<SetParentForTask>(set_parent),
        serve::<GetDependenciesForTask>(dependencies),
        serve::<GetDependentsForTask>(dependents),
        serve::<AddDependenciesForTask>(add_dependencies),
        serve::<RemoveDependenciesForTask>(remove_dependencies),
        serve::<AddDependentsForTask>(add_dependents),
        serve::<RemoveDependentsForTask>(remove_dependents),
    ]
}

fn task_param(state: &State, call: &Call) -> Result<Id, Refusal> {
    lookup(&state.tasks, "task", call.param(0))?;
    Ok(call.param(0).to_owned())
}

fn add_project(state: &mut State, call: &Call) -> Reply {
    let task = task_param(state, call)?;
    let project = call.required("project")?;
    lookup(&state.projects, "project", &project)?;
    let section = if call.has("section") {
        call.text("section")?
    } else {
        None
    };
    let position = Position::read(call, Position::End)?;
    let position = match &position {
        Position::Before(anchor) | Position::After(anchor) => {
            let in_project = state.tasks.get(anchor).is_some_and(|other| {
                other
                    .memberships
                    .iter()
                    .any(|membership| membership.project == project)
            });
            if !in_project {
                return Err(Refusal::bad_request(format!(
                    "insert_before, insert_after: Task {anchor} is not in project {project}"
                )));
            }
            position
        }
        _ => position,
    };
    state.place(&task, &project, section.as_ref(), &position)?;
    Ok(Answer::empty())
}

fn remove_project(state: &mut State, call: &Call) -> Reply {
    let task = task_param(state, call)?;
    let project = call.required("project")?;
    state.remove_from_project(&task, &project)?;
    Ok(Answer::empty())
}

fn projects(state: &mut State, call: &Call) -> Reply {
    let task = task_param(state, call)?;
    let projects: Vec<Id> = state
        .tasks
        .get(&task)
        .map(|task| {
            task.memberships
                .iter()
                .map(|membership| membership.project.clone())
                .collect()
        })
        .unwrap_or_default();
    page(state, call, &projects)
}

fn add_followers(state: &mut State, call: &Call) -> Reply {
    let task = task_param(state, call)?;
    let followers = users(state, call, "followers")?;
    for follower in followers {
        let added = match state.tasks.get_mut(&task) {
            Some(record) if !record.followers.contains(&follower) => {
                record.followers.push(follower.clone());
                true
            }
            _ => false,
        };
        if added {
            let value = bare_ref(state, &follower);
            state.emit(Change::listed(&task, "followers", true, value));
        }
    }
    Ok(Answer::data(call.selection.one(state, &task)))
}

fn remove_followers(state: &mut State, call: &Call) -> Reply {
    let task = task_param(state, call)?;
    let followers = users(state, call, "followers")?;
    for follower in followers {
        let removed = match state.tasks.get_mut(&task) {
            Some(record) if record.followers.contains(&follower) => {
                record.followers.retain(|other| other != &follower);
                true
            }
            _ => false,
        };
        if removed {
            let value = bare_ref(state, &follower);
            state.emit(Change::listed(&task, "followers", false, value));
        }
    }
    Ok(Answer::data(call.selection.one(state, &task)))
}

fn add_tag(state: &mut State, call: &Call) -> Reply {
    let task = task_param(state, call)?;
    let tag = call.required("tag")?;
    lookup(&state.tags, "tag", &tag)?;
    let added = match state.tasks.get_mut(&task) {
        Some(record) if !record.tags.contains(&tag) => {
            record.tags.push(tag.clone());
            true
        }
        _ => false,
    };
    if added {
        state.emit(Change::added(&task, &tag));
    }
    Ok(Answer::empty())
}

fn remove_tag(state: &mut State, call: &Call) -> Reply {
    let task = task_param(state, call)?;
    let tag = call.required("tag")?;
    lookup(&state.tags, "tag", &tag)?;
    let removed = match state.tasks.get_mut(&task) {
        Some(record) if record.tags.contains(&tag) => {
            record.tags.retain(|other| other != &tag);
            true
        }
        _ => false,
    };
    if removed {
        state.emit(Change::removed(&task, &tag));
    }
    Ok(Answer::empty())
}

fn tags(state: &mut State, call: &Call) -> Reply {
    let task = task_param(state, call)?;
    let tags = state
        .tasks
        .get(&task)
        .map(|task| task.tags.clone())
        .unwrap_or_default();
    page(state, call, &tags)
}

fn set_parent(state: &mut State, call: &Call) -> Reply {
    let task = task_param(state, call)?;
    if !call.has("parent") {
        return Err(Refusal::bad_request("parent: Missing input"));
    }
    let parent = call.text("parent")?;
    let position = Position::read(call, Position::End)?;
    state.set_parent(&task, parent.as_ref(), &position)?;
    Ok(Answer::data(call.selection.one(state, &task)))
}

fn dependencies(state: &mut State, call: &Call) -> Reply {
    let task = task_param(state, call)?;
    let list = state
        .tasks
        .get(&task)
        .map(|task| task.dependencies.clone())
        .unwrap_or_default();
    page(state, call, &list)
}

fn dependents(state: &mut State, call: &Call) -> Reply {
    let task = task_param(state, call)?;
    let list: Vec<Id> = state
        .tasks
        .iter()
        .filter(|(_, other)| other.dependencies.contains(&task))
        .map(|(gid, _)| gid.clone())
        .collect();
    page(state, call, &list)
}

/// Links `blocked` to depend on `blocking`, or unlinks them.
fn link(state: &mut State, blocked: &Id, blocking: &Id, add: bool) -> Result<(), Refusal> {
    lookup(&state.tasks, "task", blocking)?;
    if blocked == blocking {
        return Err(Refusal::bad_request(
            "dependencies: A task cannot depend on itself",
        ));
    }
    if let Some(record) = state.tasks.get_mut(blocked) {
        if add {
            if !record.dependencies.contains(blocking) {
                record.dependencies.push(blocking.clone());
            }
        } else {
            record.dependencies.retain(|other| other != blocking);
        }
    }
    Ok(())
}

fn add_dependencies(state: &mut State, call: &Call) -> Reply {
    relink(state, call, "dependencies", true, false)
}

fn remove_dependencies(state: &mut State, call: &Call) -> Reply {
    relink(state, call, "dependencies", false, false)
}

fn add_dependents(state: &mut State, call: &Call) -> Reply {
    relink(state, call, "dependents", true, true)
}

fn remove_dependents(state: &mut State, call: &Call) -> Reply {
    relink(state, call, "dependents", false, true)
}

fn relink(state: &mut State, call: &Call, name: &str, add: bool, reversed: bool) -> Reply {
    let task = task_param(state, call)?;
    let others = call
        .gids(name)?
        .ok_or_else(|| Refusal::bad_request(format!("{name}: Missing input")))?;
    for other in others {
        if reversed {
            lookup(&state.tasks, name, &other)?;
            link(state, &other, &task, add)?;
        } else {
            link(state, &task, &other, add)?;
        }
    }
    Ok(Answer::empty())
}
