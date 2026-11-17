//! Decodes realistic response bodies, shaped like the examples in Asana's specification, into the
//! generated models, and checks what requests serialise to. Every GID, name and address is
//! invented.

use std::collections::BTreeMap;
use std::str::FromStr;

use asana_openapi::ids::{CustomFieldGid, Gid, TaskGid, UserRef};
use asana_openapi::models::{
    Clearable, CustomField, CustomFieldResourceSubtype, Membership, PortfolioItem, Project,
    ProjectColor, RateOrPlaceholder, Section, Story, Task, TaskCreateRequest, TaskResourceSubtype,
    TaskUpdateRequest, User, Webhook,
};
use rust_decimal::Decimal;
use serde_json::json;
use time::macros::{date, datetime};

fn decode<T: serde::de::DeserializeOwned>(value: serde_json::Value) -> T {
    match serde_json::from_value(value) {
        Ok(decoded) => decoded,
        Err(error) => panic!("the body does not decode: {error}"),
    }
}

/// One custom field value of each type, as a task's `custom_fields` holds them.
fn custom_fields_of_each_type() -> serde_json::Value {
    json!([
        {"gid": "1201234567890401", "resource_type": "custom_field", "name": "Notes",
         "resource_subtype": "text", "type": "text", "text_value": "Some text",
         "display_value": "Some text"},
        {"gid": "1201234567890402", "resource_type": "custom_field", "name": "Hours",
         "resource_subtype": "number", "type": "number", "precision": 2,
         "number_value": 5.25, "display_value": "5.25"},
        {"gid": "1201234567890403", "resource_type": "custom_field", "name": "Priority",
         "resource_subtype": "enum", "type": "enum",
         "enum_value": {"gid": "1201234567890501", "resource_type": "enum_option",
                        "name": "Low", "enabled": true, "color": "blue"},
         "display_value": "Low"},
        {"gid": "1201234567890404", "resource_type": "custom_field", "name": "Tags",
         "resource_subtype": "multi_enum", "type": "multi_enum",
         "multi_enum_values": [
             {"gid": "1201234567890502", "resource_type": "enum_option", "name": "A"},
             {"gid": "1201234567890503", "resource_type": "enum_option", "name": "B"}
         ],
         "display_value": "A, B"},
        {"gid": "1201234567890405", "resource_type": "custom_field", "name": "Review",
         "resource_subtype": "date", "type": "date",
         "date_value": {"date": "2024-08-23", "date_time": "2024-08-23T22:00:00.000Z"},
         "display_value": "2024-08-23T22:00:00.000Z"},
        {"gid": "1201234567890406", "resource_type": "custom_field", "name": "Reviewers",
         "resource_subtype": "people", "type": "people",
         "people_value": [{"gid": "1201234567890001", "resource_type": "user", "name": "Ada Example"}],
         "display_value": "Ada Example"},
        {"gid": "1201234567890407", "resource_type": "custom_field", "name": "Empty",
         "resource_subtype": "number", "type": "number", "number_value": null,
         "display_value": null}
    ])
}

#[test]
fn a_full_task_with_memberships_custom_fields_and_nulls() {
    let custom_fields = custom_fields_of_each_type();
    let task: Task = decode(json!({
        "gid": "1201234567890123",
        "resource_type": "task",
        "name": "Send the Acme invoice",
        "resource_subtype": "default_task",
        "approval_status": "pending",
        "assignee": {"gid": "1201234567890001", "resource_type": "user", "name": "Ada Example"},
        "assignee_status": "upcoming",
        "assignee_section": null,
        "completed": false,
        "completed_at": null,
        "completed_by": null,
        "created_at": "2012-02-22T02:06:58.147Z",
        "created_by": {"gid": "1201234567890001", "resource_type": "user"},
        "due_at": null,
        "due_on": "2019-09-15",
        "start_on": null,
        "modified_at": "2012-02-22T02:06:58.147Z",
        "notes": "Mittens really likes the stuff from Humboldt.",
        "num_likes": 5,
        "actual_time_minutes": 200.5,
        "parent": null,
        "external": {"gid": "my_gid", "data": "A blob of information"},
        "followers": [{"gid": "1201234567890001", "resource_type": "user", "name": "Ada Example"}],
        "memberships": [{
            "project": {"gid": "1201234567890200", "resource_type": "project", "name": "Stuff to buy"},
            "section": {"gid": "1201234567890300", "resource_type": "section", "name": "Next Actions"}
        }, {
            "project": {"gid": "1201234567890201", "resource_type": "project", "name": "Errands"},
            "section": null
        }],
        "custom_fields": custom_fields,
        "a_field_asana_adds_later": {"anything": [1, 2, 3]}
    }));

    assert_eq!(task.gid, Some(TaskGid::parse("1201234567890123").unwrap()));
    assert_eq!(
        task.resource_subtype,
        Some(TaskResourceSubtype::DefaultTask)
    );
    assert_eq!(task.due_on, Some(date!(2019 - 09 - 15)));
    assert_eq!(
        task.created_at,
        Some(datetime!(2012-02-22 02:06:58.147 UTC))
    );
    assert_eq!(task.start_on, None);
    assert_eq!(task.assignee_section, None);
    assert_eq!(task.parent, None);
    assert_eq!(
        task.actual_time_minutes,
        Some(Decimal::from_str("200.5").unwrap())
    );
    assert_eq!(
        task.assignee.as_ref().and_then(|user| user.name.as_deref()),
        Some("Ada Example")
    );
    assert_eq!(
        task.external
            .as_ref()
            .and_then(|external| external.gid.as_deref()),
        Some("my_gid"),
        "an external GID is the caller's own text, not an Asana GID"
    );

    let memberships = task.memberships.unwrap();
    assert_eq!(memberships.len(), 2);
    let section: &Section = memberships[0].section.as_ref().unwrap();
    assert_eq!(section.name.as_deref(), Some("Next Actions"));
    assert!(memberships[1].section.is_none());

    let fields: Vec<CustomField> = task.custom_fields.unwrap();
    assert_eq!(fields[0].text_value.as_deref(), Some("Some text"));
    assert_eq!(
        fields[1].number_value,
        Some(Decimal::from_str("5.25").unwrap())
    );
    assert_eq!(
        fields[1].resource_subtype,
        Some(CustomFieldResourceSubtype::Number)
    );
    assert_eq!(
        fields[2]
            .enum_value
            .as_ref()
            .and_then(|option| option.name.as_deref()),
        Some("Low")
    );
    assert_eq!(fields[3].multi_enum_values.as_ref().map(Vec::len), Some(2));
    let date_value = fields[4].date_value.as_ref().unwrap();
    assert_eq!(date_value.date, Some(date!(2024 - 08 - 23)));
    assert_eq!(date_value.date_time, Some(datetime!(2024-08-23 22:00 UTC)));
    assert_eq!(fields[5].people_value.as_ref().map(Vec::len), Some(1));
    assert_eq!(fields[6].number_value, None);
    assert_eq!(fields[6].display_value, None);
}

#[test]
fn a_number_keeps_every_digit() {
    // From the bytes, as a response arrives: a `json!` literal would pass through `f64` first.
    let body = r#"{"number_value": 1234567890.123456789, "precision": 9}"#;
    let field: CustomField = serde_json::from_str(body).unwrap();
    assert_eq!(
        field.number_value,
        Some(Decimal::from_str("1234567890.123456789").unwrap())
    );
    // Inside a union, which decodes through `serde_json::Value` first, too.
    let body = r#"{"resource_type": "rate", "rate": 12345678901234.56789,
                   "resource": {"resource_type": "user"}}"#;
    let RateOrPlaceholder::Rate(rate) = serde_json::from_str(body).unwrap() else {
        panic!("a user's rate");
    };
    assert_eq!(
        rate.rate,
        Some(Decimal::from_str("12345678901234.56789").unwrap())
    );
}

#[test]
fn a_compact_task() {
    let task: Task = decode(json!({
        "gid": "12345",
        "resource_type": "task",
        "name": "Bug Task",
        "resource_subtype": "milestone"
    }));
    assert_eq!(task.name.as_deref(), Some("Bug Task"));
    assert_eq!(task.resource_subtype, Some(TaskResourceSubtype::Milestone));
    assert_eq!(task.notes, None);
}

#[test]
fn a_project() {
    let project: Project = decode(json!({
        "gid": "1201234567890200",
        "resource_type": "project",
        "name": "Stuff to buy",
        "archived": false,
        "color": "light-green",
        "created_at": "2012-02-22T02:06:58.147Z",
        "current_status_update": {"gid": "12345", "resource_type": "status_update",
                                  "title": "Status Update - Jun 15", "resource_subtype": "project_status_update"},
        "default_view": "calendar",
        "due_on": "2019-09-15",
        "start_on": null,
        "members": [{"gid": "1201234567890001", "resource_type": "user", "name": "Ada Example"}],
        "owner": {"gid": "1201234567890001", "resource_type": "user", "name": "Ada Example"},
        "team": {"gid": "1201234567890700", "resource_type": "team", "name": "Marketing"},
        "workspace": {"gid": "1201234567890800", "resource_type": "workspace", "name": "Acme"},
        "permalink_url": "https://app.asana.com/0/resource/1201234567890200/list",
        "privacy_setting": "public_to_workspace"
    }));
    assert_eq!(project.color, Some(ProjectColor::LightGreen));
    assert_eq!(project.due_on, Some(date!(2019 - 09 - 15)));
    assert_eq!(
        project
            .current_status_update
            .as_ref()
            .and_then(|update| update.title.as_deref()),
        Some("Status Update - Jun 15")
    );
    assert_eq!(
        project.workspace.and_then(|workspace| workspace.name),
        Some("Acme".to_owned())
    );
}

#[test]
fn a_section() {
    let section: Section = decode(json!({
        "gid": "1201234567890300",
        "resource_type": "section",
        "name": "Next Actions",
        "created_at": "2012-02-22T02:06:58.147Z",
        "project": {"gid": "1201234567890200", "resource_type": "project", "name": "Stuff to buy"},
        "projects": [{"gid": "1201234567890200", "resource_type": "project", "name": "Stuff to buy"}]
    }));
    assert_eq!(section.name.as_deref(), Some("Next Actions"));
    assert_eq!(section.projects.map(|projects| projects.len()), Some(1));
}

#[test]
fn a_story_about_a_section_move_and_a_number_change() {
    let story: Story = decode(json!({
        "gid": "1201234567890900",
        "resource_type": "story",
        "created_at": "2012-02-22T02:06:58.147Z",
        "created_by": {"gid": "1201234567890001", "resource_type": "user", "name": "Ada Example"},
        "resource_subtype": "section_changed",
        "type": "system",
        "text": "moved this Task from \"To Do\" to \"Done\"",
        "is_pinned": false,
        "source": "web",
        "old_section": {"gid": "1201234567890300", "resource_type": "section", "name": "To Do"},
        "new_section": {"gid": "1201234567890301", "resource_type": "section", "name": "Done"},
        "old_number_value": 1,
        "new_number_value": 2.5,
        "target": {"gid": "1201234567890123", "resource_type": "task", "name": "Bug Task"}
    }));
    assert_eq!(story.resource_subtype.as_deref(), Some("section_changed"));
    assert_eq!(
        story.new_section.and_then(|section| section.name),
        Some("Done".to_owned())
    );
    assert_eq!(
        story.new_number_value,
        Some(Decimal::from_str("2.5").unwrap())
    );
    assert!(story.target.is_some());
}

#[test]
fn a_user() {
    let user: User = decode(json!({
        "gid": "1201234567890001",
        "resource_type": "user",
        "name": "Ada Example",
        "email": "ada@example.com",
        "photo": {
            "image_21x21": "https://example.com/21.png",
            "image_128x128": "https://example.com/128.png"
        },
        "workspaces": [{"gid": "1201234567890800", "resource_type": "workspace", "name": "Acme"}]
    }));
    assert_eq!(user.email.as_deref(), Some("ada@example.com"));
    assert_eq!(user.workspaces.map(|workspaces| workspaces.len()), Some(1));
    assert!(user.photo.is_some());
}

#[test]
fn a_webhook() {
    let webhook: Webhook = decode(json!({
        "gid": "1201234567891000",
        "resource_type": "webhook",
        "active": false,
        "resource": {"gid": "1201234567890200", "resource_type": "project", "name": "Stuff to buy"},
        "target": "https://example.com/receive-webhook/7654",
        "created_at": "2012-02-22T02:06:58.147Z",
        "filters": [{"resource_type": "task", "resource_subtype": "milestone",
                     "action": "changed", "fields": ["due_at", "due_on", "dependencies"]}],
        "last_failure_at": "2012-02-22T02:06:58.147Z",
        "last_failure_content": "500 Server Error\\n\\nCould not complete the request",
        "last_success_at": "2012-02-22T02:06:58.147Z",
        "delivery_retry_count": 3,
        "failure_deletion_timestamp": null,
        "next_attempt_after": null
    }));
    assert_eq!(webhook.active, Some(false));
    let filters = webhook.filters.unwrap();
    assert_eq!(filters[0].fields.as_ref().map(Vec::len), Some(3));
    assert_eq!(webhook.delivery_retry_count, Some(3));
    assert_eq!(webhook.next_attempt_after, None);
}

#[test]
fn a_custom_field_definition_with_enum_options() {
    let field: CustomField = decode(json!({
        "gid": "1201234567890403",
        "resource_type": "custom_field",
        "name": "Priority",
        "resource_subtype": "enum",
        "type": "enum",
        "representation_type": "enum",
        "enabled": true,
        "description": "Development team priority",
        "precision": 2,
        "format": "custom",
        "custom_label": "gold pieces",
        "custom_label_position": "suffix",
        "is_global_to_workspace": true,
        "has_notifications_enabled": true,
        "enum_options": [
            {"gid": "1201234567890501", "resource_type": "enum_option", "name": "Low",
             "enabled": true, "color": "blue"},
            {"gid": "1201234567890502", "resource_type": "enum_option", "name": "High",
             "enabled": false, "color": "red"}
        ]
    }));
    let options = field.enum_options.unwrap();
    assert_eq!(options.len(), 2);
    assert_eq!(options[1].enabled, Some(false));
    assert_eq!(
        options[0].gid.as_ref().map(Gid::as_str),
        Some("1201234567890501")
    );
}

#[test]
fn an_unknown_enum_value_is_kept() {
    let task: Task = decode(json!({"resource_subtype": "a_subtype_from_next_year"}));
    let subtype = task.resource_subtype.unwrap();
    assert_eq!(
        subtype,
        TaskResourceSubtype::Unknown("a_subtype_from_next_year".to_owned())
    );
    assert_eq!(subtype.as_str(), "a_subtype_from_next_year");
    assert_eq!(subtype.to_string(), "a_subtype_from_next_year");
    assert_eq!(
        serde_json::to_value(&subtype).unwrap(),
        json!("a_subtype_from_next_year")
    );
}

#[test]
fn unions_pick_their_member_and_keep_unknown_ones() {
    let membership: Membership = decode(json!({
        "gid": "1201234567891100",
        "resource_type": "membership",
        "resource_subtype": "project_membership",
        "access_level": "editor",
        "parent": {"gid": "1201234567890200", "resource_type": "project", "name": "Stuff to buy"},
        "member": {"gid": "1201234567890001", "resource_type": "user", "name": "Ada Example"}
    }));
    let Membership::Project(project_membership) = membership else {
        panic!("a project membership: {membership:?}");
    };
    assert_eq!(
        project_membership.parent.and_then(|project| project.name),
        Some("Stuff to buy".to_owned())
    );

    let unknown: Membership = decode(json!({
        "gid": "1201234567891101",
        "resource_type": "membership",
        "resource_subtype": "space_station_membership",
        "access_level": "astronaut"
    }));
    let Membership::Other(raw) = unknown else {
        panic!("an unknown membership is kept: {unknown:?}");
    };
    assert_eq!(raw["access_level"], "astronaut");

    let item: PortfolioItem = decode(json!({
        "gid": "1201234567890200", "resource_type": "project", "name": "Stuff to buy"
    }));
    assert!(matches!(item, PortfolioItem::Project(_)));

    let rate: RateOrPlaceholder = decode(json!({
        "gid": "1201234567891200", "resource_type": "rate", "rate": 100.5,
        "resource": {"gid": "1201234567891300", "resource_type": "placeholder", "name": "Designer"}
    }));
    let RateOrPlaceholder::PlaceholderRate(rate) = rate else {
        panic!("a placeholder's rate: {rate:?}");
    };
    assert_eq!(rate.rate, Some(Decimal::from_str("100.5").unwrap()));
}

#[test]
fn a_request_leaves_out_what_was_not_set_and_sends_null_to_clear() {
    let custom_field = CustomFieldGid::parse("1201234567890402").unwrap();
    let mut custom_fields = BTreeMap::new();
    custom_fields.insert(custom_field, json!(5.25));
    let create = TaskCreateRequest::builder()
        .name("Send the Acme invoice")
        .due_on(date!(2026 - 11 - 30))
        .assignee(UserRef::Me)
        .custom_fields(custom_fields)
        .build();
    assert_eq!(
        serde_json::to_value(&create).unwrap(),
        json!({
            "name": "Send the Acme invoice",
            "due_on": "2026-11-30",
            "assignee": "me",
            "custom_fields": {"1201234567890402": 5.25}
        })
    );

    let clear = TaskUpdateRequest::builder()
        .due_on(Clearable::Clear)
        .due_at(Clearable::Clear)
        .start_at(datetime!(2026-11-30 09:00 UTC))
        .build();
    assert_eq!(
        serde_json::to_value(&clear).unwrap(),
        json!({"due_on": null, "due_at": null, "start_at": "2026-11-30T09:00:00Z"})
    );
}
