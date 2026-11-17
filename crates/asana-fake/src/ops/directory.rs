//! Workspaces, users and tags: the objects a workspace holds beside its projects.

use asana_openapi::endpoints::{
    CreateTag, CreateTagForWorkspace, DeleteTag, GetTag, GetTags, GetTagsForWorkspace, GetUser,
    GetUserForWorkspace, GetUsers, GetUsersForWorkspace, GetWorkspace, GetWorkspaces, UpdateTag,
};

use crate::answer::{Answer, Call, Refusal, Reply};
use crate::changes::Change;
use crate::paging::page;
use crate::router::{Route, serve};
use crate::state::{Id, Kind, State, Tag, lookup};

pub(crate) fn routes() -> Vec<Route> {
    vec![
        serve::<GetWorkspaces>(workspaces),
        serve::<GetWorkspace>(workspace),
        serve::<GetUsers>(users),
        serve::<GetUser>(user),
        serve::<GetUsersForWorkspace>(users_of_workspace),
        serve::<GetUserForWorkspace>(user_of_workspace),
        serve::<GetTags>(tags),
        serve::<GetTagsForWorkspace>(tags_of_workspace),
        serve::<CreateTag>(create_tag),
        serve::<CreateTagForWorkspace>(create_tag_in_workspace),
        serve::<GetTag>(tag),
        serve::<UpdateTag>(update_tag),
        serve::<DeleteTag>(delete_tag),
    ]
}

fn workspaces(state: &mut State, call: &Call) -> Reply {
    let me = state.acting_user();
    let workspaces = state
        .users
        .get(&me)
        .map(|user| user.workspaces.clone())
        .unwrap_or_default();
    page(state, call, &workspaces)
}

fn workspace(state: &mut State, call: &Call) -> Reply {
    lookup(&state.workspaces, "workspace", call.param(0))?;
    Ok(Answer::data(call.selection.one(state, call.param(0))))
}

fn users(state: &mut State, call: &Call) -> Reply {
    let users: Vec<Id> = match call.query("workspace") {
        Some(workspace) => lookup(&state.workspaces, "workspace", workspace)?
            .users
            .clone(),
        None if call.query("team").is_some() => {
            return Err(Refusal::not_found(
                "team",
                call.query("team").unwrap_or_default(),
            ));
        }
        None => state.users.keys().cloned().collect(),
    };
    page(state, call, &users)
}

fn user(state: &mut State, call: &Call) -> Reply {
    let gid = state.resolve_user("user", call.param(0))?;
    Ok(Answer::data(call.selection.one(state, &gid)))
}

fn users_of_workspace(state: &mut State, call: &Call) -> Reply {
    let users = lookup(&state.workspaces, "workspace", call.param(0))?
        .users
        .clone();
    page(state, call, &users)
}

fn user_of_workspace(state: &mut State, call: &Call) -> Reply {
    let workspace = call.param(0).to_owned();
    lookup(&state.workspaces, "workspace", &workspace)?;
    let gid = state.resolve_user("user", call.param(1))?;
    let member = state
        .users
        .get(&gid)
        .is_some_and(|user| user.workspaces.contains(&workspace));
    if !member {
        return Err(Refusal::not_found("user", call.param(1)));
    }
    Ok(Answer::data(call.selection.one(state, &gid)))
}

fn tags_in(state: &State, workspace: &str) -> Vec<Id> {
    state
        .tags
        .iter()
        .filter(|(_, tag)| tag.workspace == workspace)
        .map(|(gid, _)| gid.clone())
        .collect()
}

fn tags(state: &mut State, call: &Call) -> Reply {
    let workspace = call
        .query("workspace")
        .ok_or_else(|| Refusal::bad_request("workspace: Missing input"))?;
    lookup(&state.workspaces, "workspace", workspace)?;
    let tags = tags_in(state, workspace);
    page(state, call, &tags)
}

fn tags_of_workspace(state: &mut State, call: &Call) -> Reply {
    lookup(&state.workspaces, "workspace", call.param(0))?;
    let tags = tags_in(state, call.param(0));
    page(state, call, &tags)
}

/// Adds a tag to `workspace`.
pub(crate) fn add_tag(
    state: &mut State,
    workspace: &Id,
    name: String,
    color: Option<String>,
    notes: String,
) -> Result<Id, Refusal> {
    lookup(&state.workspaces, "workspace", workspace)?;
    let gid = state.new_gid(Kind::Tag);
    let created_at = state.now();
    state.tags.insert(
        gid.clone(),
        Tag {
            name,
            workspace: workspace.clone(),
            color,
            notes,
            created_at,
        },
    );
    state.emit(Change::added(&gid, workspace));
    Ok(gid)
}

fn create_tag(state: &mut State, call: &Call) -> Reply {
    let workspace = call.required("workspace")?;
    created(state, call, &workspace)
}

fn create_tag_in_workspace(state: &mut State, call: &Call) -> Reply {
    let workspace = call.param(0).to_owned();
    created(state, call, &workspace)
}

fn created(state: &mut State, call: &Call, workspace: &Id) -> Reply {
    let name = call.required("name")?;
    let gid = add_tag(
        state,
        workspace,
        name,
        call.text("color")?,
        call.text("notes")?.unwrap_or_default(),
    )?;
    Ok(Answer::created(call.selection.one(state, &gid)))
}

fn tag(state: &mut State, call: &Call) -> Reply {
    lookup(&state.tags, "tag", call.param(0))?;
    Ok(Answer::data(call.selection.one(state, call.param(0))))
}

fn update_tag(state: &mut State, call: &Call) -> Reply {
    let gid = call.param(0).to_owned();
    lookup(&state.tags, "tag", &gid)?;
    let name = call.text("name")?;
    let color = call.text("color")?;
    let notes = call.text("notes")?;
    if let Some(tag) = state.tags.get_mut(&gid) {
        if let Some(name) = name {
            tag.name = name;
        }
        if color.is_some() {
            tag.color = color;
        }
        if let Some(notes) = notes {
            tag.notes = notes;
        }
    }
    Ok(Answer::data(call.selection.one(state, &gid)))
}

fn delete_tag(state: &mut State, call: &Call) -> Reply {
    let gid = call.param(0).to_owned();
    lookup(&state.tags, "tag", &gid)?;
    state.emit(Change::deleted(&gid));
    for task in state.tasks.values_mut() {
        task.tags.retain(|tag| tag != &gid);
    }
    state.tags.remove(&gid);
    Ok(Answer::empty())
}
