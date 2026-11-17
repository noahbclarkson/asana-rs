//! The batch API: up to ten actions in one request, each answered as if sent alone.

use asana_openapi::endpoints::CreateBatchRequest;
use serde_json::{Value, json};

use crate::answer::{Answer, Call, Refusal, Reply};
use crate::router::{Action, Matched, Route, find, serve};
use crate::select::Selection;
use crate::state::State;

pub(crate) fn routes() -> Vec<Route> {
    vec![serve::<CreateBatchRequest>(run)]
}

/// The most actions one batch may hold.
const MAX_ACTIONS: usize = 10;

fn run(state: &mut State, call: &Call) -> Reply {
    let actions = call
        .data()
        .get("actions")
        .and_then(Value::as_array)
        .ok_or_else(|| Refusal::bad_request("actions: Missing input"))?;
    if actions.len() > MAX_ACTIONS {
        return Err(Refusal::bad_request(format!(
            "actions: A batch holds at most {MAX_ACTIONS} actions"
        )));
    }
    let answers = actions
        .iter()
        .map(|action| {
            let answer = match one(state, action) {
                Ok(answer) => answer,
                Err(refusal) => refusal.into_answer(),
            };
            json!({ "status_code": answer.status.as_u16(), "headers": {}, "body": answer.body })
        })
        .collect();
    Ok(Answer::data(Value::Array(answers)))
}

/// Runs one action through the same handler a request of its own would reach.
fn one(state: &mut State, action: &Value) -> Reply {
    let method = action
        .get("method")
        .and_then(Value::as_str)
        .ok_or_else(|| Refusal::bad_request("actions: Each action needs a method"))?;
    let relative = action
        .get("relative_path")
        .and_then(Value::as_str)
        .ok_or_else(|| Refusal::bad_request("actions: Each action needs a relative_path"))?;
    let (path, query_text) = relative.split_once('?').unwrap_or((relative, ""));
    let mut query: Vec<(String, String)> = url::form_urlencoded::parse(query_text.as_bytes())
        .into_owned()
        .collect();
    let options = action.get("options").and_then(Value::as_object);
    let mut listed_fields: Vec<String> = Vec::new();
    let mut selection = Selection::parse(
        query
            .iter()
            .find(|(key, _)| key == "opt_fields")
            .map(|(_, value)| value.as_str()),
    );
    if let Some(options) = options {
        for (key, value) in options {
            match (key.as_str(), value) {
                ("fields", Value::Array(fields)) => {
                    let fields: Vec<String> = fields
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect();
                    selection = Selection::of_list(&fields);
                    listed_fields = fields;
                }
                (key, Value::String(text)) => query.push((key.to_owned(), text.clone())),
                (key, Value::Number(number)) => query.push((key.to_owned(), number.to_string())),
                _ => {}
            }
        }
    }
    let body = action.get("data").map(|data| json!({ "data": data }));
    match find(method, path) {
        Matched::Served(route, params) => match route.action {
            Action::Sync(handler) => {
                route.check_fields(&query)?;
                route.check_paths(listed_fields.iter().map(String::as_str))?;
                let call = Call {
                    params,
                    query,
                    body,
                    form: Vec::new(),
                    selection,
                    path: path.to_owned(),
                };
                handler(state, &call)
            }
            Action::CreateWebhook => Err(Refusal::bad_request(format!(
                "{}: This action cannot be sent in a batch",
                route.operation
            ))),
        },
        Matched::Unserved(operation) => Err(crate::transport::unserved(operation)),
        Matched::Nothing => Err(crate::transport::no_route()),
    }
}
