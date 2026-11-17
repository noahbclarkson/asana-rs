# asana-fake

An in-memory Asana for tests. `FakeAsana` is a stateful fake behind
[`asana-client`](https://docs.rs/asana-client)'s transport: your code talks to a real `Client`, and
the fake answers with Asana's own envelope, paging and error shapes. It performs the webhook
handshake against a receiver you register and signs its deliveries, so "a task dragged into a
section starts my work" can be tested end to end with no network and no token.

```rust
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

// ... run the code under test against `client`, then assert on the fake:
assert_eq!(fake.section_of(&task, &project.gid()), Some(done));
assert!(fake.requests().iter().any(|request| request.operation == Some("addTaskForSection")));
```

## Seeding

`fake.workspace("Acme")` returns a builder with `.user(name, email)`, `.project(name)`, `.tag(name)`,
`.enum_field(name, &["Planned", "Sent"])` (and `text_field`, `number_field`, `multi_enum_field`,
`date_field`, `people_field`), and `.free()` for a workspace on the free plan, where search answers
`402`. A project has `.section(name)`, `.sections([..])`, `.task(name)`, `.with_field(&field)` and
`.template(name, task_name, description)`; a task has `.in_section`, `.also_in`, `.notes`,
`.completed`, `.due_on`, `.assignee`, `.follower`, `.tag`, `.custom_field(&field, &value)` and
`.subtask(name)`. Seeding makes no webhook events. GIDs come from a counter, so a test is
deterministic; times come from the clock given to `FakeAsana::with_clock`.

## Acting as a person

`fake.as_user(&ada)` moves, creates, renames, completes, assigns, comments on and deletes tasks
and sets custom fields as someone would in Asana's web app. Each action makes the events Asana
would (a section move is `removed` from the old section, `added` to the new one, and a
`section_changed` story), attributed to that person.

## Webhooks

```rust
fake.receiver(|request: http::Request<bytes::Bytes>| async move {
    my_app.handle(request).await // answer the handshake, verify deliveries
});
client.webhooks().establish(project.gid(), "https://hooks.example.com/asana").store(&store).send().await?;
fake.as_user(&ada).move_task(&task, &done);
let deliveries = fake.deliver().await; // one signed batch per webhook
```

Creating a webhook (through the client, or `fake.create_webhook`) sends the receiver the handshake
with `X-Hook-Secret` and refuses the webhook unless it is echoed. Deliveries carry
`X-Hook-Signature`, the HMAC-SHA256 of the body under the secret. A `410` from the receiver deletes
the webhook; any other failure keeps the events for the next `deliver()`. `fake.heartbeat(&webhook)`
sends an empty batch, and `fake.set_auto_deliver(true)` delivers at the end of every request.

## Faults

```rust
fake.fail_next::<asana_client::openapi::endpoints::CreateTask>(Fault::rate_limited(Duration::from_secs(2)));
fake.fail_next_request(Fault::server_error()); // also Fault::timeout(), Fault::slow(d), Fault::status(code, msg)
```

## What it serves

Tasks (subtasks, projects, followers, tags, parents, dependencies, duplication), sections
(`addTask` included), projects and task counts, custom fields, enum options and project settings,
tags, users and workspaces, task search with the common filters, stories, attachments (multipart
upload, links, downloads), task templates and jobs, the batch API, the events stream (`412` first)
and webhooks. Every other operation in Asana's specification answers `501` naming its
`operationId`, so a gap in the fake is loud.

Licensed under either of Apache License 2.0 or MIT license, at your option.
