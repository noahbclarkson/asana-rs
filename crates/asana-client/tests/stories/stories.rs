//! Comments and reading the latest stories.

use asana_client::Html;
use asana_client::openapi::ids::{TaskGid, UserGid};
use asana_client::resources::stories::StoryExt;
use serde_json::json;

use crate::support::{BASE, Scripted, TASK, client, reply};
use crate::{STORY, USER, gid, json_body, query};

fn story(gid: &str, text: &str, subtype: &str) -> serde_json::Value {
    json!({
        "gid": gid,
        "resource_type": "story",
        "created_at": "2026-01-15T02:06:58.147Z",
        "resource_subtype": subtype,
        "text": text,
        "type": if subtype == "comment_added" { "comment" } else { "system" },
    })
}

#[tokio::test(start_paused = true)]
async fn a_rich_comment_is_posted_as_html_text_with_its_options() {
    let created = json!({ "data": story(STORY, "Approved", "comment_added") }).to_string();
    let transport = Scripted::new(move |_, _| Ok(reply(201, &created)));
    let client = client(transport.clone());
    let reviewer: UserGid = gid(USER);
    let made = client
        .stories()
        .comment(
            gid::<TaskGid>(TASK),
            Html::paragraph("Approved by ")
                .mention(&reviewer)
                .text(" <today>"),
        )
        .pinned(true)
        .send()
        .await
        .unwrap();
    assert!(made.is_comment());

    let request = transport.request(0);
    assert_eq!(request.method(), http::Method::POST);
    assert_eq!(
        request.uri().to_string(),
        format!("{BASE}/tasks/{TASK}/stories")
    );
    assert_eq!(
        json_body(&request),
        json!({ "data": {
            "html_text": format!(
                "<body>Approved by <a data-asana-gid=\"{USER}\"/> &lt;today&gt;</body>"
            ),
            "is_pinned": true,
        }})
    );
}

#[tokio::test(start_paused = true)]
async fn plain_text_is_sent_as_text() {
    let created = json!({ "data": story(STORY, "<b>ok</b>", "comment_added") }).to_string();
    let transport = Scripted::new(move |_, _| Ok(reply(201, &created)));
    let client = client(transport.clone());
    client
        .stories()
        .comment(gid(TASK), "<b>ok</b>")
        .send()
        .await
        .unwrap();
    assert_eq!(
        json_body(&transport.request(0)),
        json!({ "data": { "text": "<b>ok</b>" } })
    );
}

#[tokio::test(start_paused = true)]
async fn latest_reads_newest_first_and_returns_oldest_first() {
    // Newest first, as `sort_ascending=false` asks: two on the first page, one on the second.
    let first = json!({
        "data": [story("1203000000000005", "five", "comment_added"),
                 story("1203000000000004", "four", "assigned")],
        "next_page": { "offset": "eyJ0eXAiOiJKV1QiLCJhbGciOiJIUzI1NiJ9", "path": "/tasks/x/stories?offset=..." },
    })
    .to_string();
    let second = json!({
        "data": [story("1203000000000003", "three", "comment_added")],
        "next_page": { "offset": "c2Vjb25k", "path": "/tasks/x/stories?offset=..." },
    })
    .to_string();
    let transport = Scripted::replies(vec![Ok(reply(200, &first)), Ok(reply(200, &second))]);
    let client = client(transport.clone());

    let stories = client.stories().latest(gid(TASK), 3).send().await.unwrap();
    let texts: Vec<_> = stories.iter().map(|s| s.text.clone().unwrap()).collect();
    assert_eq!(texts, ["three", "four", "five"]);
    assert!(stories[1].is_activity() && !stories[1].is_comment());

    // Exactly as many pages as `count` needs, each asking for no more than is still wanted.
    assert_eq!(transport.count(), 2);
    let first_query = query(&transport.request(0));
    assert!(first_query.contains(&("sort_ascending".into(), "false".into())));
    assert!(first_query.contains(&("limit".into(), "3".into())));
    let second_query = query(&transport.request(1));
    assert!(second_query.contains(&("limit".into(), "1".into())));
    assert!(second_query.contains(&(
        "offset".into(),
        "eyJ0eXAiOiJKV1QiLCJhbGciOiJIUzI1NiJ9".into()
    )));
}

#[tokio::test(start_paused = true)]
async fn latest_of_none_sends_nothing() {
    let transport = Scripted::new(|_, _| Ok(reply(500, "{}")));
    let client = client(transport.clone());
    assert!(
        client
            .stories()
            .latest(gid(TASK), 0)
            .send()
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(transport.count(), 0);
}

#[tokio::test(start_paused = true)]
async fn an_edit_replaces_the_text_and_a_delete_sends_delete() {
    let updated = json!({ "data": story(STORY, "Edited", "comment_added") }).to_string();
    let transport = Scripted::replies(vec![
        Ok(reply(200, &updated)),
        Ok(reply(200, r#"{"data": {}}"#)),
    ]);
    let client = client(transport.clone());
    client
        .stories()
        .update(gid(STORY))
        .html(&Html::new().italic("Edited"))
        .send()
        .await
        .unwrap();
    client.stories().delete(gid(STORY)).send().await.unwrap();

    let update = transport.request(0);
    assert_eq!(update.method(), http::Method::PUT);
    assert_eq!(update.uri().to_string(), format!("{BASE}/stories/{STORY}"));
    assert_eq!(
        json_body(&update),
        json!({ "data": { "html_text": "<body><em>Edited</em></body>" } })
    );
    assert_eq!(transport.request(1).method(), http::Method::DELETE);
}
