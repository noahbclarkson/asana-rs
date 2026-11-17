//! Resource handles: `client.tasks()`, `client.sections()` and the rest.
//!
//! Every operation is already reachable as `client.send(asana_openapi::endpoints::…)`. A handle
//! adds nothing the API lacks; it makes the common calls read well, with typed arguments and
//! sensible defaults, and each of its requests ends in `.send().await`. Each module adds its own
//! accessor to [`Client`](crate::Client) in an `impl` block of its own.

pub mod attachments;
pub mod batch;
pub mod custom_fields;
pub mod events;
pub mod jobs;
pub mod projects;
pub mod sections;
pub mod stories;
pub mod tags;
pub mod tasks;
pub mod templates;
pub mod users;
pub mod workspaces;
