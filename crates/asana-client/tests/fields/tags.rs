//! The tags handle: found by name across pages, and made when missing.

use asana_client::NameMatch;
use asana_client::openapi::ids::WorkspaceGid;

use crate::helpers::{body, param, path};
use crate::support::{Scripted, client, reply};

const WORKSPACE: &str = "1201234567890100";

fn workspace() -> WorkspaceGid {
    WORKSPACE.parse().unwrap()
}

const FIRST_PAGE: &str = r#"{"data": [
    {"gid": "1201234567890201", "resource_type": "tag", "name": "Urgent"},
    {"gid": "1201234567890202", "resource_type": "tag", "name": "Waiting"}],
  "next_page": {"offset": "b2Zmc2V0LTI", "path": "/workspaces/1201234567890100/tags?offset=b2Zmc2V0LTI",
                "uri": "https://app.asana.com/api/1.0/workspaces/1201234567890100/tags?offset=b2Zmc2V0LTI"}}"#;
const SECOND_PAGE: &str = r#"{"data": [
    {"gid": "1201234567890203", "resource_type": "tag", "name": "Urgent"}],
  "next_page": null}"#;

#[tokio::test(start_paused = true)]
async fn find_by_name_reads_every_page_and_reports_shared_names() {
    let transport = Scripted::new(|_, index| {
        Ok(reply(
            200,
            if index % 2 == 0 {
                FIRST_PAGE
            } else {
                SECOND_PAGE
            },
        ))
    });
    let client = client(transport.clone());

    let urgent = client
        .tags()
        .find_by_name(&workspace(), "Urgent")
        .await
        .unwrap();
    let NameMatch::Several(urgent) = urgent else {
        panic!("two tags share the name");
    };
    assert_eq!(urgent.len(), 2);

    let first = transport.request(0);
    assert_eq!(path(&first), format!("/workspaces/{WORKSPACE}/tags"));
    assert_eq!(param(&first, "opt_fields").as_deref(), Some("name"));
    assert_eq!(param(&first, "limit").as_deref(), Some("100"));
    assert_eq!(
        param(&transport.request(1), "offset").as_deref(),
        Some("b2Zmc2V0LTI")
    );

    let waiting = client
        .tags()
        .find_by_name(&workspace(), "Waiting")
        .await
        .unwrap();
    assert_eq!(
        waiting.one().unwrap().gid.unwrap().to_string(),
        "1201234567890202"
    );
}

#[tokio::test(start_paused = true)]
async fn ensure_makes_a_missing_tag_in_the_workspace() {
    let transport = Scripted::new(|request, _| {
        Ok(if request.method() == http::Method::GET {
            reply(200, SECOND_PAGE)
        } else {
            reply(
                201,
                r#"{"data": {"gid": "1201234567890204", "resource_type": "tag", "name": "On hold"}}"#,
            )
        })
    });
    let client = client(transport.clone());

    let tag = client.tags().ensure(&workspace(), "On hold").await.unwrap();
    assert_eq!(tag.gid.unwrap().to_string(), "1201234567890204");
    let create = transport.request(1);
    assert_eq!(create.method(), http::Method::POST);
    assert_eq!(path(&create), format!("/workspaces/{WORKSPACE}/tags"));
    assert_eq!(
        body(&create),
        serde_json::json!({"data": {"name": "On hold"}})
    );

    let existing = client.tags().ensure(&workspace(), "Urgent").await.unwrap();
    assert_eq!(existing.gid.unwrap().to_string(), "1201234567890203");
    assert_eq!(transport.count(), 3, "an existing tag is not made again");
}
