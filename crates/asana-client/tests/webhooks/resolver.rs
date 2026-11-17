//! The resolver: what it drops, how it groups, and re-reading a task before acting.

use std::time::Duration;

use asana_client::openapi::fields;
use asana_client::openapi::ids::{Gid, SectionGid, TaskGid, UserGid};
use asana_client::webhooks::{Coalesced, Resolver, SeenTasks, confirm_in_section, reread};
use asana_webhooks::{EventBatch, ManualClock, MemoryDedupe, ResourceGid};

use crate::support::{Scripted, client, reply};
use crate::{
    ALICE, BOT, NOW, PROJECT, SECTION_A, SECTION_B, SECTION_C, TASK_A, TASK_B, changed, event,
    query,
};

fn task(gid: &str) -> TaskGid {
    gid.parse().unwrap()
}

fn section(gid: &str) -> SectionGid {
    gid.parse().unwrap()
}

fn batch(events: Vec<asana_webhooks::Event>) -> EventBatch {
    EventBatch {
        events,
        ..EventBatch::default()
    }
}

fn entered(task: &str, section: &str, seconds: i64) -> asana_webhooks::Event {
    event(
        "added",
        ("task", task),
        Some(("section", section)),
        Some(ALICE),
        seconds,
    )
}

fn left(task: &str, section: &str, seconds: i64) -> asana_webhooks::Event {
    event(
        "removed",
        ("task", task),
        Some(("section", section)),
        Some(ALICE),
        seconds,
    )
}

#[tokio::test]
async fn the_integrations_own_writes_are_dropped() {
    let resolver = Resolver::builder()
        .ignore_user(BOT.parse::<UserGid>().unwrap())
        .build();
    let ours = event(
        "added",
        ("task", TASK_A),
        Some(("section", SECTION_A)),
        Some(BOT),
        0,
    );
    let theirs = entered(TASK_B, SECTION_A, 1);
    let system = event("changed", ("task", TASK_A), None, None, 2);

    let kept = resolver
        .accept(&batch(vec![ours, theirs.clone(), system.clone()]), NOW)
        .await
        .unwrap();

    assert_eq!(kept, vec![theirs, system]);
}

#[tokio::test]
async fn stale_events_are_dropped() {
    let resolver = Resolver::builder()
        .max_age(Duration::from_secs(10 * 60))
        .build();
    // NOW is 09:20; the first event is 20 minutes old, the second 5.
    let stale = entered(TASK_A, SECTION_A, 0);
    let fresh = entered(TASK_B, SECTION_A, 15 * 60);

    let kept = resolver
        .accept(&batch(vec![stale, fresh.clone()]), NOW)
        .await
        .unwrap();

    assert_eq!(kept, vec![fresh]);
}

#[tokio::test]
async fn repeats_are_dropped_until_the_dedupe_forgets_them() {
    let clock = ManualClock::new(NOW);
    let dedupe = MemoryDedupe::with_clock(clock.clock());
    let resolver = Resolver::builder()
        .dedupe(&dedupe)
        .dedupe_ttl(Duration::from_secs(60 * 60))
        .build();
    let delivery = batch(vec![
        entered(TASK_A, SECTION_A, 0),
        changed(TASK_A, "due_on", 1),
    ]);

    assert_eq!(resolver.accept(&delivery, NOW).await.unwrap().len(), 2);
    // Asana redelivers the same batch.
    assert!(resolver.accept(&delivery, NOW).await.unwrap().is_empty());
    // A repeat within one batch is dropped too.
    let doubled = batch(vec![changed(TASK_B, "name", 5), changed(TASK_B, "name", 5)]);
    assert_eq!(resolver.accept(&doubled, NOW).await.unwrap().len(), 1);

    clock.advance(Duration::from_secs(61 * 60));
    assert_eq!(resolver.accept(&delivery, NOW).await.unwrap().len(), 2);
}

#[test]
fn a_move_through_two_sections_coalesces_into_the_newest() {
    let resolver = Resolver::builder().build();
    // A → B, then B → C a few seconds later.
    let events = vec![
        left(TASK_A, SECTION_A, 0),
        entered(TASK_A, SECTION_B, 0),
        left(TASK_A, SECTION_B, 5),
        entered(TASK_A, SECTION_C, 5),
    ];

    let groups = resolver.coalesce(events);

    assert_eq!(groups.len(), 1, "{groups:?}");
    match &groups[0] {
        Coalesced::SectionEntered {
            task: moved,
            section: to,
            by,
            ..
        } => {
            assert_eq!(moved, &task(TASK_A));
            assert_eq!(to, &section(SECTION_C));
            assert_eq!(by.as_ref().map(Gid::as_str), Some(ALICE));
        }
        other => panic!("expected SectionEntered, got {other:?}"),
    }
}

#[test]
fn at_the_same_instant_an_entry_beats_a_departure_whatever_the_order() {
    let resolver = Resolver::builder().build();
    let events = vec![entered(TASK_A, SECTION_B, 0), left(TASK_A, SECTION_A, 0)];

    let groups = resolver.coalesce(events);

    assert!(
        matches!(&groups[..], [Coalesced::SectionEntered { section: to, .. }] if to == &section(SECTION_B)),
        "{groups:?}"
    );
}

#[test]
fn changes_merge_per_task_and_groups_come_out_in_time_order() {
    let resolver = Resolver::builder().build();
    let events = vec![
        changed(TASK_B, "due_on", 30),
        changed(TASK_A, "assignee", 10),
        changed(TASK_A, "due_on", 20),
        changed(TASK_A, "assignee", 25),
        event(
            "added",
            ("task", TASK_A),
            Some(("project", PROJECT)),
            Some(ALICE),
            1,
        ),
        event(
            "added",
            ("story", "1203000000000001"),
            Some(("task", TASK_A)),
            Some(ALICE),
            2,
        ),
    ];

    let groups = resolver.coalesce(events);

    assert_eq!(groups.len(), 4, "{groups:?}");
    assert!(matches!(&groups[0], Coalesced::TaskAdded { .. }));
    assert!(
        matches!(&groups[1], Coalesced::StoryAdded { task: Some(on), .. } if on == &task(TASK_A))
    );
    match &groups[2] {
        Coalesced::TaskChanged {
            task: changed_task,
            fields,
            at,
            ..
        } => {
            assert_eq!(changed_task, &task(TASK_A));
            assert_eq!(fields, &["assignee".to_owned(), "due_on".to_owned()]);
            assert_eq!(*at, groups[2].at());
            assert_eq!(at.second(), 25);
        }
        other => panic!("expected TaskChanged, got {other:?}"),
    }
    assert_eq!(groups[3].task(), Some(&task(TASK_B)));
}

#[test]
fn a_deleted_task_reports_only_its_deletion() {
    let resolver = Resolver::builder().build();
    let events = vec![
        entered(TASK_A, SECTION_A, 0),
        changed(TASK_A, "name", 1),
        event("deleted", ("task", TASK_A), None, Some(ALICE), 2),
        event("undeleted", ("task", TASK_B), None, Some(ALICE), 3),
    ];

    let groups = resolver.coalesce(events);

    assert_eq!(groups.len(), 2, "{groups:?}");
    assert!(matches!(
        &groups[0],
        Coalesced::Deleted { resource: ResourceGid::Task(gone), .. } if gone == &task(TASK_A)
    ));
    assert!(matches!(&groups[1], Coalesced::Other(_)));
}

#[test]
fn a_shared_seen_set_drops_an_entry_already_reported_and_forgets_on_departure() {
    let seen = SeenTasks::new(Duration::from_secs(3600));
    seen.first_sighting(&task(TASK_A), &section(SECTION_A));
    let resolver = Resolver::builder().seen(seen.clone()).build();

    assert!(
        resolver
            .coalesce(vec![entered(TASK_A, SECTION_A, 0)])
            .is_empty()
    );
    let left_groups = resolver.coalesce(vec![left(TASK_A, SECTION_A, 10)]);
    assert!(matches!(&left_groups[..], [Coalesced::SectionLeft { .. }]));
    assert!(!seen.contains(&task(TASK_A), &section(SECTION_A)));
    assert_eq!(
        resolver
            .coalesce(vec![entered(TASK_A, SECTION_A, 20)])
            .len(),
        1
    );
}

#[tokio::test]
async fn a_task_is_reread_and_confirmed_in_its_section() {
    let transport = Scripted::replies(vec![Ok(reply(
        200,
        &serde_json::json!({
            "data": {
                "gid": TASK_A,
                "resource_type": "task",
                "name": "Acme onboarding",
                "memberships": [
                    {
                        "project": {"gid": PROJECT, "resource_type": "project"},
                        "section": {"gid": SECTION_B, "resource_type": "section"}
                    }
                ]
            }
        })
        .to_string(),
    ))]);
    let client = client(transport.clone());

    let now = reread(
        &client,
        &task(TASK_A),
        fields![Task: name, memberships.section],
    )
    .await
    .unwrap();

    assert!(confirm_in_section(&now, &section(SECTION_B)));
    assert!(!confirm_in_section(&now, &section(SECTION_A)));
    let request = transport.request(0);
    assert!(request.uri().path().ends_with(&format!("/tasks/{TASK_A}")));
    assert!(query(&request).contains(&("opt_fields".into(), "name,memberships.section".into())));
}
