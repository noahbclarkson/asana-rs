//! Attachments: upload (multipart), link, list, read, delete, and serving the bytes behind a
//! `download_url`.

use asana_openapi::endpoints::{
    CreateAttachmentForObject, DeleteAttachment, GetAttachment, GetAttachmentsForObject,
};
use bytes::Bytes;
use http::StatusCode;

use crate::answer::{Answer, Call, Refusal, Reply};
use crate::changes::Change;
use crate::paging::page;
use crate::router::{Route, serve};
use crate::state::{Attachment, Content, Id, Kind, State, lookup};

pub(crate) fn routes() -> Vec<Route> {
    vec![
        serve::<CreateAttachmentForObject>(create),
        serve::<GetAttachmentsForObject>(list),
        serve::<GetAttachment>(get),
        serve::<DeleteAttachment>(delete),
    ]
}

/// Stores an attachment on `parent` and emits its `added` event.
pub(crate) fn add_attachment(
    state: &mut State,
    parent: &Id,
    name: String,
    content: Content,
) -> Result<Id, Refusal> {
    match state.kind_of(parent) {
        Some(Kind::Task | Kind::Project) if state.exists(parent) => {}
        _ => return Err(Refusal::unknown("parent", parent)),
    }
    let gid = state.new_gid(Kind::Attachment);
    let created_at = state.now();
    state.attachments.insert(
        gid.clone(),
        Attachment {
            name,
            parent: parent.clone(),
            created_at,
            content,
        },
    );
    state.emit(Change::added(&gid, parent));
    Ok(gid)
}

fn create(state: &mut State, call: &Call) -> Reply {
    let text = |name: &str| {
        call.form
            .iter()
            .find(|part| part.name == name && part.file_name.is_none())
            .map(|part| String::from_utf8_lossy(&part.data).into_owned())
    };
    let parent = text("parent").ok_or_else(|| Refusal::bad_request("parent: Missing input"))?;
    let gid = if text("resource_subtype").as_deref() == Some("external") {
        let url = text("url").ok_or_else(|| Refusal::bad_request("url: Missing input"))?;
        if !url.starts_with("https://") && !url.starts_with("http://") {
            return Err(Refusal::bad_request("url: Not a valid URL"));
        }
        let name = text("name").unwrap_or_else(|| url.clone());
        add_attachment(state, &parent, name, Content::External { url })?
    } else {
        let file = call
            .form
            .iter()
            .find(|part| part.name == "file")
            .ok_or_else(|| Refusal::bad_request("file: Missing input"))?;
        let name = file.file_name.clone().unwrap_or_else(|| "file".to_owned());
        let content = Content::File {
            content_type: file
                .content_type
                .clone()
                .unwrap_or_else(|| "application/octet-stream".to_owned()),
            bytes: file.data.clone(),
        };
        add_attachment(state, &parent, name, content)?
    };
    Ok(Answer::data(call.selection.one(state, &gid)))
}

fn list(state: &mut State, call: &Call) -> Reply {
    let parent = call
        .query("parent")
        .ok_or_else(|| Refusal::bad_request("parent: Missing input"))?;
    if !state.exists(parent) {
        return Err(Refusal::unknown("parent", parent));
    }
    let attachments: Vec<Id> = state
        .attachments
        .iter()
        .filter(|(_, attachment)| attachment.parent == parent)
        .map(|(gid, _)| gid.clone())
        .collect();
    page(state, call, &attachments)
}

fn get(state: &mut State, call: &Call) -> Reply {
    lookup(&state.attachments, "attachment", call.param(0))?;
    Ok(Answer::data(call.selection.one(state, call.param(0))))
}

fn delete(state: &mut State, call: &Call) -> Reply {
    let gid = call.param(0).to_owned();
    lookup(&state.attachments, "attachment", &gid)?;
    state.emit(Change::deleted(&gid));
    state.attachments.remove(&gid);
    Ok(Answer::empty())
}

/// Answers a `GET` of an attachment's `download_url` with its bytes.
pub(crate) fn download(state: &State, path: &str) -> http::Response<Bytes> {
    let gid = path.trim_start_matches("/attachments/");
    let found = state
        .attachments
        .get(gid)
        .and_then(|attachment| match &attachment.content {
            Content::File {
                content_type,
                bytes,
            } => Some((content_type.clone(), bytes.clone())),
            Content::External { .. } => None,
        });
    let (status, content_type, body) = match found {
        Some((content_type, bytes)) => (StatusCode::OK, content_type, bytes),
        None => (
            StatusCode::NOT_FOUND,
            "text/plain".to_owned(),
            Bytes::from_static(b"Not Found"),
        ),
    };
    let mut response = http::Response::new(body.clone());
    *response.status_mut() = status;
    if let Ok(value) = http::HeaderValue::from_str(&content_type) {
        response
            .headers_mut()
            .insert(http::header::CONTENT_TYPE, value);
    }
    response.headers_mut().insert(
        http::header::CONTENT_LENGTH,
        http::HeaderValue::from(body.len()),
    );
    response
}
