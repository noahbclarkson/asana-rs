# asana-rs

[![CI](https://github.com/noahbclarkson/asana-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/noahbclarkson/asana-rs/actions/workflows/ci.yml)

A Rust client for [Asana's REST API](https://developers.asana.com/reference/rest-api-reference).
Every operation in Asana's OpenAPI document is generated as a typed request, a hand-written layer
makes the common work read well, and webhooks are supported from the handshake to the events you
act on.

This is an unofficial client. It is not affiliated with or endorsed by Asana, Inc.

## Quick start

Add the client and an async runtime. Until the crates are on crates.io, take them from git:

```toml
[dependencies]
asana-client = { git = "https://github.com/noahbclarkson/asana-rs" }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

Once published, the first line becomes `asana-client = "0.1"`.

Make a [personal access token](https://developers.asana.com/docs/personal-access-token) and put it
in `ASANA_ACCESS_TOKEN`. This reads a task, creates one in a section, moves it, comments on it and
sets a custom field (it is
[`crates/asana-client/examples/quickstart.rs`](crates/asana-client/examples/quickstart.rs)):

```rust
use std::env;

use asana_client::openapi::ids::{CustomFieldGid, ProjectGid, SectionGid, TaskGid};
use asana_client::{Auth, Client, CustomFieldValue, Html, Require, fields};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // A GID is parsed once, into a type of its own: a section's GID cannot go where a task's is
    // wanted, and text that is not a GID never reaches a request path.
    let task: TaskGid = env::var("ASANA_TASK_GID")?.parse()?;
    let project: ProjectGid = env::var("ASANA_PROJECT_GID")?.parse()?;
    let section: SectionGid = env::var("ASANA_SECTION_GID")?.parse()?;
    let next_section: SectionGid = env::var("ASANA_NEXT_SECTION_GID")?.parse()?;
    let amount: CustomFieldGid = env::var("ASANA_NUMBER_FIELD_GID")?.parse()?;

    // One client per token: it holds the token's rate limits. Cloning it is cheap.
    let client = Client::builder()
        .auth(Auth::personal_access_token(env::var("ASANA_ACCESS_TOKEN")?))
        .user_agent("acme-quickstart/0.1")
        .build()?;

    // Read a task with the fields you need. A misspelt field does not compile.
    let read = client
        .tasks()
        .get(task)
        .fields(fields![Task: name, completed, due_on, memberships.section.name])
        .send()
        .await?;
    // Every field of a model is an `Option`; `require` turns a missing one into an error.
    println!("read {:?}", read.name.require("name")?);

    // Create a task straight into a section.
    let created = client
        .tasks()
        .create()
        .section(project, section)
        .name("Quick start")
        .notes("Made by the asana-rs quick start.")
        .send()
        .await?;
    let created = created.gid.require("gid")?;

    // Move it to another section of the same project.
    client
        .sections()
        .add_task(next_section, created.clone())
        .send()
        .await?;

    // Comment on it, in rich text. `Html` escapes what it is given.
    client
        .stories()
        .comment(
            created.clone(),
            Html::paragraph("Moved by ").bold("asana-rs"),
        )
        .send()
        .await?;

    // Set a custom field, typed by its kind: here a number, a decimal sent as a JSON number.
    client
        .tasks()
        .update(created.clone())
        .custom_field(amount, CustomFieldValue::number(1250))
        .send()
        .await?;

    println!("made task {created}");
    Ok(())
}
```

More examples are in [`crates/asana-client/examples/`](crates/asana-client/examples/), each
reading its inputs from the environment: `create_and_move_task`, `custom_fields`, `paging`,
`search`, `batch`, `events_poll`, `stories_and_files`, `client_setup`, `oauth_flow`, and
`mock_transport` (which runs offline). Webhook receivers are in each adapter crate's `examples/`.

## The crates

| Crate | What it is for |
|---|---|
| [`asana-client`](crates/asana-client) | The client: authentication (personal access token, service account, OAuth), rate limits, retries, paging, errors, the resource handles (`client.tasks()`, …), and the client side of webhooks. Start here. |
| [`asana-openapi`](crates/asana-openapi) | Generated from Asana's specification: one descriptor per operation, one model per resource, the `opt_fields` enums and `fields!`, the OAuth scopes. Plus typed GIDs. Sends nothing. |
| [`asana-webhooks`](crates/asana-webhooks) | The webhook protocol with no HTTP client and no web framework: telling a handshake from a delivery, the handshake store, verifying signatures over the raw body, typed events, filters, de-duplication, and a framework-neutral `Receiver` that gives every answer. |
| [`asana-webhooks-axum`](crates/asana-webhooks-axum) | Mounts the `Receiver` in axum: `router(&receiver, path)`. |
| [`asana-webhooks-actix`](crates/asana-webhooks-actix) | Mounts the `Receiver` in actix-web: `resource(&receiver, path)` or `configure(..)`. |
| [`asana-fake`](crates/asana-fake) | An in-memory Asana behind the client's transport, for your tests. |

`asana-client` depends on `asana-openapi` and `asana-webhooks` and re-exports the first as
`asana_client::openapi`. To establish webhooks or receive them, add `asana-webhooks` (and an
adapter, if you use axum or actix-web) yourself.

## How it is put together

- **Every operation is a typed descriptor you send.** `asana_openapi::endpoints` has one struct
  per operation, built with a builder whose required arguments are the path's GIDs. It knows its
  method, path, query, body, OAuth scope, whether it is safe to repeat, and what it returns. The
  client sends any of them:

  ```rust
  use asana_client::{Auth, Client};
  use asana_client::openapi::endpoints::GetTask;

  let client = Client::builder()
      .auth(Auth::personal_access_token("0/abc"))
      .user_agent("acme-sync/1.0")
      .build()?;
  let task = client.send(GetTask::builder("1201234567890123".parse()?).build()).await?;
  ```

- **Handles are sugar over descriptors.** `client.tasks().get(task)` builds the same `GetTask` and
  returns a `Request` that adds setters for that operation (`.fields(..)`, `.name(..)`,
  `.custom_field(..)`). Anything a setter does not cover is reachable through
  `Request::map`/`endpoint_mut`, and `into_endpoint` hands the descriptor over (to a batch, say).
  Handles exist for the common work; nothing needs one.
- **Typed GIDs.** `TaskGid`, `SectionGid`, `ProjectGid` and the rest: one type per kind of resource.
  A GID is checked when it is made (1 to 64 ASCII digits), never parsed as a number, and cannot
  carry a `/` or `?` into a path.
- **`fields!` chooses `opt_fields`.** `fields![Task: name, memberships.section.name]` is checked
  when it compiles against the paths Asana's specification lists for that resource. Every field
  of a model is an `Option`, because what comes back depends on what was asked for; `require`
  turns a missing one into `Error::MissingField`.
- **One execution path.** Every call, generated or hand-wrapped, goes through the same steps:
  per-client limits on concurrent reads and writes and on searches per minute (set below Asana's
  per-token limits); a cooldown shared by the whole client after a `429`, waiting exactly the
  `Retry-After` Asana gave; retries of server errors and lost answers only when the operation is
  safe to repeat; one token refresh after a `401` for OAuth; lists as a lazy `Stream` of items or
  pages, with Asana's offsets passed back untouched.
- **One error type.** `asana_client::Error` says what happened (`NotFound`, `RateLimited`,
  `PremiumRequired`, `Decode` with the JSON path that failed, …). Its `Display` never includes a
  response body, a query value, a header or a token.
- **Asana's data is treated as loose.** Unknown enum values and unknown resource types are kept,
  not errors. Numbers in custom fields are `rust_decimal::Decimal`, read through the shortest
  text of the double Asana sends, so `1250.5` is never `1250.4999999999999`.

[`docs/guide.md`](docs/guide.md) walks through the common work; [`docs/design.md`](docs/design.md)
records the decisions and what was rejected.

## Webhooks

Asana calls a webhook's target with a secret *while* the request creating the webhook is still
open, then sends signed batches of compact events. `asana-client` establishes the webhook across
that handshake and compares the secrets; `asana-webhooks` verifies each delivery over its raw
bytes before parsing anything, and its `Receiver` gives every answer a receiver owes Asana; the
adapters mount that receiver in axum or actix-web and answer within Asana's 10 seconds by
queueing the batch for a worker. This is the start of the axum receiver in
[`crates/asana-webhooks-axum/examples/receiver.rs`](crates/asana-webhooks-axum/examples/receiver.rs),
which serves the route, then establishes a webhook that delivers to it:

```rust
let (queue, deliveries) = mpsc::channel::<Delivery>(1024);
let receiver = Receiver::new(MemoryHandshakeStore::new(), MemorySecrets::new(), queue);

// 2. Serve before establishing: the handshake arrives during the create request.
let app = router(&receiver, "/webhooks/asana");
let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await?;
let server = tokio::spawn(async move { axum::serve(listener, app).await });

// 3. Establish. `establish` makes a fresh route key, opens it in the same handshake store the
//    handler captures into, sends `POST /webhooks`, and checks the captured secret against the
//    one Asana returns. Keep the GID, the secret and the route; Asana never sends the secret
//    again.
let established = client
    .webhooks()
    .establish(resource, base_url)
    .filter(Filter::task().action(EventAction::Added))
    .filter(Filter::task().action(EventAction::Changed))
    .store(receiver.handshakes())
    .send()
    .await?;
receiver
    .secrets()
    .insert(&established.route, established.secret.clone());
```

Running webhooks in production (a handshake store shared by every instance, heartbeats, `410`,
the 24-hour deletion, at-most-once delivery and catching up from the events stream, latency) is
in [`docs/webhooks.md`](docs/webhooks.md).

## OAuth

`asana_client::oauth` covers the authorisation code flow with PKCE: the authorisation URL, reading
the redirect, exchanging and refreshing tokens, revoking and introspecting them, and working out
the scopes an app needs from the operations it calls (`scopes_for![GetTask, UpdateTask]`).
`RefreshingTokenSource` keeps a client's access token fresh over storage you provide. See
[`docs/oauth.md`](docs/oauth.md) and the `oauth_flow` example.

## Testing your code

`asana-fake` is an in-memory Asana that plugs in where the network would be, so a test can create
tasks, move them between sections and receive signed webhook deliveries without a token. For a
narrower test, any type implementing `asana_client::Transport` can answer the client's requests.
See [`docs/testing.md`](docs/testing.md).

## What is covered

All 271 operations in the pinned specification, in 52 groups, are generated and can be sent with
`client.send(..)`. The count comes from the generated index, `asana_openapi::endpoints::OPERATIONS`;
[`docs/coverage.md`](docs/coverage.md) lists every operation with its scope and retry behaviour,
and is printed by `cargo run -p asana-client --example coverage`.

Hand-written handles cover the common work: tasks (create, update, move, subtasks, followers,
tags, dependencies, duplicate), sections (including lookup by name and a cached directory),
projects, custom fields and their enum options, tags, users, workspaces and task search, stories
and comments, attachments (upload and download), task and project templates, jobs, the batch API,
the events stream, and webhooks (establish, health, list, delete, and the resolver that turns
events into work).

## Requirements and features

- Rust 1.89 or later (edition 2024), the `rust-version` in the root `Cargo.toml`.
- No cargo features in any crate: the whole API is always there, and every build of a crate is
  the same build.
- The default transport is `reqwest` with rustls, with redirects off. To use another HTTP stack,
  implement `asana_client::Transport`; to keep your own `reqwest` settings, pass your
  `reqwest::Client` to `ReqwestTransport::new`.

## The specification

The code is generated from Asana's OpenAPI document at commit
[`65bd6e7`](https://github.com/Asana/openapi/commit/65bd6e77a6692a62c03fefa6a340dd401795e5bc) of
[Asana/openapi](https://github.com/Asana/openapi) (committed 9 November 2026), vendored unmodified
in [`spec/`](spec/) with its source recorded in `spec/UPSTREAM`. Where the document is wrong, a
correction in `spec/overlays/` is applied before generating. A weekly workflow fetches Asana's
latest document, regenerates, and proposes the result as a pull request;
[`docs/generating.md`](docs/generating.md) explains the generator and how to update by hand.

## Licence

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.

The vendored specification, `spec/asana_oas.yaml`, is Asana's, published under the Apache License,
Version 2.0; [`spec/NOTICE`](spec/NOTICE) has the attribution. Asana is a trademark of Asana, Inc.
This project is not affiliated with or endorsed by Asana.

Unless you explicitly state otherwise, any contribution you submit for inclusion in this work, as
defined in the Apache-2.0 licence, is dual licensed as above, without any additional terms or
conditions.
