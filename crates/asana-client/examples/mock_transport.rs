//! A test double for the client's transport: canned answers, and a record of what was sent.
//! The same shape works in a `#[tokio::test]`; this runs as an example and needs no network.
//!
//! ```text
//! cargo run -p asana-client --example mock_transport
//! ```

use std::sync::{Arc, Mutex};

use asana_client::openapi::ids::TaskGid;
use asana_client::{Auth, Client, Error, RetryPolicy, Transport, TransportError, fields};
use bytes::Bytes;
use futures_util::future::BoxFuture;

/// Answers every request from a closure and keeps each request it saw.
struct Canned {
    answer: fn(&http::Request<Bytes>) -> http::Response<Bytes>,
    seen: Mutex<Vec<http::Request<Bytes>>>,
}

impl Transport for Canned {
    fn send(
        &self,
        request: http::Request<Bytes>,
    ) -> BoxFuture<'_, Result<http::Response<Bytes>, TransportError>> {
        let response = (self.answer)(&request);
        if let Ok(mut seen) = self.seen.lock() {
            seen.push(request);
        }
        Box::pin(async move { Ok(response) })
    }
}

/// Asana's envelope around a task, or its error body for any other path.
fn answer(request: &http::Request<Bytes>) -> http::Response<Bytes> {
    let (status, body) = if request.uri().path().ends_with("/tasks/1200000000000001") {
        (
            200,
            r#"{"data": {"gid": "1200000000000001", "resource_type": "task", "name": "Acme launch", "completed": false}}"#,
        )
    } else {
        (404, r#"{"errors": [{"message": "Not a recognized ID"}]}"#)
    };
    http::Response::builder()
        .status(status)
        .body(Bytes::from_static(body.as_bytes()))
        .unwrap_or_default()
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let transport = Arc::new(Canned {
        answer,
        seen: Mutex::new(Vec::new()),
    });
    // Keep one `Arc` for the assertions and give the client another.
    let client = Client::builder()
        .auth(Auth::personal_access_token("0/made-up"))
        .transport(Arc::clone(&transport))
        .retry(RetryPolicy::never())
        .build()?;

    let task: TaskGid = "1200000000000001".parse()?;
    let read = client
        .tasks()
        .get(task)
        .fields(fields![Task: name, completed])
        .send()
        .await?;
    assert_eq!(read.name.as_deref(), Some("Acme launch"));

    // A 404 becomes `Error::NotFound`, with Asana's messages kept for a developer to read.
    let missing = client.tasks().get("1200000000000002".parse()?).send().await;
    assert!(matches!(missing, Err(Error::NotFound(_))));

    // What the client sent: the method, the path under the base URL, the query.
    let seen = transport.seen.lock().map_err(|_| "poisoned")?;
    let first = &seen[0];
    assert_eq!(first.method(), http::Method::GET);
    assert_eq!(first.uri().path(), "/api/1.0/tasks/1200000000000001");
    assert_eq!(first.uri().query(), Some("opt_fields=name%2Ccompleted"));
    println!("{} requests, as expected", seen.len());
    Ok(())
}
