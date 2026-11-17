//! Tests of the models half against small specifications and the real one.

use std::sync::OnceLock;

use serde_json::{Value, json};

use super::ir::{ItemKind, Ty};
use super::{Analysis, Direction, rust_type};
use crate::naming::{SchemaRole, role_of};
use crate::spec::Spec;

/// The real specification with its overlays, loaded once for every test that reads it.
fn real() -> &'static Spec {
    static SPEC: OnceLock<Spec> = OnceLock::new();
    SPEC.get_or_init(|| {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
        crate::spec::load(&root).unwrap()
    })
}

fn analysis() -> &'static Analysis {
    real().analysis().unwrap()
}

fn fields(name: &str) -> Vec<(String, Ty, bool)> {
    match &analysis().items[name].kind {
        ItemKind::Struct { fields, .. } => fields
            .iter()
            .map(|field| (field.json.clone(), field.ty.clone(), field.clearable))
            .collect(),
        other => panic!("{name} is not a struct: {other:?}"),
    }
}

fn field(name: &str, json: &str) -> (Ty, bool) {
    fields(name)
        .into_iter()
        .find(|(key, _, _)| key == json)
        .map(|(_, ty, clearable)| (ty, clearable))
        .unwrap_or_else(|| panic!("{name} has no field {json}"))
}

fn named(name: &str) -> Ty {
    Ty::Named(name.to_owned())
}

#[test]
fn the_suffix_rule_and_its_overrides_name_families() {
    assert_eq!(
        role_of("TaskCompact"),
        SchemaRole::Read {
            family: "Task".into()
        }
    );
    assert_eq!(
        role_of("UserBaseResponse"),
        SchemaRole::Read {
            family: "User".into()
        }
    );
    assert_eq!(
        role_of("ProjectMembershipNormalResponse"),
        SchemaRole::Read {
            family: "ProjectMembership".into()
        }
    );
    assert_eq!(
        role_of("TaskRequestBase"),
        SchemaRole::Write {
            type_name: "TaskRequestBase".into()
        }
    );
    assert_eq!(
        role_of("EnumOption"),
        SchemaRole::Read {
            family: "EnumOption".into()
        }
    );
}

#[test]
fn a_family_is_the_union_of_its_shapes() {
    let task = fields("Task");
    let keys: Vec<_> = task.iter().map(|(key, _, _)| key.as_str()).collect();
    assert_eq!(&keys[..2], ["gid", "resource_type"]);
    for key in [
        "name",
        "due_on",
        "modified_at",
        "memberships",
        "custom_fields",
        "permalink_url",
    ] {
        assert!(keys.contains(&key), "Task lacks {key}");
    }
    assert_eq!(field("Task", "gid").0, Ty::Gid("Task".into()));
    assert_eq!(field("Task", "due_on").0, Ty::Date);
    assert_eq!(field("Task", "modified_at").0, Ty::DateTime);
    assert_eq!(field("Task", "actual_time_minutes").0, Ty::Decimal);
    assert_eq!(field("Task", "num_likes").0, Ty::Integer);
    // An inline `{gid, resource_type: user}` is a compact user.
    assert_eq!(field("Task", "created_by").0, named("User"));
    assert_eq!(field("Task", "assignee").0, named("User"));
    assert_eq!(
        field("Task", "memberships").0,
        Ty::Vec(Box::new(named("TaskMembership")))
    );
    // An extension of a family's compact shape adds the property the family lacked.
    assert_eq!(field("Goal", "metric").0, named("GoalMetric"));
    assert!(
        fields("GoalMetric")
            .iter()
            .any(|(key, _, _)| key == "can_manage")
    );
}

#[test]
fn requests_leave_out_read_only_fields_and_can_clear_nullable_ones() {
    let create = fields("TaskCreateRequest");
    let keys: Vec<_> = create.iter().map(|(key, _, _)| key.as_str()).collect();
    assert!(!keys.contains(&"gid") && !keys.contains(&"created_at"));
    assert!(
        !keys.contains(&"assigned_by"),
        "readOnly inside allOf is honoured"
    );
    assert_eq!(field("TaskCreateRequest", "due_on"), (Ty::Date, true));
    assert_eq!(field("TaskCreateRequest", "name"), (Ty::String, false));
    assert_eq!(field("TaskCreateRequest", "assignee"), (Ty::UserRef, true));
    assert_eq!(
        field("TaskCreateRequest", "custom_fields").0,
        Ty::Map(Box::new(Ty::Gid("CustomField".into())), Box::new(Ty::Json))
    );
    assert_eq!(
        field("AddFollowersRequest", "followers").0,
        Ty::Vec(Box::new(Ty::UserRef))
    );
    assert_eq!(
        field("TaskUpdateRequest", "resource_subtype").0,
        named("TaskResourceSubtype"),
        "a request shares its family's enum"
    );
}

#[test]
fn typed_gids_cover_the_families_other_crates_name() {
    let gids = &analysis().classes.gid_families;
    for (family, resource_type) in [
        ("Task", "task"),
        ("CustomField", "custom_field"),
        ("EnumOption", "enum_option"),
        ("Placeholder", "placeholder"),
        ("Webhook", "webhook"),
    ] {
        assert_eq!(gids.get(family).map(String::as_str), Some(resource_type));
    }
    assert!(!gids.contains_key("Member"), "a member is a user or a team");
    assert_eq!(
        analysis().classes.gid_type("PlaceholderRate").as_deref(),
        Some("Rate")
    );
    assert_eq!(field("PlaceholderRate", "gid").0, Ty::Gid("Rate".into()));
}

#[test]
fn unions_are_told_apart_by_the_field_that_differs() {
    let union = |name: &str| match &analysis().items[name].kind {
        ItemKind::Union {
            discriminator,
            variants,
        } => (
            discriminator.clone(),
            variants
                .iter()
                .map(|variant| (variant.name.clone(), variant.tag.clone()))
                .collect::<Vec<_>>(),
        ),
        other => panic!("{name} is not a union: {other:?}"),
    };
    let (pointer, variants) = union("Membership");
    assert_eq!(pointer, "/resource_subtype");
    assert!(variants.contains(&("Goal".to_owned(), "goal_membership".to_owned())));
    assert!(variants.contains(&("Project".to_owned(), "project_membership".to_owned())));
    assert_eq!(union("PortfolioItem").0, "/resource_type");
    assert_eq!(union("RateOrPlaceholder").0, "/resource/resource_type");
}

#[test]
fn rust_type_names_references_and_builds_inline_objects() {
    let spec = real();
    let reference: openapiv3::ReferenceOr<openapiv3::Schema> =
        serde_json::from_value(json!({"$ref": "#/components/schemas/TaskResponse"})).unwrap();
    let task = rust_type(spec, &reference, "Unused", Direction::Read).unwrap();
    assert_eq!(task.ty.to_string(), "crate :: models :: Task");
    assert!(task.items.is_empty());

    let list: openapiv3::ReferenceOr<openapiv3::Schema> = serde_json::from_value(json!({
        "type": "array", "items": {"$ref": "#/components/schemas/StoryCompact"}
    }))
    .unwrap();
    let stories = rust_type(spec, &list, "Unused", Direction::Read).unwrap();
    assert_eq!(stories.ty.to_string(), "Vec < crate :: models :: Story >");

    let body: openapiv3::ReferenceOr<openapiv3::Schema> =
        serde_json::from_value(json!({"$ref": "#/components/schemas/TaskCreateRequest"})).unwrap();
    let create = rust_type(spec, &body, "Unused", Direction::Write).unwrap();
    assert_eq!(
        create.ty.to_string(),
        "crate :: models :: TaskCreateRequest"
    );

    let inline: openapiv3::ReferenceOr<openapiv3::Schema> = serde_json::from_value(json!({
        "type": "object",
        "properties": {
            "project": {"type": "string"},
            "section": {"$ref": "#/components/schemas/SectionCompact"},
            "when": {"type": "string", "format": "date"},
            "mode": {"type": "string", "enum": ["a", "b"]}
        }
    }))
    .unwrap();
    let made = rust_type(spec, &inline, "DoThingData", Direction::Read).unwrap();
    assert_eq!(made.ty.to_string(), "DoThingData");
    let text: String = made.items.iter().map(ToString::to_string).collect();
    assert!(text.contains("pub struct DoThingData"));
    assert!(text.contains("pub enum DoThingDataMode"));
    assert!(text.contains("Option < DoThingDataMode >"));
    assert!(text.contains("crate :: models :: Section"));
}

#[test]
fn a_small_specification_builds_without_the_real_one() {
    let raw: Value = json!({
        "openapi": "3.0.0",
        "info": {"title": "t", "version": "1"},
        "paths": {},
        "components": {"schemas": {
            "TaskCompact": {"type": "object", "properties": {
                "gid": {"type": "string", "readOnly": true},
                "resource_type": {"type": "string", "example": "task"}
            }},
            "TaskResponse": {"allOf": [
                {"$ref": "#/components/schemas/TaskCompact"},
                {"type": "object", "properties": {
                    "parent": {"allOf": [{"$ref": "#/components/schemas/TaskCompact"}, {"nullable": true}]}
                }}
            ]}
        }}
    });
    let classes = super::classify::Classes::build(&raw);
    // The real crate needs its well-known GID families; a toy specification lacks them, and
    // generation says so instead of quietly dropping types other crates name.
    assert!(classes.unwrap_err().contains("no longer"));
}
