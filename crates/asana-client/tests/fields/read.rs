//! Reading every kind of custom field value from a task, by the field's `resource_subtype`.

use asana_client::openapi::ids::CustomFieldGid;
use asana_client::openapi::models::Task;
use asana_client::{CustomFieldValue, TaskCustomFields};
use rust_decimal::Decimal;
use time::macros::{date, datetime};

/// A task with one field of every kind, shaped like the custom field values in Asana's
/// documentation.
const TASK: &str = r#"{
  "gid": "1201234567890123",
  "resource_type": "task",
  "name": "Acme renewal",
  "custom_fields": [
    {"gid": "1201234567890001", "resource_type": "custom_field", "name": "Notes",
     "resource_subtype": "text", "type": "text", "text_value": "Call before Friday",
     "display_value": "Call before Friday", "enabled": true},
    {"gid": "1201234567890002", "resource_type": "custom_field", "name": "Fee",
     "resource_subtype": "number", "type": "number", "precision": 2,
     "number_value": 1250.50, "display_value": "1250.50", "enabled": true},
    {"gid": "1201234567890003", "resource_type": "custom_field", "name": "Stage",
     "resource_subtype": "enum", "type": "enum", "display_value": "Approved",
     "enum_value": {"gid": "1201234567890031", "resource_type": "enum_option",
                    "name": "Approved", "enabled": true, "color": "green"}},
    {"gid": "1201234567890004", "resource_type": "custom_field", "name": "Channels",
     "resource_subtype": "multi_enum", "type": "multi_enum", "display_value": "Email, Post",
     "multi_enum_values": [
       {"gid": "1201234567890041", "resource_type": "enum_option", "name": "Email",
        "enabled": true, "color": "blue"},
       {"gid": "1201234567890042", "resource_type": "enum_option", "name": "Post",
        "enabled": false, "color": "none"}]},
    {"gid": "1201234567890005", "resource_type": "custom_field", "name": "Signed",
     "resource_subtype": "date", "type": "date", "display_value": "2026-12-17T02:30:00.000Z",
     "date_value": {"date": "2026-12-17", "date_time": "2026-12-17T02:30:00.000Z"}},
    {"gid": "1201234567890006", "resource_type": "custom_field", "name": "Reviewers",
     "resource_subtype": "people", "type": "people", "display_value": "Ada Example",
     "people_value": [{"gid": "1201234567890061", "resource_type": "user",
                       "name": "Ada Example"}]},
    {"gid": "1201234567890007", "resource_type": "custom_field", "name": "Related",
     "resource_subtype": "reference", "display_value": "Acme launch",
     "reference_value": [{"gid": "1201234567890071", "resource_type": "task",
                          "name": "Acme launch"}]},
    {"gid": "1201234567890008", "resource_type": "custom_field", "name": "Score",
     "resource_subtype": "formula_rating", "display_value": "4 stars",
     "text_value": "4"},
    {"gid": "1201234567890009", "resource_type": "custom_field", "name": "Due",
     "resource_subtype": "date", "type": "date", "display_value": null,
     "date_value": null},
    {"gid": "1201234567890010", "resource_type": "custom_field", "name": "Owner",
     "resource_subtype": "enum", "type": "enum", "display_value": null, "enum_value": null},
    {"gid": "1201234567890011", "resource_type": "custom_field", "name": "Day only",
     "resource_subtype": "date", "display_value": "2026-11-30",
     "date_value": {"date": "2026-11-30", "date_time": null}},
    {"gid": "1201234567890012", "resource_type": "custom_field", "name": "Legacy",
     "type": "number", "number_value": 7}
  ]
}"#;

fn task() -> Task {
    serde_json::from_str(TASK).unwrap()
}

fn gid(text: &str) -> CustomFieldGid {
    text.parse().unwrap()
}

fn value(gid_text: &str) -> CustomFieldValue {
    task().custom_field(&gid(gid_text)).unwrap().value
}

#[test]
fn text_and_number_read_their_values_and_the_number_keeps_its_digits() {
    assert_eq!(
        value("1201234567890001").as_text(),
        Some("Call before Friday")
    );
    let fee = value("1201234567890002").as_number().unwrap();
    assert_eq!(fee.to_string(), "1250.50");
    assert_eq!(fee, Decimal::new(125_050, 2));
}

#[test]
fn an_enum_reads_its_option_with_name_colour_and_state() {
    let stage = value("1201234567890003");
    let option = stage.as_enum_option().unwrap();
    assert_eq!(option.gid.to_string(), "1201234567890031");
    assert_eq!(option.name.as_deref(), Some("Approved"));
    assert_eq!(option.color.as_deref(), Some("green"));
    assert_eq!(option.enabled, Some(true));
}

#[test]
fn a_multi_enum_reads_every_option_in_order() {
    let channels = value("1201234567890004");
    let names: Vec<_> = channels
        .as_multi_enum()
        .unwrap()
        .iter()
        .map(|option| (option.gid.to_string(), option.enabled))
        .collect();
    assert_eq!(
        names,
        [
            ("1201234567890041".to_owned(), Some(true)),
            ("1201234567890042".to_owned(), Some(false))
        ]
    );
}

#[test]
fn a_date_reads_its_day_and_time_and_a_day_alone() {
    let signed = value("1201234567890005");
    let signed = signed.as_date().unwrap();
    assert_eq!(signed.date, date!(2026 - 12 - 17));
    assert_eq!(signed.date_time, Some(datetime!(2026-12-17 02:30 UTC)));

    let day = value("1201234567890011");
    assert_eq!(day.as_date().unwrap().date, date!(2026 - 11 - 30));
    assert_eq!(day.as_date().unwrap().date_time, None);
}

#[test]
fn people_and_reference_read_their_objects() {
    let reviewers = value("1201234567890006");
    let reviewer = &reviewers.as_people().unwrap()[0];
    assert_eq!(reviewer.gid.to_string(), "1201234567890061");
    assert_eq!(reviewer.name.as_deref(), Some("Ada Example"));

    let CustomFieldValue::Reference(related) = value("1201234567890007") else {
        panic!("a reference value");
    };
    assert_eq!(related[0].gid, "1201234567890071");
    assert_eq!(related[0].resource_type.as_deref(), Some("task"));
}

#[test]
fn an_unknown_kind_keeps_its_subtype_and_value_members() {
    let field = task().custom_field_named("Score").unwrap();
    assert_eq!(field.display_value.as_deref(), Some("4 stars"));
    let CustomFieldValue::Unknown {
        resource_subtype,
        raw,
    } = field.value
    else {
        panic!("an unknown kind");
    };
    assert_eq!(resource_subtype, "formula_rating");
    assert_eq!(
        raw,
        serde_json::json!({"display_value": "4 stars", "text_value": "4"})
    );
}

#[test]
fn an_unset_field_is_empty_whatever_its_kind() {
    assert_eq!(value("1201234567890009"), CustomFieldValue::Empty);
    assert_eq!(value("1201234567890010"), CustomFieldValue::Empty);
    assert!(value("1201234567890010").is_empty());
}

#[test]
fn the_deprecated_type_is_read_when_resource_subtype_is_absent() {
    assert_eq!(
        value("1201234567890012"),
        CustomFieldValue::number(Decimal::from(7))
    );
}

#[test]
fn fields_are_found_by_gid_or_name_and_listed_in_order() {
    let task = task();
    let all = task.custom_fields_typed();
    assert_eq!(all.len(), 12);
    assert_eq!(all[0].name.as_deref(), Some("Notes"));
    assert_eq!(
        task.custom_field_named("Stage").unwrap().gid,
        gid("1201234567890003")
    );
    assert!(task.custom_field_named("stage").is_none());
    assert!(task.custom_field(&gid("1209999999999999")).is_none());
}

#[test]
fn custom_fields_not_selected_are_absent_not_an_error() {
    let task: Task =
        serde_json::from_str(r#"{"gid": "1201234567890123", "name": "Acme"}"#).unwrap();
    assert!(task.custom_field(&gid("1201234567890001")).is_none());
    assert!(task.custom_field_named("Notes").is_none());
    assert!(task.custom_fields_typed().is_empty());
}
