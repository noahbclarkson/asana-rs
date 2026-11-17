//! Writing every kind of custom field value as Asana's `custom_fields` request map takes it.

use asana_client::openapi::ids::{CustomFieldGid, EnumOptionGid, TaskGid, UserGid};
use asana_client::openapi::models::TaskUpdateRequest;
use asana_client::{CustomFieldValue, ResourceRef, custom_fields_map};
use rust_decimal::Decimal;
use time::macros::{date, datetime};

fn field(last: u32) -> CustomFieldGid {
    format!("12012345678900{last:02}").parse().unwrap()
}

fn option(text: &str) -> EnumOptionGid {
    text.parse().unwrap()
}

fn user(text: &str) -> UserGid {
    text.parse().unwrap()
}

#[test]
fn every_kind_is_written_as_asana_takes_it() {
    let related: TaskGid = "1201234567890071".parse().unwrap();
    let map = custom_fields_map([
        (field(1), CustomFieldValue::text("Call before Friday")),
        (field(2), CustomFieldValue::number(Decimal::new(125_050, 2))),
        (
            field(3),
            CustomFieldValue::enum_option(option("1201234567890031")),
        ),
        (
            field(4),
            CustomFieldValue::multi_enum([option("1201234567890041"), option("1201234567890042")]),
        ),
        (field(5), CustomFieldValue::date(date!(2026 - 12 - 17))),
        (
            field(6),
            CustomFieldValue::date_time(datetime!(2026-12-17 15:30:00.25 +13:00)),
        ),
        (
            field(7),
            CustomFieldValue::people([user("1201234567890061")]),
        ),
        (
            field(8),
            CustomFieldValue::reference([ResourceRef::new(&related)]),
        ),
        (field(9), CustomFieldValue::clear()),
        (
            field(10),
            CustomFieldValue::multi_enum(Vec::<EnumOptionGid>::new()),
        ),
        (
            field(11),
            CustomFieldValue::raw(serde_json::json!({"rating": 4})),
        ),
    ]);
    insta::assert_snapshot!(serde_json::to_string_pretty(&map).unwrap());
}

#[test]
fn a_number_keeps_every_digit_it_was_given() {
    let cases = [
        (Decimal::new(125_050, 2), "1250.50"),
        (Decimal::new(-5, 1), "-0.5"),
        (Decimal::new(1, 28), "0.0000000000000000000000000001"),
        (
            "79228162514264337593543950335".parse().unwrap(),
            "79228162514264337593543950335",
        ),
    ];
    for (number, written) in cases {
        let json = CustomFieldValue::number(number).to_request_json();
        assert_eq!(serde_json::to_string(&json).unwrap(), written);
        assert!(json.is_number(), "{written} is sent as a JSON number");
    }
}

#[test]
fn a_value_read_from_a_task_writes_back_as_its_gid() {
    let task: asana_client::openapi::models::Task = serde_json::from_str(
        r#"{"custom_fields": [{"gid": "1201234567890003", "resource_subtype": "enum",
            "enum_value": {"gid": "1201234567890031", "name": "Approved", "color": "green"}}]}"#,
    )
    .unwrap();
    let read = asana_client::TaskCustomFields::custom_field(&task, &field(3)).unwrap();
    assert_eq!(
        read.value.to_request_json(),
        serde_json::json!("1201234567890031")
    );
}

#[test]
fn a_later_value_for_the_same_field_wins() {
    let map = custom_fields_map([
        (field(1), CustomFieldValue::text("first")),
        (field(1), CustomFieldValue::text("second")),
    ]);
    assert_eq!(map.len(), 1);
    assert_eq!(map[&field(1)], serde_json::json!("second"));
}

#[test]
fn the_map_sits_in_a_task_update_body() {
    let body = TaskUpdateRequest::builder()
        .custom_fields(custom_fields_map([
            (field(2), CustomFieldValue::number(Decimal::new(1999, 2))),
            (field(9), CustomFieldValue::clear()),
        ]))
        .build();
    assert_eq!(
        serde_json::to_string(&body).unwrap(),
        r#"{"custom_fields":{"1201234567890002":19.99,"1201234567890009":null}}"#
    );
}
