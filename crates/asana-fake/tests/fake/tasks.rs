//! Tasks, sections, stories, custom fields, paging and `opt_fields`.

use asana_client::openapi::endpoints::{AddTaskForSection, GetGoal, GetTasks};
use asana_client::resources::attachments::Upload;
use asana_client::{CustomFieldValue, Error, NameMatch, TaskCustomFields, Transport, fields};
use asana_fake::FakeAsana;
use asana_openapi::ids::{EnumOptionGid, GoalGid, SectionGid, TaskGid};
use futures_util::TryStreamExt;
use http::StatusCode;

use crate::{World, client};

#[tokio::test]
async fn a_task_created_into_a_section_moves_and_leaves_a_story() {
    let world = World::new();
    let client = client(&world.fake);

    let task = client
        .tasks()
        .create()
        .workspace(world.workspace.clone())
        .section(world.project.clone(), world.sending.clone())
        .name("Engagement letter")
        .notes("Send it today")
        .send()
        .await
        .unwrap();
    let gid = task.gid.unwrap();
    assert_eq!(task.name.as_deref(), Some("Engagement letter"));
    assert_eq!(
        world.fake.section_of(&gid, &world.project),
        Some(world.sending.clone())
    );

    client
        .sections()
        .add_task(world.sent.clone(), gid.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(
        world.fake.section_of(&gid, &world.project),
        Some(world.sent.clone())
    );
    assert_eq!(world.fake.requests_for::<AddTaskForSection>().len(), 1);

    let stories = client
        .stories()
        .list(gid.clone())
        .fields(fields![Story: text, resource_subtype])
        .collect_max(10)
        .await
        .unwrap();
    assert_eq!(stories.len(), 1);
    assert_eq!(
        stories[0].text.as_deref(),
        Some("moved this task from \"Send documents\" to \"Sent\" in Onboarding")
    );
}

#[tokio::test]
async fn a_comment_is_stored_with_escaped_rich_text() {
    let world = World::new();
    let client = client(&world.fake);

    let story = client
        .stories()
        .comment(world.task.clone(), "Fees <agreed> & signed")
        .send()
        .await
        .unwrap();
    assert_eq!(story.text.as_deref(), Some("Fees <agreed> & signed"));
    assert_eq!(
        story.html_text.as_deref(),
        Some("<body>Fees &lt;agreed&gt; &amp; signed</body>")
    );
    assert_eq!(story.created_by.unwrap().gid, Some(world.ada));
}

#[tokio::test]
async fn a_typed_custom_field_is_written_and_read_back() {
    let fake = FakeAsana::new();
    let acme = fake.workspace("Acme");
    let stage = acme.enum_field("Stage", &["Planned", "Sent"]);
    let fee = acme.number_field("Fee", 2);
    let project = acme.project("Onboarding");
    project.with_field(&stage.gid()).with_field(&fee.gid());
    let task = project.task("Welcome pack").gid();
    let sent: EnumOptionGid = stage.option("Sent");
    let client = client(&fake);

    client
        .tasks()
        .update(task.clone())
        .custom_field(stage.gid(), CustomFieldValue::enum_option(sent.clone()))
        .custom_field(fee.gid(), CustomFieldValue::number(1250))
        .send()
        .await
        .unwrap();

    let read = client.tasks().get(task.clone()).send().await.unwrap();
    let stage_value = read.custom_field(&stage.gid()).unwrap();
    assert_eq!(
        stage_value
            .value
            .as_enum_option()
            .map(|option| option.gid.clone()),
        Some(sent)
    );
    assert_eq!(stage_value.display_value.as_deref(), Some("Sent"));
    let fee_value = read.custom_field(&fee.gid()).unwrap();
    assert_eq!(fee_value.value.as_number(), Some(1250.into()));
    assert_eq!(fee_value.display_value.as_deref(), Some("1250.00"));

    // A value of the wrong kind is refused, as Asana refuses it.
    let refused = client
        .tasks()
        .update(task)
        .custom_field(fee.gid(), CustomFieldValue::text("lots"))
        .send()
        .await
        .unwrap_err();
    assert_eq!(refused.status(), Some(StatusCode::BAD_REQUEST));
}

#[tokio::test]
async fn a_section_is_found_by_name() {
    let world = World::new();
    let client = client(&world.fake);

    let found = client
        .sections()
        .find_by_name(world.project.clone(), " send DOCUMENTS ")
        .ignore_case()
        .send()
        .await
        .unwrap();
    let NameMatch::One(section) = found else {
        panic!("expected one section, got {found:?}");
    };
    assert_eq!(section.gid, Some(world.sending));

    let missing = client
        .sections()
        .find_by_name(world.project.clone(), "Archived")
        .send()
        .await
        .unwrap();
    assert!(matches!(missing, NameMatch::Missing));
}

#[tokio::test]
async fn two_hundred_and_fifty_tasks_come_back_over_three_pages_in_order() {
    let fake = FakeAsana::new();
    let project = fake.workspace("Acme").project("Bulk");
    let section = project.section("Inbox");
    for index in 0..250 {
        let _ = project
            .task(&format!("Task {index:03}"))
            .in_section(&section);
    }
    let client = client(&fake);

    let tasks: Vec<_> = client
        .tasks()
        .list()
        .project(project.gid())
        .stream()
        .try_collect()
        .await
        .unwrap();

    assert_eq!(tasks.len(), 250);
    let names: Vec<String> = tasks.iter().filter_map(|task| task.name.clone()).collect();
    let expected: Vec<String> = (0..250).map(|index| format!("Task {index:03}")).collect();
    assert_eq!(names, expected);
    let pages = fake.requests_for::<GetTasks>();
    assert_eq!(pages.len(), 3);
    assert_eq!(pages[0].query("offset"), None);
    assert!(pages[1].query("offset").is_some());
    // A list item is compact unless fields are asked for.
    assert!(tasks[0].notes.is_none());
}

#[tokio::test]
async fn opt_fields_selects_exactly_the_paths_asked_for() {
    let world = World::new();
    let client = client(&world.fake);

    let task = client
        .tasks()
        .get(world.task.clone())
        .fields(fields![Task: name, memberships.section.name])
        .send()
        .await
        .unwrap();
    assert_eq!(task.gid, Some(world.task.clone()));
    assert_eq!(task.name.as_deref(), Some("Welcome pack"));
    assert!(task.notes.is_none());
    let memberships = task.memberships.unwrap();
    let section = memberships[0].section.as_deref().unwrap();
    assert_eq!(section.gid, Some(world.planned.clone()));
    assert_eq!(section.name.as_deref(), Some("Planned"));
    assert!(memberships[0].project.is_none());

    // Without fields a single object is the full record.
    let full = client.tasks().get(world.task.clone()).send().await.unwrap();
    assert_eq!(full.notes.as_deref(), Some("Prepare the pack"));
    assert_eq!(full.workspace.unwrap().gid, Some(world.workspace));
}

#[tokio::test]
async fn an_unknown_gid_is_404_and_an_unserved_operation_is_501() {
    let world = World::new();
    let client = client(&world.fake);

    let missing: TaskGid = "1299999999999999".parse().unwrap();
    let error = client.tasks().get(missing).send().await.unwrap_err();
    assert!(error.is_not_found());

    let goal: GoalGid = "1201234567890123".parse().unwrap();
    let error = client
        .send(GetGoal::builder(goal).build())
        .await
        .unwrap_err();
    assert_eq!(error.status(), Some(StatusCode::NOT_IMPLEMENTED));

    // The body names the operation, so a gap in the fake is plain to see.
    let request = http::Request::get("https://app.asana.com/api/1.0/goals/1201234567890123")
        .header("authorization", "Bearer 0/made-up")
        .body(bytes::Bytes::new())
        .unwrap();
    let response = world.fake.transport().send(request).await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(response.body()).unwrap();
    assert_eq!(
        body["errors"][0]["message"],
        "asana-fake does not serve getGoal"
    );
}

#[tokio::test]
async fn an_upload_is_downloaded_byte_for_byte() {
    let world = World::new();
    let client = client(&world.fake);

    let attachment = client
        .attachments()
        .upload(
            world.task.clone(),
            Upload::bytes("pack.pdf", &b"%PDF-1.7 made up"[..], "application/pdf"),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(attachment.name.as_deref(), Some("pack.pdf"));
    let bytes = client
        .attachments()
        .download(attachment.gid.unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(&bytes[..], b"%PDF-1.7 made up");
}

#[tokio::test]
async fn a_section_holding_tasks_cannot_be_deleted() {
    let world = World::new();
    let client = client(&world.fake);

    let error = client
        .sections()
        .delete(world.planned.clone())
        .send()
        .await
        .unwrap_err();
    assert!(matches!(error, Error::BadRequest(_)));
    let empty: SectionGid = world.sent.clone();
    client.sections().delete(empty).send().await.unwrap();
}

#[tokio::test]
async fn me_and_a_user_by_email_are_found() {
    let world = World::new();
    let client = client(&world.fake);

    let me = client.users().me().send().await.unwrap();
    assert_eq!(me.gid, Some(world.ada.clone()));
    assert_eq!(me.email.as_deref(), Some("ada@example.com"));

    let found = client
        .users()
        .by_email(&world.workspace, "ada@example.com")
        .await
        .unwrap();
    assert_eq!(found.and_then(|user| user.gid), Some(world.ada));
    let nobody = client
        .users()
        .by_email(&world.workspace, "nobody@example.com")
        .await
        .unwrap();
    assert!(nobody.is_none());
}
