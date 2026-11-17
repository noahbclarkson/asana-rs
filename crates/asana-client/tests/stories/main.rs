//! Stories, attachments, jobs, the batch API and the events stream, against a scripted transport
//! with bodies shaped like Asana's documented examples (the GIDs are made up).

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

mod attachments;
mod batch;
mod events;
mod jobs;
mod stories;

use serde_json::Value;

pub(crate) const STORY: &str = "1203000000000001";
pub(crate) const ATTACHMENT: &str = "1204000000000001";
pub(crate) const TEMPLATE: &str = "1205000000000001";
pub(crate) const JOB: &str = "1206000000000001";
pub(crate) const NEW_TASK: &str = "1201234567890999";
pub(crate) const USER: &str = "1207000000000001";

/// A request's JSON body.
pub(crate) fn json_body(request: &http::Request<bytes::Bytes>) -> Value {
    serde_json::from_slice(request.body()).unwrap()
}

/// A request's query as name-value pairs.
pub(crate) fn query(request: &http::Request<bytes::Bytes>) -> Vec<(String, String)> {
    request
        .uri()
        .query()
        .map(|query| serde_urlencoded::from_str(query).unwrap())
        .unwrap_or_default()
}

/// Parses a made-up GID into its type.
pub(crate) fn gid<G: asana_client::openapi::ids::Gid>(text: &str) -> G {
    G::parse(text).unwrap()
}
