# asana-client

An async client for [Asana's REST API](https://developers.asana.com/reference/rest-api-reference).
Every operation in Asana's OpenAPI specification is a typed request (from
[`asana-openapi`](https://crates.io/crates/asana-openapi), re-exported as `asana_client::openapi`),
and this crate sends them all through one path: personal access tokens, service accounts and
OAuth with refresh; per-token limits on concurrent reads, writes and searches; retries that wait
exactly Asana's `Retry-After` and never resend a request that creates something; lists as a
stream; and one error type whose messages never hold a response body or a token.

On top of that are handles for the common work (`client.tasks()`, `sections()`, `projects()`,
`custom_fields()`, `stories()`, `attachments()`, `task_templates()`, `batch()`, `events()`,
`webhooks()`, …), typed custom field values, OAuth with PKCE, and the client side of webhooks:
establishing one across Asana's handshake, checking its health, and turning delivered events into
work.

```toml
[dependencies]
asana-client = "0.1"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

```rust
use asana_client::{Auth, Client};
use asana_client::openapi::endpoints::GetTask;

let client = Client::builder()
    .auth(Auth::personal_access_token("0/abc"))
    .user_agent("acme-sync/1.0")
    .build()?;
let task = client.send(GetTask::builder("1201234567890123".parse()?).build()).await?;
```

The same read through a handle, choosing fields (checked when it compiles) and moving the task if
it is not in a section yet:

```rust
use asana_client::fields;
use asana_client::resources::tasks::TaskExt;

let read = client
    .tasks()
    .get(task.clone())
    .fields(fields![Task: name, due_on, memberships.project.name, memberships.section.name])
    .send()
    .await?;
if !read.is_in_section(&section) {
    client.sections().add_task(section, task.clone()).send().await?;
}
client.tasks().update(task).clear_due_on().send().await?;
```

The [examples](https://github.com/noahbclarkson/asana-rs/tree/main/crates/asana-client/examples)
cover a quick start, custom fields, paging, search, batch, the events stream, attachments and
templates, OAuth, webhook health, and testing with a mock transport.

This is an unofficial client, not affiliated with Asana. It is part of
[asana-rs](https://github.com/noahbclarkson/asana-rs), whose `docs/` has the guide, the webhooks
guide, OAuth and testing. Licensed under MIT or Apache-2.0.
