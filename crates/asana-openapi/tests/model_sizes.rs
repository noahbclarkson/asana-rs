//! Keeps the models small enough to decode on an ordinary thread's stack. Nested models are boxed
//! by the generator; before that a `Story` held tasks, projects and users by value and was 41 KB,
//! which overflowed a 2 MiB stack while decoding. Run with `--nocapture` to see the sizes.

use std::mem::size_of;

use asana_openapi::models::{
    Attachment, CustomField, CustomFieldRequest, Event, Goal, Job, Membership, Portfolio, Project,
    ProjectRequest, ProjectTemplate, Section, StatusUpdate, Story, Task, TaskCreateRequest,
    TaskUpdateRequest, Team, User, Webhook, Workspace,
};

/// The most any one model may take, inline.
const CEILING: usize = 4 * 1024;

#[test]
fn every_model_fits_under_the_ceiling() {
    let sizes = [
        ("Attachment", size_of::<Attachment>()),
        ("CustomField", size_of::<CustomField>()),
        ("CustomFieldRequest", size_of::<CustomFieldRequest>()),
        ("Event", size_of::<Event>()),
        ("Goal", size_of::<Goal>()),
        ("Job", size_of::<Job>()),
        ("Membership", size_of::<Membership>()),
        ("Portfolio", size_of::<Portfolio>()),
        ("Project", size_of::<Project>()),
        ("ProjectRequest", size_of::<ProjectRequest>()),
        ("ProjectTemplate", size_of::<ProjectTemplate>()),
        ("Section", size_of::<Section>()),
        ("StatusUpdate", size_of::<StatusUpdate>()),
        ("Story", size_of::<Story>()),
        ("Task", size_of::<Task>()),
        ("TaskCreateRequest", size_of::<TaskCreateRequest>()),
        ("TaskUpdateRequest", size_of::<TaskUpdateRequest>()),
        ("Team", size_of::<Team>()),
        ("User", size_of::<User>()),
        ("Webhook", size_of::<Webhook>()),
        ("Workspace", size_of::<Workspace>()),
    ];
    for (name, size) in sizes {
        println!("{name}: {size} bytes");
    }
    let over: Vec<_> = sizes.iter().filter(|(_, size)| *size > CEILING).collect();
    assert!(over.is_empty(), "over {CEILING} bytes: {over:?}");
}
