//! The events stream, `GET /events?resource=…&sync=…`.
//!
//! The first call (no token, or a token the fake does not know) is answered `412` with a fresh
//! token and opens a stream on the resource; changes after that are returned, at most 100 at a
//! time, to a call that sends the token, each answer carrying the next token and `has_more`.
//! [`FakeAsana::expire_sync_tokens`](crate::FakeAsana::expire_sync_tokens) forgets every token,
//! as Asana does after a day.

use asana_openapi::endpoints::GetEvents;
use http::StatusCode;
use serde_json::{Value, json};

use crate::answer::{Answer, Call, Refusal, Reply};
use crate::router::{Route, serve};
use crate::state::{State, Stream};

pub(crate) fn routes() -> Vec<Route> {
    vec![serve::<GetEvents>(events)]
}

/// Most events one answer carries.
const PAGE: usize = 100;

fn events(state: &mut State, call: &Call) -> Reply {
    let resource = call
        .query("resource")
        .ok_or_else(|| Refusal::bad_request("resource: Missing input"))?
        .to_owned();
    if !state.exists(&resource) {
        return Err(Refusal::unknown("resource", &resource));
    }
    let known = call
        .query("sync")
        .and_then(|token| state.sync_tokens.get(token).copied())
        .filter(|(stream, _)| {
            state
                .streams
                .get(*stream)
                .is_some_and(|stream| stream.resource == resource)
        });
    let Some((stream, seen)) = known else {
        state.streams.push(Stream {
            resource,
            events: Vec::new(),
        });
        let token = state.new_token();
        state
            .sync_tokens
            .insert(token.clone(), (state.streams.len() - 1, 0));
        let mut refusal = Refusal::new(
            StatusCode::PRECONDITION_FAILED,
            "Sync token invalid or too old. If you are attempting to keep resources in sync, you must fetch the full dataset for this query now and use the new sync token for the next sync.",
        );
        refusal.extra.insert("sync".into(), Value::String(token));
        return Err(refusal);
    };
    let events: Vec<Value> = state
        .streams
        .get(stream)
        .map(|stream| {
            stream
                .events
                .iter()
                .skip(seen)
                .take(PAGE)
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    let total = state
        .streams
        .get(stream)
        .map_or(0, |stream| stream.events.len());
    let next = seen + events.len();
    let token = state.new_token();
    state.sync_tokens.insert(token.clone(), (stream, next));
    Ok(Answer::with_status(
        StatusCode::OK,
        json!({ "data": events, "sync": token, "has_more": next < total }),
    ))
}
