//! Paged lists through the real HTTP stack: a stream over three pages, pages, and a bound.

use asana_client::openapi::endpoints::GetTasksForProject;
use asana_client::{Auth, Client, ReqwestTransport};
use futures_util::{StreamExt, TryStreamExt};
use wiremock::matchers::{method, path, query_param, query_param_is_missing};
use wiremock::{Mock, MockServer, ResponseTemplate};

const PROJECT: &str = "1209876543210987";

fn page(names: &[&str], next: Option<&str>) -> serde_json::Value {
    let data: Vec<_> = names
        .iter()
        .enumerate()
        .map(|(index, name)| {
            serde_json::json!({ "gid": format!("12000000000000{index:02}"), "resource_type": "task", "name": name })
        })
        .collect();
    let next_page = next.map(|offset| {
        serde_json::json!({
            "offset": offset,
            "path": format!("/projects/{PROJECT}/tasks?limit=100&offset={offset}"),
            "uri": format!("https://app.asana.com/api/1.0/projects/{PROJECT}/tasks?limit=100&offset={offset}"),
        })
    });
    serde_json::json!({ "data": data, "next_page": next_page })
}

async fn three_pages() -> MockServer {
    let server = MockServer::start().await;
    let route = format!("/api/1.0/projects/{PROJECT}/tasks");
    Mock::given(method("GET"))
        .and(path(route.as_str()))
        .and(query_param_is_missing("offset"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(
            &["a", "b"],
            Some("eyJ0eXAiOJiKV1iQLCJhbGciOiJIUzI1NiJ9"),
        )))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(route.as_str()))
        .and(query_param(
            "offset",
            "eyJ0eXAiOJiKV1iQLCJhbGciOiJIUzI1NiJ9",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(&["c", "d"], Some("b+/=2"))))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(route.as_str()))
        .and(query_param("offset", "b+/=2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(&["e"], None)))
        .mount(&server)
        .await;
    server
}

fn client(server: &MockServer) -> Client {
    Client::builder()
        .auth(Auth::personal_access_token("0/acme-test-token-not-real"))
        .transport(ReqwestTransport::new(reqwest::Client::new()))
        .base_url(format!("{}/api/1.0", server.uri()))
        .build()
        .unwrap()
}

fn endpoint() -> GetTasksForProject {
    GetTasksForProject::builder(PROJECT.parse().unwrap()).build()
}

fn names(tasks: &[asana_client::openapi::models::Task]) -> Vec<&str> {
    tasks
        .iter()
        .filter_map(|task| task.name.as_deref())
        .collect()
}

#[tokio::test]
async fn a_stream_reads_every_page_in_order_passing_offsets_back_verbatim() {
    let server = three_pages().await;
    let tasks: Vec<_> = client(&server)
        .stream(endpoint())
        .try_collect()
        .await
        .unwrap();
    assert_eq!(names(&tasks), ["a", "b", "c", "d", "e"]);

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 3);
    for request in &requests {
        assert!(
            request
                .url
                .query_pairs()
                .any(|(key, value)| key == "limit" && value == "100")
        );
    }
}

#[tokio::test]
async fn nothing_is_fetched_until_the_stream_is_read() {
    let server = three_pages().await;
    let client = client(&server);
    let mut stream = client.stream(endpoint());
    assert!(server.received_requests().await.unwrap().is_empty());
    stream.next().await.unwrap().unwrap();
    stream.next().await.unwrap().unwrap();
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
    stream.next().await.unwrap().unwrap();
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

#[tokio::test]
async fn pages_and_send_page_expose_the_next_offset() {
    let server = three_pages().await;
    let client = client(&server);
    let pages: Vec<_> = client.pages(endpoint()).try_collect().await.unwrap();
    assert_eq!(pages.len(), 3);
    assert_eq!(pages[1].next.as_ref().unwrap().as_str(), "b+/=2");
    assert!(pages[2].next.is_none());

    let first = client.send_page(endpoint()).await.unwrap();
    assert_eq!(names(&first.items), ["a", "b"]);
    assert_eq!(
        first.next.unwrap().as_str(),
        "eyJ0eXAiOJiKV1iQLCJhbGciOiJIUzI1NiJ9"
    );
}

#[tokio::test]
async fn collect_max_stops_at_its_bound() {
    let server = three_pages().await;
    let client = client(&server);
    let tasks = client.collect_max(endpoint(), 3).await.unwrap();
    assert_eq!(names(&tasks), ["a", "b", "c"]);
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    assert!(
        requests[0]
            .url
            .query_pairs()
            .any(|(key, value)| key == "limit" && value == "3")
    );
}

#[tokio::test]
async fn a_failed_page_ends_the_stream_with_its_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(query_param_is_missing("offset"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(&["a"], Some("gone"))))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(query_param("offset", "gone"))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "errors": [{ "message": "offset: Your pagination token has expired." }]
        })))
        .mount(&server)
        .await;
    let results: Vec<_> = client(&server).stream(endpoint()).collect().await;
    assert_eq!(results.len(), 2);
    assert!(results[0].is_ok());
    assert!(matches!(
        results[1],
        Err(asana_client::Error::BadRequest(_))
    ));
}

#[tokio::test]
async fn a_page_with_no_items_that_offers_a_next_one_does_not_end_the_list() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(query_param_is_missing("offset"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(&["a"], Some("one"))))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(query_param("offset", "one"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(&[], Some("two"))))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(query_param("offset", "two"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(&["b"], None)))
        .mount(&server)
        .await;
    let client = client(&server);

    let tasks: Vec<_> = client.stream(endpoint()).try_collect().await.unwrap();
    assert_eq!(names(&tasks), ["a", "b"]);
    assert_eq!(client.pages(endpoint()).count().await, 3);
    assert_eq!(
        names(&client.collect_max(endpoint(), 10).await.unwrap()),
        ["a", "b"]
    );
}

#[tokio::test]
async fn the_audit_log_ends_at_its_first_page_with_no_items() {
    use asana_client::openapi::endpoints::GetAuditLogEvents;

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(query_param_is_missing("offset"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [{"gid": "1200000000000001"}],
            "next_page": {"offset": "more"}
        })))
        .mount(&server)
        .await;
    // Asana keeps offering a next page, for events still to come.
    Mock::given(method("GET"))
        .and(query_param("offset", "more"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [],
            "next_page": {"offset": "more"}
        })))
        .mount(&server)
        .await;
    let client = client(&server);
    let audit = || GetAuditLogEvents::builder("1200000000000002".parse().unwrap()).build();

    let events: Vec<_> = client.stream(audit()).try_collect().await.unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(client.collect_max(audit(), 10).await.unwrap().len(), 1);
    assert_eq!(server.received_requests().await.unwrap().len(), 4);
}
