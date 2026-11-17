//! The task, section and project handles, against a scripted transport: what each method sends
//! (method, path, query and JSON body) and what it decodes.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers fail the test loudly on an impossible setup"
)]

#[path = "../pipeline/support.rs"]
#[expect(
    dead_code,
    reason = "the shared support module has helpers only the pipeline tests use"
)]
mod support;

mod projects;
mod sections;
mod tasks;

use std::sync::Arc;

use asana_client::Client;
use serde_json::Value;
use support::{Scripted, reply};

pub(crate) const WORKSPACE: &str = "1200000000000001";
pub(crate) const PROJECT: &str = "1200000000000100";
pub(crate) const OTHER_PROJECT: &str = "1200000000000101";
pub(crate) const SECTION: &str = "1200000000000200";
pub(crate) const OTHER_SECTION: &str = "1200000000000201";
pub(crate) const TASK: &str = support::TASK;
pub(crate) const OTHER_TASK: &str = "1201234567890124";
pub(crate) const TAG: &str = "1200000000000300";
pub(crate) const USER: &str = "1200000000000400";

/// What the transport was sent, decoded for comparing.
#[derive(Debug)]
pub(crate) struct Sent {
    pub(crate) method: String,
    /// The path after the base URL's `/api/1.0`.
    pub(crate) path: String,
    pub(crate) query: Vec<(String, String)>,
    pub(crate) body: Option<Value>,
}

impl Sent {
    /// The value of one query parameter.
    pub(crate) fn param(&self, name: &str) -> Option<&str> {
        self.query
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

/// The `index`th request the transport saw.
pub(crate) fn sent(transport: &Scripted, index: usize) -> Sent {
    let request = transport.request(index);
    let uri = request.uri();
    let path = uri
        .path()
        .strip_prefix("/api/1.0")
        .expect("the base URL's path")
        .to_owned();
    let query = serde_urlencoded::from_str(uri.query().unwrap_or_default()).unwrap();
    let body = if request.body().is_empty() {
        None
    } else {
        Some(serde_json::from_slice(request.body()).unwrap())
    };
    Sent {
        method: request.method().as_str().to_owned(),
        path,
        query,
        body,
    }
}

/// JSON parsed from text, so numbers compare as Asana would send them.
pub(crate) fn json(text: &str) -> Value {
    serde_json::from_str(text).unwrap()
}

/// A client whose transport answers every request with `status` and `body`.
pub(crate) fn answering(status: u16, body: &str) -> (Arc<Scripted>, Client) {
    let transport = Scripted::replies(vec![Ok(reply(status, body))]);
    let client = support::client(Arc::clone(&transport));
    (transport, client)
}

/// `{"data": {}}`, Asana's answer to an operation that returns nothing.
pub(crate) const EMPTY: &str = r#"{"data": {}}"#;

/// Parses a GID of any kind.
pub(crate) fn gid<T: std::str::FromStr>(text: &str) -> T
where
    T::Err: std::fmt::Debug,
{
    text.parse().unwrap()
}

/// A page of a list: `data`, and `next_page` when `next` is given.
pub(crate) fn page(items: &[Value], next: Option<&str>) -> String {
    let next_page = next.map(|offset| {
        serde_json::json!({
            "offset": offset,
            "path": format!("/tasks?limit=100&offset={offset}"),
            "uri": format!("https://app.asana.com/api/1.0/tasks?limit=100&offset={offset}"),
        })
    });
    serde_json::json!({ "data": items, "next_page": next_page }).to_string()
}

/// A client whose transport answers a list in pages: the first page without an offset, each
/// later page for the offset the one before it gave.
pub(crate) fn paged(pages: Vec<(Option<&'static str>, String)>) -> (Arc<Scripted>, Client) {
    let transport = Scripted::new(move |request, _| {
        let offset = request.uri().query().and_then(|query| {
            serde_urlencoded::from_str::<Vec<(String, String)>>(query)
                .unwrap()
                .into_iter()
                .find(|(key, _)| key == "offset")
                .map(|(_, value)| value)
        });
        let body = pages
            .iter()
            .find(|(at, _)| at.map(str::to_owned) == offset)
            .map(|(_, body)| body.clone())
            .expect("a page for this offset");
        Ok(reply(200, &body))
    });
    let client = support::client(Arc::clone(&transport));
    (transport, client)
}
