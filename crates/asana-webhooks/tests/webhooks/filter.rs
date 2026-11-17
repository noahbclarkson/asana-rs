//! Webhook filters: their JSON and Asana's rules.

use asana_webhooks::{EventAction, Filter, FilterError, ResourceType, validate_filters};

#[test]
fn filters_serialise_to_asanas_shape() {
    let filters = vec![
        Filter::task()
            .subtype("milestone")
            .action(EventAction::Changed)
            .fields(["due_at", "due_on"]),
        Filter::new(ResourceType::Project).action(EventAction::Added),
        Filter::any_resource().action(EventAction::Deleted),
    ];
    let json = serde_json::to_value(&filters).unwrap();
    assert_eq!(
        json,
        serde_json::json!([
            {"resource_type": "task", "resource_subtype": "milestone", "action": "changed",
             "fields": ["due_at", "due_on"]},
            {"resource_type": "project", "action": "added"},
            {"action": "deleted"}
        ])
    );
    // And back, as Asana returns them on a webhook.
    let read: Vec<Filter> = serde_json::from_value(json).unwrap();
    assert_eq!(read, filters);
    let unknown: Filter =
        serde_json::from_str(r#"{"resource_type":"allocation","action":"archived"}"#).unwrap();
    assert_eq!(
        unknown.resource_type(),
        Some(&ResourceType::Other("allocation".to_owned()))
    );
    assert_eq!(
        unknown.action_filter(),
        Some(&EventAction::Unknown("archived".to_owned()))
    );
}

#[test]
fn fields_need_the_changed_action_everywhere() {
    for filter in [
        Filter::task().field("name"),
        Filter::task().action(EventAction::Added).field("name"),
    ] {
        assert_eq!(
            validate_filters(&ResourceType::Project, &[filter]),
            Err(FilterError::FieldsNeedChanged)
        );
    }
    let ok = Filter::task().action(EventAction::Changed).field("name");
    assert_eq!(ok.field_list(), ["name"]);
    assert_eq!(validate_filters(&ResourceType::Project, &[ok]), Ok(()));
}

#[test]
fn lower_level_webhooks_need_no_filter() {
    for target in [
        ResourceType::Project,
        ResourceType::Task,
        ResourceType::Section,
    ] {
        assert_eq!(validate_filters(&target, &[]), Ok(()));
        assert_eq!(validate_filters(&target, &[Filter::story()]), Ok(()));
    }
}

#[test]
fn higher_level_webhooks_need_filters_from_the_documented_table() {
    for target in [
        ResourceType::Workspace,
        ResourceType::Team,
        ResourceType::Portfolio,
        ResourceType::Goal,
    ] {
        assert_eq!(
            validate_filters(&target, &[]),
            Err(FilterError::FiltersRequired(target.clone()))
        );
        assert_eq!(
            validate_filters(&target, &[Filter::task().action(EventAction::Added)]),
            Err(FilterError::NeverPropagated(ResourceType::Task))
        );
        assert_eq!(
            validate_filters(&target, &[Filter::story()]),
            Err(FilterError::NeverPropagated(ResourceType::Story))
        );
        assert_eq!(
            validate_filters(
                &target,
                &[Filter::any_resource().action(EventAction::Added)]
            ),
            Err(FilterError::ResourceTypeRequired)
        );
        assert_eq!(
            validate_filters(
                &target,
                &[Filter::project().action(EventAction::Changed).field("name")]
            ),
            Err(FilterError::FieldsOnHigherLevel)
        );
        let allowed = [
            Filter::project().action(EventAction::Undeleted),
            Filter::new(ResourceType::Goal).action(EventAction::Changed),
            Filter::new(ResourceType::Portfolio).action(EventAction::Deleted),
            Filter::new(ResourceType::ProjectTemplate).action(EventAction::Added),
            Filter::new(ResourceType::TeamMembership).action(EventAction::Removed),
            Filter::new(ResourceType::WorkspaceMembership).action(EventAction::Added),
            Filter::project(),
        ];
        assert_eq!(validate_filters(&target, &allowed), Ok(()));
        for refused in [
            Filter::new(ResourceType::Portfolio).action(EventAction::Changed),
            Filter::new(ResourceType::TeamMembership).action(EventAction::Changed),
            Filter::section().action(EventAction::Added),
            Filter::new(ResourceType::Other("allocation".to_owned())),
        ] {
            assert!(
                matches!(
                    validate_filters(&target, &[refused]),
                    Err(FilterError::NotAllowedOnHigherLevel { .. })
                ),
                "{target}"
            );
        }
    }
}
