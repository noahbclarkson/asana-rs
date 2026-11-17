//! Decoding deliveries, the routing helpers, and event keys.

use asana_openapi::ids::Gid;
use asana_webhooks::{
    DecodeError, Event, EventAction, Limits, ResourceGid, ResourceType, UnreadableReason, sign,
    verify,
};
use time::macros::datetime;

use crate::{documented_secret, receive};

const SECTION_MOVE: &str = r#"{"events":[
  {
    "user": {"gid": "1201234567890001", "resource_type": "user", "name": "Ada Example"},
    "created_at": "2026-03-02T09:15:00.123Z",
    "action": "removed",
    "resource": {"gid": "1201234567890123", "resource_type": "task", "resource_subtype": "default_task", "name": "Send the Acme proposal"},
    "parent": {"gid": "1201234567890400", "resource_type": "section", "name": "Drafting"}
  },
  {
    "user": {"gid": "1201234567890001", "resource_type": "user", "name": "Ada Example"},
    "created_at": "2026-03-02T09:15:00.123Z",
    "action": "added",
    "resource": {"gid": "1201234567890123", "resource_type": "task", "resource_subtype": "default_task", "name": "Send the Acme proposal"},
    "parent": {"gid": "1201234567890456", "resource_type": "section", "name": "Ready to sign"}
  },
  {
    "user": {"gid": "1201234567890001", "resource_type": "user"},
    "created_at": "2026-03-02T09:15:00.456Z",
    "action": "added",
    "resource": {"gid": "1201234567890789", "resource_type": "story", "resource_subtype": "section_changed"},
    "parent": {"gid": "1201234567890123", "resource_type": "task", "resource_subtype": "default_task"}
  }
]}"#;

#[test]
fn a_section_move_decodes_into_left_entered_and_a_story() {
    let batch = receive(SECTION_MOVE);
    assert!(batch.unreadable.is_empty());
    assert!(!batch.is_heartbeat());
    let [left, entered, story] = batch.events.as_slice() else {
        panic!("three events");
    };

    let (task, section) = entered.section_entered().unwrap();
    assert_eq!(task.as_str(), "1201234567890123");
    assert_eq!(section.as_str(), "1201234567890456");
    assert_eq!(entered.section_left(), None);
    assert_eq!(entered.task_added_to_project(), None);

    let (task, section) = left.section_left().unwrap();
    assert_eq!(
        (task.as_str(), section.as_str()),
        ("1201234567890123", "1201234567890400")
    );
    assert_eq!(left.section_entered(), None);

    let user = entered.user.as_ref().unwrap();
    assert_eq!(user.gid.as_str(), "1201234567890001");
    assert_eq!(user.name.as_deref(), Some("Ada Example"));
    assert!(entered.is_by(&user.gid));
    assert_eq!(
        entered.resource.resource_subtype.as_deref(),
        Some("default_task")
    );
    assert_eq!(
        entered.resource.name.as_deref(),
        Some("Send the Acme proposal")
    );
    assert_eq!(entered.created_at, datetime!(2026-03-02 09:15:00.123 UTC));
    assert_eq!(entered.change, None);

    let (story_gid, on_task) = story.story_added().unwrap();
    assert_eq!(story_gid.as_str(), "1201234567890789");
    assert_eq!(on_task.unwrap().as_str(), "1201234567890123");
    assert_eq!(story.section_entered(), None);
}

#[test]
fn a_system_event_has_no_user_and_a_missing_parent_is_none() {
    let batch = receive(
        r#"{"events":[{
          "user": null,
          "created_at": "2026-03-02T09:20:00Z",
          "action": "changed",
          "resource": {"gid": "1201234567890123", "resource_type": "task"},
          "change": {"field": "completed", "action": "changed"}
        }]}"#,
    );
    let event = &batch.events[0];
    assert_eq!(event.user, None);
    assert_eq!(event.actor(), None);
    assert_eq!(event.parent, None);
    let (task, field) = event.task_changed().unwrap();
    assert_eq!((task.as_str(), field), ("1201234567890123", "completed"));
    let change = event.change.as_ref().unwrap();
    assert_eq!(change.action, EventAction::Changed);
    assert_eq!(change.new_value, None);
}

#[test]
fn change_values_are_resources_when_they_name_one_and_raw_json_always() {
    let batch = receive(
        r#"{"events":[
          {
            "user": {"gid": "1201234567890001", "resource_type": "user"},
            "created_at": "2026-03-02T09:21:00.000Z",
            "action": "changed",
            "resource": {"gid": "1201234567890123", "resource_type": "task"},
            "parent": null,
            "change": {"field": "assignee", "action": "changed",
                       "new_value": {"gid": "1201234567890002", "resource_type": "user"}}
          },
          {
            "user": {"gid": "1201234567890001", "resource_type": "user"},
            "created_at": "2026-03-02T09:22:00.000Z",
            "action": "changed",
            "resource": {"gid": "1201234567890123", "resource_type": "task"},
            "change": {"field": "followers", "action": "added",
                       "added_value": {"gid": "1201234567890003", "resource_type": "user"}}
          },
          {
            "user": {"gid": "1201234567890001", "resource_type": "user"},
            "created_at": "2026-03-02T09:23:00.000Z",
            "action": "changed",
            "resource": {"gid": "1201234567890123", "resource_type": "task"},
            "change": {"field": "custom_fields", "action": "changed",
                       "new_value": {"gid": "1201234567890900", "resource_type": "custom_field",
                                     "resource_subtype": "enum",
                                     "enum_value": {"gid": "1201234567890901", "name": "High"}}}
          },
          {
            "user": {"gid": "1201234567890001", "resource_type": "user"},
            "created_at": "2026-03-02T09:24:00.000Z",
            "action": "changed",
            "resource": {"gid": "1201234567890123", "resource_type": "task"},
            "change": {"field": "assignee", "action": "changed", "new_value": null}
          }
        ]}"#,
    );
    let [assignee, follower, custom, cleared] = batch.events.as_slice() else {
        panic!("four events");
    };

    let value = assignee
        .change
        .as_ref()
        .unwrap()
        .new_value
        .as_ref()
        .unwrap();
    let user = value.resource.as_ref().unwrap();
    assert_eq!(user.kind(), ResourceType::User);
    assert_eq!(
        user.gid_as::<asana_openapi::ids::UserGid>()
            .unwrap()
            .as_str(),
        "1201234567890002"
    );

    let change = follower.change.as_ref().unwrap();
    assert_eq!(change.action, EventAction::Added);
    let added = change
        .added_value
        .as_ref()
        .unwrap()
        .resource
        .as_ref()
        .unwrap();
    assert!(matches!(&added.gid, ResourceGid::User(gid) if gid.as_str() == "1201234567890003"));

    let (task, field) = custom.custom_field_changed().unwrap();
    assert_eq!(
        (task.as_str(), field.as_str()),
        ("1201234567890123", "1201234567890900")
    );
    let raw = &custom
        .change
        .as_ref()
        .unwrap()
        .new_value
        .as_ref()
        .unwrap()
        .raw;
    assert_eq!(raw["enum_value"]["name"], "High");
    assert_eq!(assignee.custom_field_changed(), None);

    let cleared = cleared.change.as_ref().unwrap().new_value.as_ref().unwrap();
    assert!(cleared.raw.is_null());
    assert_eq!(cleared.resource, None);
}

#[test]
fn unknown_types_actions_and_extra_fields_are_kept() {
    let batch = receive(
        r#"{"events":[{
          "user": {"gid": "1201234567890001", "resource_type": "user", "email": "ada@example.com"},
          "created_at": "2026-03-02T09:30:00.000Z",
          "action": "archived",
          "type": "allocation",
          "resource": {"gid": "1201234567890555", "resource_type": "allocation"},
          "parent": {"gid": "1201234567890600", "resource_type": "project"},
          "something_new": {"nested": [1, 2, 3]}
        }]}"#,
    );
    let event = &batch.events[0];
    assert_eq!(event.action, EventAction::Unknown("archived".to_owned()));
    assert_eq!(
        event.resource.gid,
        ResourceGid::Other {
            resource_type: "allocation".to_owned(),
            gid: "1201234567890555".to_owned()
        }
    );
    assert_eq!(
        event.resource.kind(),
        ResourceType::Other("allocation".to_owned())
    );
    assert!(matches!(
        event.parent.as_ref().unwrap().gid,
        ResourceGid::Project(_)
    ));
    assert_eq!(event.task_removed_from_project(), None);
}

#[test]
fn an_unreadable_event_is_kept_without_failing_its_neighbours() {
    let batch = receive(
        r#"{"events":[
          "not an event",
          {"created_at": "2026-03-02T09:30:00Z", "action": "added"},
          {"created_at": "yesterday", "action": "added",
           "resource": {"gid": "1201234567890123", "resource_type": "task"}},
          {"created_at": "2026-03-02T09:30:00Z", "action": "added",
           "resource": {"gid": "12/../34", "resource_type": "task"}},
          {"created_at": "2026-03-02T09:30:00Z", "action": "deleted",
           "resource": {"gid": "1201234567890123", "resource_type": "task"}}
        ]}"#,
    );
    assert_eq!(batch.events.len(), 1);
    assert_eq!(
        batch.events[0].task_deleted().unwrap().as_str(),
        "1201234567890123"
    );
    let reasons: Vec<_> = batch
        .unreadable
        .iter()
        .map(|unreadable| (unreadable.index, unreadable.reason))
        .collect();
    assert_eq!(
        reasons,
        [
            (0, UnreadableReason::NotAnObject("event")),
            (1, UnreadableReason::Missing("resource")),
            (2, UnreadableReason::InvalidTimestamp),
            (3, UnreadableReason::InvalidGid("resource.gid")),
        ]
    );
    assert_eq!(batch.unreadable[0].raw, "not an event");
    assert!(!batch.is_heartbeat());
}

#[test]
fn a_heartbeat_is_an_empty_events_array_or_an_empty_body() {
    assert!(receive(r#"{"events":[]}"#).is_heartbeat());
    assert!(receive("").is_heartbeat());
}

#[test]
fn a_body_that_is_not_a_batch_fails_whole() {
    let secret = documented_secret();
    let decode = |body: &[u8], limits: &Limits| {
        verify(&secret, &sign(&secret, body), body)
            .unwrap()
            .decode_with(limits)
    };
    let limits = Limits::default();
    assert!(matches!(
        decode(b"{\"events\":[", &limits),
        Err(DecodeError::NotJson { .. })
    ));
    assert_eq!(decode(b"{}", &limits), Err(DecodeError::NoEventsArray));
    assert_eq!(decode(b"[]", &limits), Err(DecodeError::NoEventsArray));
    assert_eq!(
        decode(b"{\"events\":{}}", &limits),
        Err(DecodeError::NoEventsArray)
    );

    let mut small = Limits::default();
    assert_eq!(small.max_events, 1_000);
    small.max_events = 2;
    let error = decode(b"{\"events\":[1,2,3]}", &small).unwrap_err();
    assert_eq!(error, DecodeError::TooManyEvents { count: 3, limit: 2 });
    assert_eq!(error.status_hint(), 413);
    assert_eq!(decode(b"nope", &limits).unwrap_err().status_hint(), 400);
}

fn first_event(body: &str) -> Event {
    receive(body)
        .events
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("no event"))
}

#[test]
fn an_event_key_survives_re_encoding_and_tells_events_apart() {
    let compact = first_event(
        r#"{"events":[{"user":{"gid":"1201234567890001","resource_type":"user"},"created_at":"2026-03-02T09:21:00.000Z","action":"changed","resource":{"gid":"1201234567890123","resource_type":"task"},"change":{"field":"custom_fields","action":"changed","new_value":{"gid":"1201234567890900","resource_type":"custom_field","enum_value":{"name":"High","gid":"1201234567890901"}}}}]}"#,
    );
    // The same event: keys reordered, spaced out, the time in another offset, a name added.
    let reordered = first_event(
        r#"{ "events" : [ {
          "change": {"new_value": {"enum_value": {"gid": "1201234567890901", "name": "High"},
                                   "resource_type": "custom_field", "gid": "1201234567890900"},
                     "action": "changed", "field": "custom_fields"},
          "resource": {"resource_type": "task", "gid": "1201234567890123", "name": "Renamed since"},
          "action": "changed",
          "created_at": "2026-03-02T22:21:00+13:00",
          "user": {"resource_type": "user", "gid": "1201234567890001"}
        } ] }"#,
    );
    assert_eq!(compact.key(), reordered.key());

    // Through serde and back.
    let stored = serde_json::to_string(&compact).unwrap();
    let restored: Event = serde_json::from_str(&stored).unwrap();
    assert_eq!(restored, compact);
    assert_eq!(restored.key(), compact.key());
    assert_eq!(
        asana_webhooks::EventKey::from_hex(&compact.key().to_string()),
        Some(compact.key())
    );

    // Any change in what happened changes the key.
    let mut later = compact.clone();
    later.created_at += time::Duration::milliseconds(1);
    let mut other_action = compact.clone();
    other_action.action = EventAction::Added;
    let mut other_value = compact.clone();
    let value = other_value
        .change
        .as_mut()
        .unwrap()
        .new_value
        .as_mut()
        .unwrap();
    value.raw["enum_value"]["name"] = "Low".into();
    let mut by_system = compact.clone();
    by_system.user = None;
    for different in [later, other_action, other_value, by_system] {
        assert_ne!(different.key(), compact.key());
    }
}

#[test]
fn age_is_measured_from_created_at() {
    let event = first_event(
        r#"{"events":[{"created_at":"2026-03-02T09:00:00Z","action":"deleted",
           "resource":{"gid":"1201234567890123","resource_type":"task"}}]}"#,
    );
    let now = datetime!(2026-03-02 10:30:00 UTC);
    assert_eq!(event.age(now), time::Duration::minutes(90));
    assert!(event.is_older_than(std::time::Duration::from_secs(3600), now));
    assert!(!event.is_older_than(std::time::Duration::from_secs(2 * 3600), now));
    assert!(event.age(datetime!(2026-03-02 08:59:00 UTC)).is_negative());
}
