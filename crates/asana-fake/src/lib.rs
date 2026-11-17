#![forbid(unsafe_code)]

//! An in-memory Asana for tests.
//!
//! [`FakeAsana`] is a stateful fake behind `asana-client`'s [`Transport`](asana_client::Transport):
//! workspaces, users, projects, sections, tasks, custom fields, tags, stories, attachments, task
//! templates, jobs and webhooks, answering with Asana's own envelope, paging and error shapes. It
//! performs the webhook handshake against a receiver the test registers and signs its deliveries,
//! so code that reacts to "a task moved to a section" can be tested end to end with no network
//! and no token.
//!
//! ```
//! use asana_client::openapi::endpoints::AddTaskForSection;
//! use asana_client::{Auth, Client};
//! use asana_fake::FakeAsana;
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let fake = FakeAsana::new();
//! let project = fake.workspace("Acme").project("Onboarding");
//! let [todo, done] = project.sections(["To do", "Done"]);
//! let task = project.task("Welcome Ada").in_section(&todo).gid();
//!
//! let client = Client::builder()
//!     .auth(Auth::personal_access_token("0/fake"))
//!     .transport(fake.transport())
//!     .build()?;
//!
//! // The code under test moves the task.
//! client.sections().add_task(done.clone(), task.clone()).send().await?;
//!
//! assert_eq!(fake.section_of(&task, &project.gid()), Some(done));
//! assert_eq!(fake.requests_for::<AddTaskForSection>().len(), 1);
//! # Ok(())
//! # }
//! ```
//!
//! # What it serves
//!
//! The operations the client's resource handles use: tasks (with subtasks, projects, followers,
//! tags, parents and dependencies), sections (`addTask` included), projects and task counts,
//! custom fields, enum options and project settings, tags, users (`me`, by email) and workspaces,
//! task search with the common filters, stories, attachments (multipart upload, links, and the
//! bytes behind `download_url`), task templates, duplication and jobs, the batch API, the events
//! stream, and webhooks. Any other operation in Asana's specification answers `501` naming its
//! `operationId`, and so does a search filter or sort the fake does not model, so a gap is loud
//! rather than a silent `404` or a filter ignored. Routes are matched by the
//! generated descriptors' own `METHOD` and `ROUTE`, so they follow the generated code.
//!
//! Answers follow Asana: `{"data": …}`, `next_page` with an opaque `offset` when a `limit` is
//! sent, `opt_fields` honoured (nested paths such as `memberships.section.name` included; a
//! single object is the full record without it, a list item is compact), and errors as
//! `{"errors": [{"message", "help"}]}` with `400`, `401`, `402`, `403`, `404`, `412` (the events
//! stream's first call) and `429` (on request, with [`Fault`]). Like Asana it refuses an
//! `opt_fields` path the specification does not list for the resource and an offset issued for
//! another list with `400`, and answers a path no operation has `404`.
//!
//! # Webhooks
//!
//! Register a [`receiver`](FakeAsana::receiver). Creating a webhook (through
//! `client.webhooks().establish(..)` or [`FakeAsana::create_webhook`]) sends it the handshake with
//! `X-Hook-Secret` and refuses the webhook unless it is echoed. From then on every change made
//! through the API or by an [`Actor`] queues events in Asana's shape on each webhook that can see
//! it; [`FakeAsana::deliver`] sends them as one signed batch per webhook (or
//! [`set_auto_deliver`](FakeAsana::set_auto_deliver) sends them as each request ends), and
//! [`FakeAsana::heartbeat`] sends an empty one. Seeding makes no events.
//!
//! # Determinism
//!
//! GIDs come from a counter (sixteen digits, starting at `1200000000000001`), tokens and secrets
//! from the same kind of counter, and times from the clock given to
//! [`FakeAsana::with_clock`] (the system clock by default).

mod actor;
mod answer;
mod changes;
mod fake;
mod faults;
mod model;
mod multipart;
mod ops;
mod paging;
mod recorded;
mod render;
mod router;
mod seed;
mod select;
mod state;
mod transport;
mod webhook_delivery;

pub use actor::Actor;
pub use fake::FakeAsana;
pub use faults::Fault;
pub use recorded::RecordedRequest;
pub use seed::{FieldSeed, ProjectSeed, TaskSeed, WorkspaceSeed};
pub use transport::FakeTransport;
pub use webhook_delivery::{Delivered, FakeError};

/// A typed GID from one the fake made.
#[expect(
    clippy::expect_used,
    reason = "every GID the fake makes is sixteen ASCII digits, which `parse` never refuses"
)]
pub(crate) fn typed<G: asana_openapi::ids::Gid>(gid: &str) -> G {
    G::parse(gid).expect("the fake makes only digit GIDs")
}
