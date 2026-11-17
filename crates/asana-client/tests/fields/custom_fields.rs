//! The custom fields handle: options found by name, and made when missing, safely under a race.

use asana_client::openapi::ids::CustomFieldGid;
use asana_client::{Error, NameMatch};
use futures_util::StreamExt;

use crate::helpers::{body, param, path};
use crate::support::{Scripted, client, reply};

const FIELD: &str = "1201234567890003";

fn field() -> CustomFieldGid {
    FIELD.parse().unwrap()
}

/// A field with an enabled "Approved", a disabled "Retired" and two enabled "Partner A".
fn definition(extra_option: Option<(&str, &str)>) -> String {
    let mut options = vec![
        r#"{"gid": "1201234567890031", "resource_type": "enum_option", "name": "Approved", "enabled": true, "color": "green"}"#.to_owned(),
        r#"{"gid": "1201234567890032", "resource_type": "enum_option", "name": "Retired", "enabled": false, "color": "none"}"#.to_owned(),
        r#"{"gid": "1201234567890033", "resource_type": "enum_option", "name": "Partner A", "enabled": true, "color": "blue"}"#.to_owned(),
        r#"{"gid": "1201234567890034", "resource_type": "enum_option", "name": "Partner A", "enabled": true, "color": "red"}"#.to_owned(),
    ];
    if let Some((gid, name)) = extra_option {
        options.push(format!(
            r#"{{"gid": "{gid}", "resource_type": "enum_option", "name": "{name}", "enabled": true, "color": "none"}}"#
        ));
    }
    format!(
        r#"{{"data": {{"gid": "{FIELD}", "resource_type": "custom_field", "name": "Stage", "enum_options": [{}]}}}}"#,
        options.join(",")
    )
}

fn created(gid: &str, name: &str) -> String {
    format!(
        r#"{{"data": {{"gid": "{gid}", "resource_type": "enum_option", "name": "{name}", "enabled": true, "color": "none"}}}}"#
    )
}

#[tokio::test(start_paused = true)]
async fn option_by_name_reads_the_options_once_and_matches_exactly() {
    let transport = Scripted::new(|_, _| Ok(reply(200, &definition(None))));
    let client = client(transport.clone());
    let fields = client.custom_fields();

    let approved = fields.option_by_name(&field(), "Approved").await.unwrap();
    let NameMatch::One(approved) = approved else {
        panic!("one match");
    };
    assert_eq!(approved.gid.unwrap().to_string(), "1201234567890031");

    let request = transport.request(0);
    assert_eq!(path(&request), format!("/custom_fields/{FIELD}"));
    assert_eq!(
        param(&request, "opt_fields").as_deref(),
        Some("name,enum_options.name,enum_options.enabled,enum_options.color")
    );

    assert_eq!(
        fields.option_by_name(&field(), "approved").await.unwrap(),
        NameMatch::Missing
    );
}

#[tokio::test(start_paused = true)]
async fn disabled_options_match_only_when_asked_and_shared_names_are_reported() {
    let transport = Scripted::new(|_, _| Ok(reply(200, &definition(None))));
    let client = client(transport);
    let fields = client.custom_fields();

    assert_eq!(
        fields.option_by_name(&field(), "Retired").await.unwrap(),
        NameMatch::Missing
    );
    assert!(matches!(
        fields
            .option_by_name_including_disabled(&field(), "Retired")
            .await
            .unwrap(),
        NameMatch::One(_)
    ));

    let partners = fields.option_by_name(&field(), "Partner A").await.unwrap();
    assert!(partners.is_ambiguous());
    assert_eq!(
        partners.first().unwrap().gid.unwrap().to_string(),
        "1201234567890033"
    );
}

#[tokio::test(start_paused = true)]
async fn ensure_option_returns_an_existing_option_without_creating() {
    let transport = Scripted::new(|_, _| Ok(reply(200, &definition(None))));
    let client = client(transport.clone());

    let option = client
        .custom_fields()
        .ensure_option(&field(), "Approved")
        .await
        .unwrap();
    assert_eq!(option.name.as_deref(), Some("Approved"));
    assert_eq!(transport.count(), 1);
}

#[tokio::test(start_paused = true)]
async fn ensure_option_creates_a_missing_option() {
    let transport = Scripted::new(|request, _| {
        Ok(if request.method() == http::Method::GET {
            reply(200, &definition(None))
        } else {
            reply(201, &created("1201234567890035", "Partner B"))
        })
    });
    let client = client(transport.clone());

    let option = client
        .custom_fields()
        .ensure_option(&field(), "Partner B")
        .await
        .unwrap();
    assert_eq!(option.gid.unwrap().to_string(), "1201234567890035");

    assert_eq!(transport.count(), 2);
    let create = transport.request(1);
    assert_eq!(create.method(), http::Method::POST);
    assert_eq!(
        path(&create),
        format!("/custom_fields/{FIELD}/enum_options")
    );
    assert_eq!(
        body(&create),
        serde_json::json!({"data": {"name": "Partner B"}})
    );
}

#[tokio::test(start_paused = true)]
async fn ensure_option_that_loses_a_race_returns_the_winners_option() {
    // The first read finds nothing; another caller then makes the option, so this creation is
    // refused; the second read finds the other caller's option.
    let transport = Scripted::new(|request, index| {
        let read = request.method() == http::Method::GET;
        Ok(match (read, index) {
            (true, 0) => reply(200, &definition(None)),
            (true, _) => reply(200, &definition(Some(("1201234567890036", "Partner B")))),
            (false, _) => reply(
                400,
                r#"{"errors": [{"message": "An enum option with this name already exists.", "help": "For more information on API status codes and how to handle them, read the docs on errors: https://developers.asana.com/docs/errors"}]}"#,
            ),
        })
    });
    let client = client(transport.clone());

    let option = client
        .custom_fields()
        .ensure_option(&field(), "Partner B")
        .await
        .unwrap();
    assert_eq!(option.gid.unwrap().to_string(), "1201234567890036");
    assert_eq!(transport.count(), 3);
}

#[tokio::test(start_paused = true)]
async fn ensure_option_returns_the_creation_error_when_the_reread_finds_nothing() {
    let transport = Scripted::new(|request, _| {
        Ok(if request.method() == http::Method::GET {
            reply(200, &definition(None))
        } else {
            reply(403, r#"{"errors": [{"message": "Forbidden"}]}"#)
        })
    });
    let client = client(transport.clone());

    let error = client
        .custom_fields()
        .ensure_option(&field(), "Partner B")
        .await
        .unwrap_err();
    assert!(matches!(error, Error::Forbidden(_)));
    assert_eq!(transport.count(), 3);
}

#[tokio::test(start_paused = true)]
async fn options_are_renamed_and_disabled_with_a_put() {
    let transport = Scripted::new(|_, _| Ok(reply(200, &created("1201234567890031", "Done"))));
    let client = client(transport.clone());
    let fields = client.custom_fields();
    let option = "1201234567890031".parse().unwrap();

    fields.rename_option(option, "Done").send().await.unwrap();
    let rename = transport.request(0);
    assert_eq!(rename.method(), http::Method::PUT);
    assert_eq!(path(&rename), "/enum_options/1201234567890031");
    assert_eq!(body(&rename), serde_json::json!({"data": {"name": "Done"}}));

    fields
        .disable_option("1201234567890031".parse().unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(
        body(&transport.request(1)),
        serde_json::json!({"data": {"enabled": false}})
    );
}

#[tokio::test(start_paused = true)]
async fn a_new_option_can_be_placed_and_coloured() {
    let transport = Scripted::new(|_, _| Ok(reply(201, &created("1201234567890037", "Urgent"))));
    let client = client(transport.clone());

    client
        .custom_fields()
        .create_option(field(), "Urgent")
        .color("red")
        .insert_before(&"1201234567890031".parse().unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(
        body(&transport.request(0)),
        serde_json::json!({"data": {"name": "Urgent", "color": "red", "insert_before": "1201234567890031"}})
    );
}

#[tokio::test(start_paused = true)]
async fn a_workspaces_fields_stream_across_pages() {
    let transport = Scripted::new(|_, index| {
        Ok(reply(
            200,
            if index == 0 {
                r#"{"data": [{"gid": "1201234567890001", "resource_type": "custom_field", "name": "Notes", "resource_subtype": "text"}],
                    "next_page": {"offset": "eyJ0eXAiOiJKV1QifQ", "path": "/workspaces/1201234567890100/custom_fields?offset=eyJ0eXAiOiJKV1QifQ", "uri": "https://app.asana.com/api/1.0/workspaces/1201234567890100/custom_fields?offset=eyJ0eXAiOiJKV1QifQ"}}"#
            } else {
                r#"{"data": [{"gid": "1201234567890003", "resource_type": "custom_field", "name": "Stage", "resource_subtype": "enum"}], "next_page": null}"#
            },
        ))
    });
    let client = client(transport.clone());

    let names: Vec<_> = client
        .custom_fields()
        .list("1201234567890100".parse().unwrap())
        .stream()
        .map(|field| field.unwrap().name.unwrap())
        .collect()
        .await;
    assert_eq!(names, ["Notes", "Stage"]);
    assert_eq!(
        param(&transport.request(1), "offset").as_deref(),
        Some("eyJ0eXAiOiJKV1QifQ")
    );
}

#[tokio::test(start_paused = true)]
async fn a_field_is_added_to_and_removed_from_a_project() {
    let transport = Scripted::new(|request, _| {
        Ok(
            if request.uri().path().ends_with("/addCustomFieldSetting") {
                reply(
                    200,
                    r#"{"data": {"gid": "1201234567890501", "resource_type": "custom_field_setting", "is_important": true}}"#,
                )
            } else {
                reply(200, r#"{"data": {}}"#)
            },
        )
    });
    let client = client(transport.clone());
    let fields = client.custom_fields();
    let project: asana_client::openapi::ids::ProjectGid = "1201234567890301".parse().unwrap();

    let setting = fields
        .add_to_project(project.clone(), field())
        .important(true)
        .insert_after(&"1201234567890500".parse().unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(setting.is_important, Some(true));
    let add = transport.request(0);
    assert_eq!(add.method(), http::Method::POST);
    assert_eq!(
        path(&add),
        "/projects/1201234567890301/addCustomFieldSetting"
    );
    assert_eq!(
        body(&add),
        serde_json::json!({"data": {"custom_field": FIELD, "insert_after": "1201234567890500", "is_important": true}})
    );

    fields
        .remove_from_project(project, field())
        .send()
        .await
        .unwrap();
    let remove = transport.request(1);
    assert_eq!(
        path(&remove),
        "/projects/1201234567890301/removeCustomFieldSetting"
    );
    assert_eq!(
        body(&remove),
        serde_json::json!({"data": {"custom_field": FIELD}})
    );
}
