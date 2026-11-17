//! The resolver: what it drops, how it groups, and re-reading a task before acting.

use std::time::Duration;

use asana_client::openapi::fields;
use asana_client::openapi::ids::{Gid, SectionGid, TaskGid, UserGid};
use asana_client::webhooks::{Coalesced, Resolver, SeenTasks, confirm_in_section, reread};
use asana_webhooks::{EventBatch, ManualClock, MemoryDedupe, ResourceGid};

use crate::support::{Scripted, client, reply};
use crate::{
    ALICE, BOT, NOW, PROJECT, SECTION_A, SECTION_B, SECTION_C, SECTION_D, TASK_A, TASK_B, changed,
    event, query,
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

/// The section a group is about, and whether the task entered it.
fn section_state(group: &Coalesced) -> (SectionGid, bool) {
    match group {
        Coalesced::SectionEntered { section, .. } => (section.clone(), true),
        Coalesced::SectionLeft { section, .. } => (section.clone(), false),
        other => panic!("expected a section group, got {other:?}"),
    }
}

#[test]
fn a_move_within_one_project_reports_the_departure_then_the_arrival() {
    let resolver = Resolver::builder().build();
    // Asana sends the arrival first as often as not.
    let events = vec![entered(TASK_A, SECTION_B, 0), left(TASK_A, SECTION_A, 0)];

    let groups = resolver.coalesce(events);

    let states: Vec<_> = groups.iter().map(section_state).collect();
    assert_eq!(
        states,
        vec![(section(SECTION_A), false), (section(SECTION_B), true)],
        "{groups:?}"
    );
    match &groups[1] {
        Coalesced::SectionEntered {
            task: moved, by, ..
        } => {
            assert_eq!(moved, &task(TASK_A));
            assert_eq!(by.as_ref().map(Gid::as_str), Some(ALICE));
        }
        other => panic!("expected SectionEntered, got {other:?}"),
    }
}

#[test]
fn a_move_through_a_section_keeps_each_sections_newest_state() {
    let resolver = Resolver::builder().build();
    // A → B, then B → C a few seconds later.
    let events = vec![
        left(TASK_A, SECTION_A, 0),
        entered(TASK_A, SECTION_B, 0),
        left(TASK_A, SECTION_B, 5),
        entered(TASK_A, SECTION_C, 5),
    ];

    let states: Vec<_> = resolver
        .coalesce(events)
        .iter()
        .map(section_state)
        .collect();

    assert_eq!(
        states,
        vec![
            (section(SECTION_A), false),
            (section(SECTION_B), false),
            (section(SECTION_C), true),
        ]
    );
}

#[test]
fn moves_in_two_projects_in_one_delivery_are_all_reported() {
    let resolver = Resolver::builder().build();
    // In one project the task goes from A to B; in another, from C to D, all in one delivery.
    let events = vec![
        left(TASK_A, SECTION_A, 0),
        entered(TASK_A, SECTION_B, 0),
        left(TASK_A, SECTION_C, 1),
        entered(TASK_A, SECTION_D, 1),
    ];

    let states: Vec<_> = resolver
        .coalesce(events)
        .iter()
        .map(section_state)
        .collect();

    assert_eq!(
        states,
        vec![
            (section(SECTION_A), false),
            (section(SECTION_B), true),
            (section(SECTION_C), false),
            (section(SECTION_D), true),
        ]
    );
}

#[test]
fn a_task_removed_from_a_section_and_added_back_is_reported_as_entering_it() {
    let seen = SeenTasks::new(Duration::from_secs(3600));
    // The watcher or an earlier delivery already reported it in A.
    assert!(seen.first_sighting(&task(TASK_A), &section(SECTION_A)));
    let resolver = Resolver::builder().seen(seen.clone()).build();

    let groups = resolver.coalesce(vec![
        left(TASK_A, SECTION_A, 0),
        entered(TASK_A, SECTION_A, 5),
    ]);

    assert!(
        matches!(&groups[..], [Coalesced::SectionEntered { section: to, .. }] if to == &section(SECTION_A)),
        "{groups:?}"
    );
    assert!(seen.contains(&task(TASK_A), &section(SECTION_A)));
    // The same arrival again (from the watcher, say) is not news.
    assert!(
        resolver
            .coalesce(vec![entered(TASK_A, SECTION_A, 6)])
            .is_empty()
    );
}

#[test]
fn a_shared_seen_set_forgets_every_section_a_multi_homed_task_left() {
    let seen = SeenTasks::new(Duration::from_secs(3600));
    let resolver = Resolver::builder().seen(seen.clone()).build();
    // Reported in A (one project) and C (another).
    assert_eq!(
        resolver
            .coalesce(vec![
                entered(TASK_A, SECTION_A, 0),
                entered(TASK_A, SECTION_C, 0)
            ])
            .len(),
        2
    );

    // One delivery moves it on in both projects.
    let groups = resolver.coalesce(vec![
        left(TASK_A, SECTION_A, 10),
        entered(TASK_A, SECTION_B, 10),
        left(TASK_A, SECTION_C, 11),
        entered(TASK_A, SECTION_D, 11),
    ]);
    assert_eq!(groups.len(), 4, "{groups:?}");
    assert!(!seen.contains(&task(TASK_A), &section(SECTION_A)));
    assert!(!seen.contains(&task(TASK_A), &section(SECTION_C)));
    assert!(seen.contains(&task(TASK_A), &section(SECTION_B)));
    assert!(seen.contains(&task(TASK_A), &section(SECTION_D)));

    // So a later return to either old section is reported.
    let back = resolver.coalesce(vec![
        entered(TASK_A, SECTION_A, 20),
        entered(TASK_A, SECTION_C, 20),
    ]);
    assert_eq!(back.len(), 2, "{back:?}");
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
