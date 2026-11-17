//! What goes on the wire: the URL and its query, the headers, JSON and multipart bodies, and the
//! deprecation headers in both directions.

use asana_client::openapi::endpoint::QueryPair;
use asana_client::openapi::endpoints::GetTask;
use asana_client::{Auth, Client};

use crate::support::{
    BASE, CreateThing, GetEvents, Scripted, TASK, Upload, client, reply, reply_with, task_body,
};

#[tokio::test(start_paused = true)]
async fn the_url_joins_base_path_and_encoded_query() {
    let transport = Scripted::new(|_, _| Ok(reply(200, task_body())));
    let client = client(transport.clone());
    let endpoint = GetTask::builder(TASK.parse().unwrap())
        .opt_fields(asana_client::openapi::fields![Task: name, memberships.section.name])
        .build();
    client.send(endpoint).await.unwrap();
    let request = transport.request(0);
    assert_eq!(request.method(), http::Method::GET);
    assert_eq!(
        request.uri().to_string(),
        format!("{BASE}/tasks/{TASK}?opt_fields=name%2Cmemberships.section.name")
    );
    assert_eq!(request.headers()["accept"], "application/json");
    assert!(request.headers().get("content-type").is_none());
    assert!(
        request.headers()["user-agent"]
            .to_str()
            .unwrap()
            .starts_with("asana-rs/")
    );

    let query: Vec<QueryPair> = vec![
        ("resource".into(), "1201234567890123".into()),
        ("sync".into(), "a b&c=d/é+".into()),
    ];
    client.send(GetEvents { query }).await.ok();
    let uri = transport.request(1).uri().to_string();
    assert_eq!(
        uri,
        format!("{BASE}/events?resource=1201234567890123&sync=a+b%26c%3Dd%2F%C3%A9%2B")
    );
}

#[tokio::test(start_paused = true)]
async fn a_json_body_is_sent_with_its_content_type() {
    let transport = Scripted::new(|_, _| Ok(reply(201, r#"{"data":{"gid":"1","name":"Acme"}}"#)));
    let thing = client(transport.clone())
        .send(CreateThing {
            name: "Acme".into(),
        })
        .await
        .unwrap();
    assert_eq!(thing.name.as_deref(), Some("Acme"));
    let request = transport.request(0);
    assert_eq!(request.method(), http::Method::POST);
    assert_eq!(request.headers()["content-type"], "application/json");
    let body: serde_json::Value = serde_json::from_slice(request.body()).unwrap();
    assert_eq!(body, serde_json::json!({ "data": { "name": "Acme" } }));
}

#[tokio::test(start_paused = true)]
async fn a_multipart_body_is_well_formed() {
    let transport = Scripted::new(|_, _| Ok(reply(200, r#"{"data":{"gid":"1"}}"#)));
    client(transport.clone()).send(Upload).await.unwrap();
    let request = transport.request(0);
    let content_type = request.headers()["content-type"].to_str().unwrap();
    let boundary = content_type
        .strip_prefix("multipart/form-data; boundary=")
        .unwrap();
    assert!(boundary.len() >= 30);
    let body = String::from_utf8(request.body().to_vec()).unwrap();
    let expected = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"parent\"\r\n\r\n{TASK}\r\n\
         --{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"acme-plan.pdf\"\r\n\
         Content-Type: application/pdf\r\n\r\n%PDF-1.7 made up\r\n--{boundary}--\r\n"
    );
    assert_eq!(body, expected);
}

#[tokio::test(start_paused = true)]
async fn asana_enable_and_disable_are_sent_and_announced_changes_recorded_once() {
    let transport = Scripted::new(|_, _| {
        Ok(reply_with(
            200,
            (
                "asana-change",
                "name=new_user_task_lists;info=https://example.com/changes;affected=true",
            ),
            task_body(),
        ))
    });
    let client = Client::builder()
        .auth(Auth::personal_access_token("0/acme-test-token-not-real"))
        .transport(transport.clone())
        .base_url(BASE)
        .user_agent("acme-sync/1.4")
        .asana_enable(["new_goal_memberships", "new_memberships"])
        .asana_disable(["new_project_templates"])
        .build()
        .unwrap();
    client.send(crate::support::get_task()).await.unwrap();
    client.send(crate::support::get_task()).await.unwrap();

    let request = transport.request(0);
    assert_eq!(
        request.headers()["asana-enable"],
        "new_goal_memberships,new_memberships"
    );
    assert_eq!(request.headers()["asana-disable"], "new_project_templates");
    assert!(
        request.headers()["user-agent"]
            .to_str()
            .unwrap()
            .starts_with("acme-sync/1.4 asana-rs/")
    );
    let changes = client.observed_changes();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].name, "new_user_task_lists");
    assert!(changes[0].affected);
}

#[test]
fn a_bad_base_url_or_change_name_is_refused_when_building() {
    let build = |base: &str, change: &str| {
        Client::builder()
            .auth(Auth::personal_access_token("x"))
            .transport(Scripted::new(|_, _| Ok(reply(200, "{}"))))
            .base_url(base)
            .asana_enable([change])
            .build()
    };
    assert!(build(BASE, "ok_name").is_ok());
    assert!(build("not a url", "ok_name").is_err());
    assert!(build(BASE, "bad name").is_err());
}

#[tokio::test(start_paused = true)]
async fn a_path_parameter_cannot_walk_the_path() {
    use asana_client::Error;
    use asana_client::openapi::endpoints::{GetTaskForCustomId, GetUser};
    use asana_client::openapi::ids::UserRef;

    let transport = Scripted::new(|_, _| Ok(reply(200, task_body())));
    let client = client(transport.clone());
    let workspace = || "1200000000000001".parse().unwrap();
    for custom_id in ["..", ".", ""] {
        let error = client
            .send(GetTaskForCustomId::builder(workspace(), custom_id).build())
            .await
            .unwrap_err();
        assert!(
            matches!(error, Error::InvalidRequest { .. }),
            "{custom_id:?}: {error:?}"
        );
    }
    let error = client
        .send(GetUser::builder(UserRef::Email("..".into())).build())
        .await
        .unwrap_err();
    assert!(matches!(error, Error::InvalidRequest { .. }), "{error:?}");
    assert_eq!(transport.count(), 0);

    // A dot inside a value is only a character.
    client
        .send(GetTaskForCustomId::builder(workspace(), "ACME-1.2").build())
        .await
        .unwrap();
    assert!(
        transport
            .request(0)
            .uri()
            .path()
            .ends_with("/tasks/custom_id/ACME-1%2E2")
    );
}
