//! The events stream across the first sync's 412, two pages and an expired token.

use std::time::Duration;

use asana_client::openapi::ids::ProjectGid;
use asana_client::resources::events::EventsItem;
use asana_client::{Error, SyncToken};
use futures_util::StreamExt;
use serde_json::json;

use crate::support::{Scripted, client, reply};
use crate::{USER, gid, query};

const PROJECT: &str = "1200000000000100";

fn event(task: &str) -> serde_json::Value {
    json!({
        "user": { "gid": USER, "resource_type": "user", "name": "Acme Ops" },
        "created_at": "2026-01-15T02:06:58.147Z",
        "type": "task",
        "action": "changed",
        "resource": { "gid": task, "resource_type": "task", "name": "Bug Task" },
        "parent": null,
        "change": { "field": "assignee", "action": "changed",
                    "new_value": { "gid": USER, "resource_type": "user" } },
    })
}

fn expired(sync: &str) -> http::Response<bytes::Bytes> {
    reply(
        412,
        &json!({
            "errors": [{ "message": "Sync token invalid or too old. If you are attempting to keep resources in sync, you must fetch the full dataset for this query now and use the new sync token for the next sync." }],
            "sync": sync,
        })
        .to_string(),
    )
}

fn page(
    events: Vec<serde_json::Value>,
    sync: &str,
    has_more: bool,
) -> http::Response<bytes::Bytes> {
    reply(
        200,
        &json!({ "data": events, "sync": sync, "has_more": has_more }).to_string(),
    )
}

fn token(text: &str) -> SyncToken {
    SyncToken::new(text)
}

#[tokio::test(start_paused = true)]
async fn a_poller_starts_reads_two_pages_and_stops_at_an_expired_token() {
    let transport = Scripted::replies(vec![
        Ok(expired("s1")),
        Ok(page(
            vec![event("1201234567890001"), json!({ "action": "changed" })],
            "s2",
            true,
        )),
        Ok(page(vec![event("1201234567890003")], "s3", false)),
        Ok(expired("s9")),
    ]);
    let client = client(transport.clone());
    let project: ProjectGid = gid(PROJECT);
    let items: Vec<_> = client
        .events()
        .poll(&project, None, Duration::from_secs(30))
        .collect()
        .await;

    assert_eq!(items.len(), 6, "{items:?}");
    assert_eq!(
        items[0].as_ref().unwrap(),
        &EventsItem::CaughtUp { sync: token("s1") }
    );
    // Within a page, resuming re-reads the page; after its last event, it moves on.
    match items[1].as_ref().unwrap() {
        EventsItem::Event { event, sync } => {
            assert_eq!(event.resource.gid.gid_str(), "1201234567890001");
            assert_eq!(sync, &token("s1"));
        }
        other => panic!("expected an event, got {other:?}"),
    }
    match items[2].as_ref().unwrap() {
        EventsItem::Unreadable { event, sync } => {
            assert_eq!(event.index, 1);
            assert_eq!(sync, &token("s2"));
        }
        other => panic!("expected an unreadable event, got {other:?}"),
    }
    assert_eq!(items[3].as_ref().unwrap().sync(), &token("s3"));
    assert_eq!(
        items[4].as_ref().unwrap(),
        &EventsItem::CaughtUp { sync: token("s3") }
    );
    assert!(matches!(
        items[5],
        Err(Error::SyncTokenExpired { ref sync }) if sync == &token("s9")
    ));

    // The pages were asked for with each token in turn, a has_more page at once, and the poll
    // after catching up waited its interval.
    let syncs: Vec<_> = (0..4)
        .map(|index| {
            query(&transport.request(index))
                .into_iter()
                .find(|(name, _)| name == "sync")
                .map(|(_, value)| value)
        })
        .collect();
    assert_eq!(
        syncs,
        [
            None,
            Some("s1".into()),
            Some("s2".into()),
            Some("s3".into())
        ]
    );
    let times = transport.times();
    assert_eq!(times[1] - times[0], Duration::from_secs(30));
    assert_eq!(times[2], times[1]);
    assert_eq!(times[3] - times[2], Duration::from_secs(30));
    assert!(query(&transport.request(0)).contains(&("resource".into(), PROJECT.into())));
}

#[tokio::test(start_paused = true)]
async fn a_stream_ends_once_caught_up_and_sync_handles_the_first_412() {
    let transport = Scripted::replies(vec![
        Ok(page(vec![event("1201234567890001")], "s2", false)),
        Ok(expired("s1")),
    ]);
    let client = client(transport.clone());
    let project: ProjectGid = gid(PROJECT);
    let items: Vec<_> = client
        .events()
        .stream(&project, Some(token("s1")))
        .collect()
        .await;
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].as_ref().unwrap().sync(), &token("s2"));
    assert_eq!(
        items[1].as_ref().unwrap(),
        &EventsItem::CaughtUp { sync: token("s2") }
    );

    let first = client.events().sync(&project, None).await.unwrap();
    assert!(first.events.is_empty() && !first.has_more);
    assert_eq!(first.sync, token("s1"));
}

#[tokio::test(start_paused = true)]
async fn an_expired_token_given_to_sync_is_the_typed_error() {
    let transport = Scripted::new(|_, _| Ok(expired("s7")));
    let client = client(transport);
    let project: ProjectGid = gid(PROJECT);
    let error = client
        .events()
        .sync(&project, Some(token("old")))
        .await
        .unwrap_err();
    assert!(matches!(error, Error::SyncTokenExpired { sync } if sync == token("s7")));
}
