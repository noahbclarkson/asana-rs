//! Custom field values, the custom fields, tags, users and workspaces handles, and the task
//! search, tested against a scripted transport with bodies shaped like Asana's documented
//! examples (the GIDs are invented).

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers fail the test loudly on an impossible setup"
)]

#[path = "../pipeline/support.rs"]
#[expect(
    dead_code,
    reason = "the pipeline's support module has helpers these tests do not use"
)]
mod support;

mod helpers;

mod custom_fields;
mod read;
mod search;
mod tags;
mod users;
mod write;
