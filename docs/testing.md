# Testing code that uses the client

Tests of code built on `asana-client` need neither a network nor a token. The client sends every
request through a `Transport`, so a test puts something else there: `asana-fake`, an in-memory
Asana that keeps state across calls, or a small transport of your own that answers with canned
bodies. A third way, pointing `Client::builder().base_url(..)` at a local HTTP server such as
`wiremock`, also works and exercises the real `ReqwestTransport`.

## The fake

Add it as a dev-dependency (from git until the crates are published, then `asana-fake = "0.1"`):

```toml
[dev-dependencies]
asana-fake = { git = "https://github.com/noahbclarkson/asana-rs" }
```

`FakeAsana` is a stateful Asana in memory, plugged into the client as its transport. A test seeds
it, runs the code under test with a client built on `fake.transport()`, and asserts on what the
fake now holds and on the requests it received. From the `asana-fake` crate doc test:

```rust
use asana_client::openapi::endpoints::AddTaskForSection;
use asana_client::{Auth, Client};
use asana_fake::FakeAsana;

let fake = FakeAsana::new();
let project = fake.workspace("Acme").project("Onboarding");
let [todo, done] = project.sections(["To do", "Done"]);
let task = project.task("Welcome Ada").in_section(&todo).gid();

let client = Client::builder()
    .auth(Auth::personal_access_token("0/fake"))
    .transport(fake.transport())
    .build()?;

// The code under test moves the task.
client.sections().add_task(done.clone(), task.clone()).send().await?;

assert_eq!(fake.section_of(&task, &project.gid()), Some(done));
assert_eq!(fake.requests_for::<AddTaskForSection>().len(), 1);
```

What it serves, and how it answers:

- **The operations the handles use**: tasks (subtasks, projects, followers, tags, parents,
  dependencies), sections, projects, custom fields and enum options, tags, users and workspaces,
  task search, stories, attachments (upload, links and download), task templates, duplication
  and jobs, the batch API, the events stream, and webhooks. Any other operation answers `501`
  naming its `operationId`, and so does a task-search filter or sort the fake does not model
  (`followers.any`, `sort_by=likes`, …), so a gap in the fake is loud instead of a filter
  silently ignored.
- **Asana's shapes and refusals**: the `{"data": …}` envelope, `next_page` with an opaque offset
  when a `limit` is sent, `opt_fields` honoured (nested paths included), and Asana's error
  bodies. As Asana does, it answers `400` for an `opt_fields` path the specification does not list
  for the resource (a misspelt `with_raw` path), for an offset issued for another list or other
  filters, and for a `custom_fields` search operator Asana does not have, and `404` for a path
  no operation has.
- **Failures on request**: `fake.fail_next::<E>(fault)` makes the next call of operation `E`
  fail, and `fail_next_request(fault)` the next call of any operation. A `Fault` is
  `rate_limited(retry_after)`, `server_error()`, `premium_required()`, `status(code, message)`,
  `timeout()`, `timeout_after_applying()` or `slow(delay)`. `timeout()` loses the request before
  anything happens; `timeout_after_applying()` carries it out and then loses the answer, which is
  the case a create must not be sent again for: after it the fake holds what the request made, so
  a test of "the create timed out, read before writing again" can fail where it would live. A
  workspace seeded with `.free()` answers task search `402`.
  Run such tests with `#[tokio::test(start_paused = true)]` so the
  client's waits pass at once.
- **Other people's changes**: `fake.as_user(&user)` returns an `Actor` that moves, creates,
  renames, completes, assigns, comments on and deletes tasks as that user, as someone in the web
  app would, making the same events.
- **Looking inside**: `fake.section_of(&task, &project)` gives a task's section,
  `fake.requests()` and `fake.requests_for::<E>()` the recorded calls, and `fake.record(&gid)`
  any object's full JSON as the fake holds it.
- **Determinism**: GIDs come from a counter starting at `1200000000000001`, and times from the
  clock given to `FakeAsana::with_clock`.

### Webhooks end to end

The fake performs the webhook handshake against a receiver the test registers with
`fake.receiver(..)` (a function from an `http::Request<Bytes>` to a future of an
`http::Response<Bytes>`, so any framework's handler can be called from it), and signs its
deliveries with the webhook's secret, so a test covers the whole path: establishing the webhook,
verifying deliveries, and the work that follows.

From the doc test on `FakeAsana::receiver`, the whole path with the application's receiver
built on `asana_webhooks::Receiver`, as a framework adapter would serve it (the test also uses
`http`, `bytes` and `tokio` as dev-dependencies):

```rust
use asana_client::{Auth, Client};
use asana_fake::FakeAsana;
use asana_webhooks::{EventAction, Filter, MemoryHandshakeStore, MemorySecrets, Receiver};
use bytes::Bytes;
use tokio::sync::mpsc;

let fake = FakeAsana::new();
let acme = fake.workspace("Acme");
let ada = acme.user("Ada Lovelace", "ada@example.com");
let project = acme.project("Onboarding");
let [todo, done] = project.sections(["To do", "Done"]);
let task = project.task("Welcome Ada").in_section(&todo).gid();
let client = Client::builder()
    .auth(Auth::personal_access_token("0/fake"))
    .transport(fake.transport())
    .build()?;

// The application's receiver: handshakes into a store, verified batches into a channel.
let (sink, mut deliveries) = mpsc::channel(16);
let receiver = Receiver::new(MemoryHandshakeStore::new(), MemorySecrets::new(), sink);
let app = receiver.clone();
fake.receiver(move |request: http::Request<Bytes>| {
    let app = app.clone();
    async move {
        let route = request.uri().path().rsplit('/').next().unwrap_or_default().to_owned();
        let headers: Vec<(String, String)> = request
            .headers()
            .iter()
            .filter_map(|(name, value)| Some((name.to_string(), value.to_str().ok()?.to_owned())))
            .collect();
        let reply = app.receive(&route, &headers, request.body()).await;
        let mut response = http::Response::builder().status(reply.status());
        if let Some(echo) = reply.echo() {
            response = response.header(echo.header_name(), echo.header_value());
        }
        response.body(Bytes::new()).unwrap_or_default()
    }
});

// The fake handshakes into the receiver while `establish` waits.
let established = client
    .webhooks()
    .establish(project.gid(), "https://hooks.example.com/webhooks/asana")
    .filter(Filter::task().action(EventAction::Added))
    .store(receiver.handshakes())
    .send()
    .await?;
receiver.secrets().insert(&established.route, established.secret);

// Someone drags the task to "Done"; the fake signs and delivers the event.
fake.as_user(&ada).move_task(&task, &done);
let delivered = fake.deliver().await;
assert_eq!(delivered[0].status, http::StatusCode::OK);

let delivery = deliveries.try_recv()?;
assert_eq!(delivery.batch.events[0].section_entered(), Some((task, done)));
```

Creating a webhook (through `client.webhooks().establish(..)` or `fake.create_webhook(..)`) sends
the registered receiver the handshake and refuses the webhook unless the secret is echoed. From
then on, every change made through the API or by an `Actor` queues events on each webhook that
can see it; `fake.deliver().await` sends them as one signed batch per webhook and returns a
`Vec<Delivered>`, each with the status the receiver answered; `fake.set_auto_deliver(true)`
sends them as each request ends; and `fake.heartbeat(&webhook).await` sends an empty batch.
Seeding makes no events. `fake.expire_sync_tokens()` makes the events stream answer `412`, to
test the re-read path.

## A transport of your own

For a test about one call, implement `Transport`: it takes an `http::Request<Bytes>` and returns
an `http::Response<Bytes>`, so the test can answer with any status and body and look at exactly
what was sent. Answer every status with `Ok`; return an error only to simulate no response at
all (`TransportError::connect`, `TransportError::timeout`). `Arc<T>` of a transport is a
transport, so a test keeps one handle for its assertions and gives the client another. From
[`crates/asana-client/examples/mock_transport.rs`](../crates/asana-client/examples/mock_transport.rs),
which runs offline (`cargo run -p asana-client --example mock_transport`):

```rust
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
```

and the test itself:

```rust
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
```

The types come from `http`, `bytes` and `futures-util` (`BoxFuture`), which such a test adds as
dev-dependencies.

Notes for tests at this level:

- **Bodies.** Asana wraps a result in `{"data": ...}` and a list in
  `{"data": [...], "next_page": {"offset": ..., "path": ..., "uri": ...}}`, or `"next_page": null`
  on the last page. Errors are `{"errors": [{"message": ..., "help": ...}]}`. Take shapes from
  Asana's API reference and use made-up GIDs.
- **Retries.** `RetryPolicy::never()` sends each request once, so a scripted `503` comes straight
  back as `Error::Server`. To test the retry itself, keep the default policy and run the test
  with `#[tokio::test(start_paused = true)]`: the client's waits (back-off, `Retry-After`, job
  polling) are on tokio's clock, so a paused clock skips them instead of sleeping.
- **What is sent.** The request's path includes the base URL's path (`/api/1.0/...` by default),
  the query is percent-encoded, and the body of a write is Asana's `{"data": ...}` envelope.
  The `Authorization` header is `Bearer` and the token.

## Calling a live workspace

A test that calls the real API belongs behind `#[ignore]`, reading its token from the environment
(this repository uses `ASANA_TEST_TOKEN`), run by hand against a workspace made for testing. CI
never holds a token.
