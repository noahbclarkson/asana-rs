//! Stories: comments, and the records of changes the fake writes itself.

use asana_openapi::endpoints::{
    CreateStoryForTask, DeleteStory, GetStoriesForTask, GetStory, UpdateStory,
};

use crate::answer::{Answer, Call, Refusal, Reply};
use crate::changes::Change;
use crate::model::strip_tags;
use crate::paging::page;
use crate::render::escape;
use crate::router::{Route, serve};
use crate::state::{Id, State, lookup};

pub(crate) fn routes() -> Vec<Route> {
    vec![
        serve::<GetStoriesForTask>(list),
        serve::<CreateStoryForTask>(create),
        serve::<GetStory>(get),
        serve::<UpdateStory>(update),
        serve::<DeleteStory>(delete),
    ]
}

fn list(state: &mut State, call: &Call) -> Reply {
    let task = call.param(0);
    lookup(&state.tasks, "task", task)?;
    let stories: Vec<Id> = state
        .stories
        .iter()
        .filter(|(_, story)| story.task == task)
        .map(|(gid, _)| gid.clone())
        .collect();
    page(state, call, &stories)
}

fn create(state: &mut State, call: &Call) -> Reply {
    let task = call.param(0).to_owned();
    let pinned = call.flag("is_pinned")?.unwrap_or(false);
    let gid = state.comment(&task, call.text("text")?, call.text("html_text")?, pinned)?;
    Ok(Answer::created(call.selection.one(state, &gid)))
}

fn get(state: &mut State, call: &Call) -> Reply {
    lookup(&state.stories, "story", call.param(0))?;
    Ok(Answer::data(call.selection.one(state, call.param(0))))
}

fn update(state: &mut State, call: &Call) -> Reply {
    let gid = call.param(0).to_owned();
    let story = lookup(&state.stories, "story", &gid)?;
    let text = call.text("text")?;
    let html = call.text("html_text")?;
    if (text.is_some() || html.is_some()) && !story.is_comment() {
        return Err(Refusal::bad_request(
            "Only the text of a comment can be edited.",
        ));
    }
    let pinned = call.flag("is_pinned")?;
    let mut changed = false;
    if let Some(story) = state.stories.get_mut(&gid) {
        match (text, html) {
            (_, Some(html)) => {
                story.text = strip_tags(&html);
                story.html_text = Some(html);
                story.edited = true;
                changed = true;
            }
            (Some(text), None) => {
                story.html_text = Some(format!("<body>{}</body>", escape(&text)));
                story.text = text;
                story.edited = true;
                changed = true;
            }
            (None, None) => {}
        }
        if let Some(pinned) = pinned {
            story.pinned = pinned;
        }
    }
    if changed {
        state.emit(Change::changed(&gid, "text", None));
    }
    Ok(Answer::data(call.selection.one(state, &gid)))
}

fn delete(state: &mut State, call: &Call) -> Reply {
    let gid = call.param(0).to_owned();
    let story = lookup(&state.stories, "story", &gid)?;
    if !story.is_comment() {
        return Err(Refusal::forbidden("Only comments can be deleted."));
    }
    let author = story.created_by.clone();
    let task = story.task.clone();
    if author.as_ref() != Some(&state.acting_user()) {
        return Err(Refusal::forbidden(
            "Only the author of a comment can delete it.",
        ));
    }
    state.emit(Change::removed(&gid, &task));
    state.stories.remove(&gid);
    Ok(Answer::empty())
}
