//! Reading Asana's scope strings, what is missing, and the scopes to ask for from descriptors.

use asana_client::oauth::{ScopeSet, required_scope};
use asana_client::openapi::endpoints::{GetTask, UpdateTask};
use asana_client::openapi::scopes::Scope;
use asana_client::scopes_for;

use crate::support::CreateThing;

#[test]
fn a_scope_string_is_read_in_order_and_unknown_scopes_are_kept() {
    let set = ScopeSet::parse("tasks:write  projects:read tasks:read brand_new:read\ttasks:read");
    assert_eq!(set.len(), 4);
    assert!(set.contains(Scope::TasksRead) && set.contains(Scope::ProjectsRead));
    assert_eq!(set.unknown().collect::<Vec<_>>(), ["brand_new:read"]);
    assert_eq!(
        set.to_string(),
        "projects:read tasks:read tasks:write brand_new:read"
    );
    assert_eq!(set.to_string().parse::<ScopeSet>().unwrap(), set);
    assert!(ScopeSet::parse("  ").is_empty());
}

#[test]
fn missing_lists_what_the_granted_scopes_do_not_cover() {
    let granted = ScopeSet::parse("tasks:read projects:read");
    let required = ScopeSet::parse("tasks:read tasks:write brand_new:read");
    assert_eq!(
        granted.missing(&required).to_string(),
        "tasks:write brand_new:read"
    );
    assert!(required.missing(&required).is_empty());
    // Write does not imply read.
    assert!(!ScopeSet::from([Scope::TasksWrite]).allows(Scope::TasksRead));
}

#[test]
fn full_permissions_cover_every_api_scope_but_not_the_openid_ones() {
    let full = ScopeSet::from([Scope::Default]);
    assert!(full.allows(Scope::TasksDelete));
    assert!(!full.allows(Scope::Openid));
    assert_eq!(
        full.missing(&ScopeSet::parse("tasks:read email"))
            .to_string(),
        "email"
    );
}

#[test]
fn the_scopes_to_request_come_from_the_descriptors() {
    assert_eq!(required_scope::<GetTask>(), Some(Scope::TasksRead));
    assert_eq!(required_scope::<CreateThing>(), None);
    assert_eq!(
        scopes_for![GetTask, UpdateTask].to_string(),
        "tasks:read tasks:write"
    );
}

#[test]
fn an_operation_without_a_granular_scope_asks_for_full_permissions_alone() {
    let set = ScopeSet::new()
        .with(Scope::Openid)
        .with_endpoint::<GetTask>()
        .with_endpoint::<CreateThing>()
        .with_endpoint::<UpdateTask>();
    assert_eq!(set.to_string(), "default openid");
    assert_eq!(scopes_for![CreateThing].to_string(), "default");
}
