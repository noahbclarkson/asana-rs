//! The real `asana-client` handles driven end to end against the fake: tasks and sections,
//! custom fields, paging, `opt_fields`, faults, webhooks with a verifying receiver, the events
//! stream, jobs, the batch API and attachments.

#![expect(
    clippy::unwrap_used,
    reason = "test helpers fail the test loudly on an impossible setup"
)]

mod faults;
mod streams;
mod tasks;
mod webhooks;

use asana_client::{Auth, Client};
use asana_fake::{FakeAsana, ProjectSeed};
use asana_openapi::ids::{ProjectGid, SectionGid, TaskGid, UserGid, WorkspaceGid};

/// A client whose transport is `fake`.
pub(crate) fn client(fake: &FakeAsana) -> Client {
    Client::builder()
        .auth(Auth::personal_access_token("0/made-up"))
        .transport(fake.transport())
        .build()
        .unwrap()
}

/// A workspace with one person and a three-section project holding one task.
pub(crate) struct World {
    pub(crate) fake: FakeAsana,
    pub(crate) workspace: WorkspaceGid,
    pub(crate) ada: UserGid,
    pub(crate) project: ProjectGid,
    pub(crate) planned: SectionGid,
    pub(crate) sending: SectionGid,
    pub(crate) sent: SectionGid,
    pub(crate) task: TaskGid,
}

impl World {
    pub(crate) fn new() -> Self {
        let fake = FakeAsana::new();
        let acme = fake.workspace("Acme");
        let ada = acme.user("Ada Lovelace", "ada@example.com");
        let project: ProjectSeed<'_> = acme.project("Onboarding");
        let [planned, sending, sent] = project.sections(["Planned", "Send documents", "Sent"]);
        let task = project
            .task("Welcome pack")
            .in_section(&planned)
            .notes("Prepare the pack")
            .gid();
        let workspace = acme.gid();
        let project = project.gid();
        Self {
            fake,
            workspace,
            ada,
            project,
            planned,
            sending,
            sent,
            task,
        }
    }
}
