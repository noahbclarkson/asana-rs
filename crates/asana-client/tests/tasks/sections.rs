//! `client.sections()`: listing, the single-section calls, moving a task in, finding a section
//! by name, and the section directory's time-to-live.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use asana_client::NameMatch;
use asana_client::fields;
use asana_client::openapi::ids::SectionGid;
use asana_client::resources::sections::Clock;
use futures_util::TryStreamExt;

use crate::support::{Scripted, client, reply};
use crate::{
    EMPTY, OTHER_SECTION, OTHER_TASK, PROJECT, SECTION, TASK, answering, gid, json, page, paged,
    sent,
};

fn section(gid: &str, name: &str) -> serde_json::Value {
    serde_json::json!({ "gid": gid, "resource_type": "section", "name": name })
}

fn single(gid: &str, name: &str) -> String {
    serde_json::json!({ "data": section(gid, name) }).to_string()
}

/// A project's sections as one page.
fn sections(named: &[(&str, &str)]) -> String {
    let items: Vec<_> = named.iter().map(|(gid, name)| section(gid, name)).collect();
    page(&items, None)
}

#[tokio::test(start_paused = true)]
async fn a_projects_sections_stream_across_pages() {
    let (transport, client) = paged(vec![
        (None, page(&[section(SECTION, "To do")], Some("b2Zmc2V0"))),
        (
            Some("b2Zmc2V0"),
            page(&[section(OTHER_SECTION, "Done")], None),
        ),
    ]);
    let listed: Vec<_> = client
        .sections()
        .list(gid(PROJECT))
        .fields(fields![Section: name, created_at])
        .stream()
        .try_collect()
        .await
        .unwrap();

    let names: Vec<_> = listed.iter().filter_map(|s| s.name.as_deref()).collect();
    assert_eq!(names, ["To do", "Done"]);
    let first = sent(&transport, 0);
    assert_eq!(first.method, "GET");
    assert_eq!(first.path, format!("/projects/{PROJECT}/sections"));
    assert_eq!(first.param("opt_fields"), Some("name,created_at"));
    assert_eq!(sent(&transport, 1).param("offset"), Some("b2Zmc2V0"));
}

#[tokio::test(start_paused = true)]
async fn get_create_update_and_delete_send_their_requests() {
    let (transport, client) = answering(200, &single(SECTION, "Next Actions"));
    let sections = client.sections();
    let read = sections.get(gid(SECTION)).send().await.unwrap();
    assert_eq!(read.name.as_deref(), Some("Next Actions"));
    sections
        .create(gid(PROJECT), "Next Actions")
        .insert_after(gid(OTHER_SECTION))
        .send()
        .await
        .unwrap();
    sections
        .update(gid(SECTION))
        .name("Waiting")
        .send()
        .await
        .unwrap();

    let get = sent(&transport, 0);
    assert_eq!(
        (get.method.as_str(), get.path),
        ("GET", format!("/sections/{SECTION}"))
    );
    let create = sent(&transport, 1);
    assert_eq!(create.method, "POST");
    assert_eq!(create.path, format!("/projects/{PROJECT}/sections"));
    assert_eq!(
        create.body.unwrap(),
        json(&format!(
            r#"{{"data": {{"name": "Next Actions", "insert_after": "{OTHER_SECTION}"}}}}"#
        ))
    );
    let update = sent(&transport, 2);
    assert_eq!(update.method, "PUT");
    assert_eq!(update.path, format!("/sections/{SECTION}"));
    assert_eq!(
        update.body.unwrap(),
        json(r#"{"data": {"name": "Waiting"}}"#)
    );

    let (transport, client) = answering(200, EMPTY);
    client.sections().delete(gid(SECTION)).send().await.unwrap();
    let delete = sent(&transport, 0);
    assert_eq!(delete.method, "DELETE");
    assert_eq!(delete.path, format!("/sections/{SECTION}"));
}

#[tokio::test(start_paused = true)]
async fn moving_a_task_into_a_section_is_retried_after_a_server_error() {
    let transport = Scripted::replies(vec![
        Ok(reply(503, r#"{"errors": [{"message": "Down"}]}"#)),
        Ok(reply(200, EMPTY)),
    ]);
    let client = client(Arc::clone(&transport));
    client
        .sections()
        .add_task(gid(SECTION), gid(TASK))
        .insert_before(gid(OTHER_TASK))
        .send()
        .await
        .unwrap();

    assert_eq!(transport.count(), 2);
    for index in 0..2 {
        let request = sent(&transport, index);
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, format!("/sections/{SECTION}/addTask"));
        assert_eq!(
            request.body.unwrap(),
            json(&format!(
                r#"{{"data": {{"task": "{TASK}", "insert_before": "{OTHER_TASK}"}}}}"#
            ))
        );
    }
}

#[tokio::test(start_paused = true)]
async fn find_by_name_says_whether_none_one_or_several_match() {
    let listing = sections(&[
        (SECTION, "  Send Documents "),
        (OTHER_SECTION, "Done"),
        ("1200000000000202", "done"),
    ]);
    let (transport, client) = answering(200, &listing);

    let one = client
        .sections()
        .find_by_name(gid(PROJECT), "Send Documents")
        .send()
        .await
        .unwrap();
    assert_eq!(
        one.one().and_then(|s| s.gid),
        Some(gid::<SectionGid>(SECTION))
    );
    let request = sent(&transport, 0);
    assert_eq!(request.path, format!("/projects/{PROJECT}/sections"));
    assert_eq!(request.param("opt_fields"), Some("name"));

    let none = client
        .sections()
        .find_by_name(gid(PROJECT), "send documents")
        .send()
        .await
        .unwrap();
    assert_eq!(none, NameMatch::Missing);

    let exact = client
        .sections()
        .find_by_name(gid(PROJECT), "Done")
        .send()
        .await
        .unwrap();
    assert!(matches!(exact, NameMatch::One(_)));

    let several = client
        .sections()
        .find_by_name(gid(PROJECT), " DONE")
        .ignore_case()
        .send()
        .await
        .unwrap();
    let NameMatch::Several(both) = several else {
        panic!("two sections are named done, ignoring case: {several:?}");
    };
    let gids: Vec<_> = both.into_iter().filter_map(|s| s.gid).collect();
    assert_eq!(
        gids,
        [gid::<SectionGid>(OTHER_SECTION), gid("1200000000000202")]
    );
}

/// A clock a test moves by hand.
#[derive(Clone)]
struct ManualClock(Arc<Mutex<Instant>>);

impl ManualClock {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(Instant::now())))
    }

    fn advance(&self, by: Duration) {
        *self.0.lock().unwrap() += by;
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Instant {
        *self.0.lock().unwrap()
    }
}

#[tokio::test(start_paused = true)]
async fn the_directory_lists_again_only_after_its_time_to_live() {
    let transport = Scripted::new(|_, index| {
        let renamed = if index == 0 { "Send Documents" } else { "Sent" };
        Ok(reply(
            200,
            &sections(&[(SECTION, renamed), (OTHER_SECTION, "Done")]),
        ))
    });
    let client = client(Arc::clone(&transport));
    let clock = ManualClock::new();
    let directory = client
        .sections()
        .directory(gid(PROJECT), Duration::from_secs(300))
        .with_clock(clock.clone());

    let found = directory.by_name("Send Documents").await.unwrap();
    assert_eq!(found, NameMatch::One(gid(SECTION)));
    assert_eq!(transport.count(), 1);

    clock.advance(Duration::from_secs(299));
    assert_eq!(
        directory
            .name_of(&gid(OTHER_SECTION))
            .await
            .unwrap()
            .as_deref(),
        Some("Done")
    );
    assert_eq!(
        directory
            .by_name_ignore_case("send documents")
            .await
            .unwrap(),
        NameMatch::One(gid(SECTION))
    );
    assert_eq!(transport.count(), 1, "still within the time-to-live");

    clock.advance(Duration::from_secs(1));
    assert_eq!(
        directory.by_name("Send Documents").await.unwrap(),
        NameMatch::Missing
    );
    assert_eq!(
        transport.count(),
        2,
        "listed again once the time-to-live ran out"
    );
    assert_eq!(
        directory.name_of(&gid(SECTION)).await.unwrap().as_deref(),
        Some("Sent")
    );

    directory.refresh().await.unwrap();
    assert_eq!(transport.count(), 3, "refresh lists at once");
    directory.invalidate().await;
    directory.sections().await.unwrap();
    assert_eq!(transport.count(), 4, "a forgotten listing is made again");
}

#[tokio::test(start_paused = true)]
async fn a_failed_listing_keeps_the_previous_one_and_is_tried_again() {
    let transport = Scripted::new(|_, index| {
        if index == 1 {
            Ok(reply(403, r#"{"errors": [{"message": "Forbidden"}]}"#))
        } else {
            Ok(reply(200, &sections(&[(SECTION, "To do")])))
        }
    });
    let client = client(Arc::clone(&transport));
    let directory = client
        .sections()
        .directory(gid(PROJECT), Duration::from_secs(60));

    directory.by_name("To do").await.unwrap();
    assert!(directory.refresh().await.is_err());
    assert_eq!(
        directory.by_name("To do").await.unwrap(),
        NameMatch::One(gid(SECTION)),
        "the earlier listing is still within its time-to-live"
    );
    assert_eq!(transport.count(), 2);
}
