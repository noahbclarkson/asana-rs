# Guide

A walk through the work most integrations do with `asana-client`. Every code block here is taken
from an example in [`crates/asana-client/examples/`](../crates/asana-client/examples/) or from a
doc test, both of which are compiled in CI; the file each comes from is named above it.

- [Authentication and the client](#authentication-and-the-client)
- [Limits and retries](#limits-and-retries)
- [Choosing fields](#choosing-fields)
- [Paging](#paging)
- [Custom fields](#custom-fields)
- [Sections and moving tasks](#sections-and-moving-tasks)
- [Stories and rich text](#stories-and-rich-text)
- [Attachments](#attachments)
- [Templates and jobs](#templates-and-jobs)
- [Batch](#batch)
- [Search](#search)
- [Events](#events)
- [Errors](#errors)
- [Calling any operation](#calling-any-operation)

## Authentication and the client

A `Client` holds one credential, one transport and the limits that go with that credential. It is
an `Arc` inside: clone it freely, and build one per token, because Asana counts its rate limits per
token.

The credential is an `Auth`:

| Constructor | For |
|---|---|
| `Auth::personal_access_token(token)` | A user's own token, from Asana's developer console. |
| `Auth::service_account(token)` | An Enterprise service account's token. |
| `Auth::oauth(source)` | A user who authorised your OAuth app; `source` is a `TokenSource`, usually `oauth::RefreshingTokenSource`. See [oauth.md](oauth.md). |

Tokens are held as `secrecy::SecretString` (re-exported as `asana_client::SecretString`) and never
appear in `Debug` output. Asana says tokens are opaque, so none is checked for format.

Only `auth` is required. Everything else has a default. From `client_setup.rs`:

```rust
// Your own reqwest client keeps your TLS, proxy and timeout choices. Turn redirects off:
// Asana's API does not redirect, and a redirect would carry the token elsewhere.
let http = reqwest::Client::builder()
    .redirect(reqwest::redirect::Policy::none())
    .connect_timeout(Duration::from_secs(5))
    .timeout(Duration::from_secs(60))
    .build()?;

let client = Client::builder()
    // Or `Auth::service_account(..)`, or `Auth::oauth(..)` with a token source.
    .auth(Auth::personal_access_token(env::var("ASANA_ACCESS_TOKEN")?))
    .transport(ReqwestTransport::new(http).max_body(8 * 1024 * 1024))
    // Sent as "acme-sync/2.1 asana-rs/<version>".
    .user_agent("acme-sync/2.1")
    // Up to 3 sends of one request, never waiting more than 30 seconds between them.
    .retry(
        RetryPolicy::builder()
            .max_attempts(3)
            .max_wait(Duration::from_secs(30))
            .build(),
    )
    // Below Asana's own per-token limits, leaving room for another tool on the same token.
    .limits(
        Limits::builder()
            .reads(20)
            .writes(5)
            .searches_per_minute(30)
            .build(),
    )
    // Opt in early to an announced API change (`Asana-Enable`).
    .asana_enable(["new_user_task_lists"])
    .build()?;
```

| Builder method | Default |
|---|---|
| `transport` | `ReqwestTransport::with_defaults()`: rustls, no redirects, a 10-second connect timeout, a 60-second total timeout, bodies up to 16 MiB. |
| `base_url` | `https://app.asana.com/api/1.0` (`DEFAULT_BASE_URL`). Point it at a test server. |
| `user_agent` | `asana-rs/<version>`; a name you give goes in front of it. |
| `retry` | `RetryPolicy::default()`: 5 attempts, waits of up to 60 seconds. `RetryPolicy::never()` sends once. |
| `limits` | `Limits::default()`: 40 concurrent reads, 12 concurrent writes, 55 searches a minute. |
| `asana_enable`, `asana_disable` | Nothing. Names of announced API changes, sent as `Asana-Enable` / `Asana-Disable`. |

When Asana announces a change on a response (`Asana-Change`), the client logs it once at `warn`
through `tracing` and keeps it; `client.observed_changes()` lists what it has seen. Each call is
also one `tracing` span, `asana.request`, at `debug`, with the operation, method, route template,
status, attempt and wait. The span never records a path value, a query value, a header or a body.

## Limits and retries

Asana limits each token: 150 requests a minute on a free plan and 1,500 on a paid one, 50
concurrent `GET`s and 15 concurrent writes counted separately, 60 task searches a minute, and a
cost limit on wide reads. It answers `429` with `Retry-After` when one is reached, and counts the
refused request against the quota too. The client's own limits keep it under the concurrency and
search ceilings, so most of the time requests wait on the client side instead of being refused:

- **Concurrency.** A `GET` takes a read permit and a `POST`, `PUT` or `DELETE` a write permit, for
  as long as it runs.
- **Search.** `searchTasksForWorkspace` takes a token from a bucket of `searches_per_minute`.
- **Batches.** Asana counts each action in a batch as a request of its own, so a batch takes a
  read or write permit for each action (at most the whole limit of a kind) and a search token for
  each search, and holds them while it runs.
- **Cooldown.** When any request gets a `429`, no request from that client (or any clone of it)
  starts before the time Asana gave. A request that would have to wait more than
  `RetryPolicy::max_wait` of it is not sent: it fails at once with `Error::RateLimited`, whose
  `retry_after` is how much is left and whose `attempts` is `0` when it was never sent. So
  `max_wait` bounds every wait for a rate limit, not only the call that saw the `429`.
  `RetryPolicy::never()` waits for none. A `Retry-After` over a day is read as a day.

The limits are per `Client`. Two clients on one token, or two processes, each think they have the
whole allowance; lower `Limits` when a token is shared.

When a request fails, the client decides whether to send it again:

| What happened | Retried? |
|---|---|
| `429 Too Many Requests` | Yes, for any operation, after exactly `Retry-After` (seconds, fractional seconds or an HTTP date). If `Retry-After` is longer than `RetryPolicy::max_wait`, the call ends with `Error::RateLimited` at once, and so does every other call on the client until less than `max_wait` of the cooldown is left. |
| `500`, `502`, `503`, `504` | Only when sending twice is safe: `GET`, `PUT`, `DELETE`, and the `POST`s the generator marks idempotent (adding or removing tags, followers, projects, dependencies and members, `addTaskForSection`, `setParentForTask`). Jittered back-off from half a second, doubling, at most 30 seconds and `max_wait`. |
| No connection was made | Yes, for any operation: the request never reached Asana. |
| A timeout or other transport failure | Only when sending twice is safe, as for a `5xx`: Asana may have acted. A `DELETE` resent after one and answered `404` counts as deleted. |
| A response body over the transport's cap | Never. |
| `401` with `Auth::oauth` | Once, at once, after telling the token source the token was rejected; this retry happens even with `RetryPolicy::never()`. |
| Anything else (`400`, `402`, `403`, `404`, `451`) | Never. |

So a `POST` that creates something (a task, a comment, an attachment, a webhook, a template
instantiation) is sent once unless Asana certainly never received it. If such a call fails with a
timeout, read before you write again. Every handle method's documentation says which operation it
sends, its scope and whether it is retried; [coverage.md](coverage.md) has the same for every
operation.

## Choosing fields

Asana returns a compact record (little more than `gid` and `name`) unless the request names the
fields it wants in `opt_fields`. `fields!` names them, checked when the code compiles against the
paths Asana's specification lists for that resource, so `fields![Task: nmae]` is a compile error.
Nested paths work as Asana writes them: `memberships.section.name`.

Every field of a generated model is an `Option`, because what comes back depends on what was
asked for. `Require` turns a missing field into `Error::MissingField` instead of a panic. From
`quickstart.rs`:

```rust
// Read a task with the fields you need. A misspelt field does not compile.
let read = client
    .tasks()
    .get(task)
    .fields(fields![Task: name, completed, due_on, memberships.section.name])
    .send()
    .await?;
// Every field of a model is an `Option`; `require` turns a missing one into an error.
println!("read {:?}", read.name.require("name")?);
```

For a field Asana has added since the crate was generated, `Fields::with_raw("new_field")` sends a
path as written. Asana answers `403` when a field needs a scope the token lacks.

## Paging

A list operation returns pages of up to 100 items with an opaque `offset` for the next one. A
request for a list can be read four ways. From `paging.rs`:

```rust
// Every task, fetched 100 at a time as the stream is read. The stream ends after its first
// error, so `?` on each item is enough.
let mut tasks = client
    .projects()
    .tasks(project.clone())
    .fields(fields![Task: name, completed])
    .stream();
let mut open = 0;
while let Some(task) = tasks.next().await {
    if task?.completed == Some(false) {
        open += 1;
    }
}
println!("{open} open tasks");

// A page at a time, when the work is done per page.
let mut pages = client.sections().list(project.clone()).pages();
while let Some(page) = pages.try_next().await? {
    println!(
        "a page of {} sections (last: {})",
        page.items.len(),
        page.is_last()
    );
}

// The first few, fetching no more pages than that needs.
let first_five = client
    .tasks()
    .list()
    .project(project.clone())
    .collect_max(5)
    .await?;
println!("first five: {}", first_five.len());

// One page now and the next one later: keep the offset (it is opaque text, and it expires).
let saved: Option<Offset> = env::var("ASANA_OFFSET").ok().map(Offset::new);
let page = client
    .send_page(
        GetTasksForProject::builder(project)
            .limit(PageLimit::new(20))
            .maybe_offset(saved)
            .build(),
    )
    .await?;
```

`StreamExt` and `TryStreamExt` come from `futures-util`. The streams are lazy (no request until
read), `'static` and `Send`, so they can be moved into a task. They end at the first error, and
where Asana offers no next page; a page with no items that does offer one is followed. The audit
log is the exception: it offers a next page for ever, to stream future events, so its streams end
at its first page with no items. Offsets are passed back exactly as Asana gave them; they expire,
so saving one for long only works for a while.

The same methods are on `Client` for any generated descriptor that pages: `client.stream(..)`,
`client.pages(..)`, `client.collect_max(.., n)` and `client.send_page(..)`.
[coverage.md](coverage.md) marks which operations page.

## Custom fields

A custom field's value comes back in a member named after its kind (`text_value`, `number_value`,
`enum_value`, …) and is written back as one JSON value per field GID. `CustomFieldValue` is that
value as one enum, for reading and for writing. Numbers are `rust_decimal::Decimal`. Asana sends
and stores them as doubles, so a number reads as the shortest text of the double Asana sent
(`1250.5`, not `1250.4999999999999`), and is written as an integer when it is one and otherwise as
the nearest double: about 15 significant digits survive, and
`asana_client::openapi::number::fits_a_double` says whether a value does. From `custom_fields.rs`:

```rust
// `custom_fields` brings every value member; `TaskCustomFields` reads them by the field's
// kind, so a number is a `Decimal`, an enum an option, a date a date.
let read = client
    .tasks()
    .get(task.clone())
    .fields(fields![Task: name, custom_fields])
    .send()
    .await?;
for field in read.custom_fields_typed() {
    match &field.value {
        CustomFieldValue::Number(amount) => println!("{}: the number {amount}", field.gid),
        CustomFieldValue::Enum(option) => {
            println!("{}: the option {:?}", field.gid, option.name);
        }
        CustomFieldValue::Empty => println!("{}: not set", field.gid),
        // Every kind has a variant; `display_value` is Asana's own text for any of them.
        _ => println!("{}: {:?}", field.gid, field.display_value),
    }
}
if let Some(current) = read.custom_field(&number).and_then(|f| f.value.as_number()) {
    println!("the number field holds {current}");
}

// An enum is set by option GID. Find the option by name, creating it if the field has none.
let approved = client
    .custom_fields()
    .ensure_option(&stage, "Approved")
    .await?
    .gid
    .require("gid")?;

// Each value is written in the JSON shape Asana takes for its kind.
client
    .tasks()
    .update(task.clone())
    .custom_field(text, CustomFieldValue::text("Acme Ltd"))
    .custom_field(number, CustomFieldValue::number(Decimal::new(125_050, 2))) // sent as 1250.5
    .custom_field(stage, CustomFieldValue::enum_option(approved))
    .custom_field(due.clone(), CustomFieldValue::date(date!(2026 - 12 - 17)))
    .send()
    .await?;
```

What each value is written as:

| Value | JSON sent |
|---|---|
| `CustomFieldValue::clear()` (`Empty`) | `null`, which clears a field of any kind |
| `text("…")` | a string |
| `number(..)` | a JSON number: an integer when it is one, otherwise the nearest double (`1250.50` is sent as `1250.5`) |
| `enum_option(gid)` | the option's GID |
| `multi_enum([..])`, `people([..])`, `reference([..])` | an array of GIDs; an empty one clears |
| `date(day)` | `{"date": "2026-12-17"}` |
| `date_time(moment)` | `{"date_time": "2026-12-17T02:30:00.000Z"}`, in UTC |
| `raw(json)` | the JSON as given, for a kind this crate does not know |

Lookups by name (`option_by_name`, `ensure_option`, `tags().find_by_name`, `tags().ensure`,
`sections().find_by_name`) trim both names and compare case and all, and return a `NameMatch`
(`Missing`, `One`, `Several`) where several objects can share a name, because Asana does not keep
names unique. `ensure_option` and `tags().ensure` read again after a failed create, so two callers
racing to make the same option both end up with one.

Reading by name, `task.custom_field_named("Stage")`, takes the first field with exactly that name;
prefer `custom_field(&gid)` when you know the GID. `TaskCustomFields` works on `Project` too.

## Sections and moving tasks

A task can be created straight into a section, moved between sections of a project, and placed
at either end or beside another task. From `create_and_move_task.rs`:

```rust
// Create the task in "Planned", followed by the token's own user.
let task = client
    .tasks()
    .create()
    .section(project.clone(), planned)
    .name("Write the release notes")
    .follower(UserRef::Me)
    .send()
    .await?
    .gid
    .require("gid")?;

// Moving between sections of one project: add the task to the section.
client
    .sections()
    .add_task(in_progress.clone(), task.clone())
    .send()
    .await?;

// Adding a task to a project it is already in moves it; this puts it first in the section.
client
    .tasks()
    .add_project(task.clone(), project.clone())
    .section(in_progress.clone())
    .at_start()
    .send()
    .await?;
```

`sections().add_task` and `tasks().add_project` are retried after a server error, because sending
either twice leaves the task where once would. `create()` is not.

To read where a task is, ask for `memberships` and use `TaskExt` (`section_in(&project)`,
`sections()`, `is_in_section(&section)`, `permalink()`):

```rust
// Webhook events and moves can race; read the task back before trusting where it is.
let now = client
    .tasks()
    .get(task.clone())
    .fields(fields![Task: name, permalink_url, memberships.project, memberships.section.name])
    .send()
    .await?;
```

Sections by name: `client.sections().find_by_name(project, "Planned").send().await?` lists the
project's sections once and returns a `NameMatch<Section>`. Code that looks sections up on every
webhook delivery keeps a `SectionDirectory`, which lists them at most once per time-to-live
(from its doc test):

```rust
use std::time::Duration;

let directory = client.sections().directory(project, Duration::from_secs(300));
if let Some(section) = directory.by_name("Send Documents").await?.one() {
    println!("{section}");
}
```

## Stories and rich text

Comments are stories. `stories().comment(task, text)` takes plain text (shown as written, markup
included) or an `Html` document, which escapes everything passed to it as text, makes links only
to `http`, `https` and `mailto` addresses, and writes mentions in Asana's own form. From
`stories_and_files.rs`:

```rust
// Rich text: everything passed as text is escaped, links are kept to http(s) and mailto, and
// a mention notifies the user without making them a follower.
let comment = Html::paragraph("Figures for ")
    .bold("Acme Ltd")
    .text(" are ready, ")
    .mention(&me)
    .next_paragraph("Sources:")
    .list([
        Html::new().link("https://example.com/report", "the report"),
        Html::from("last month's notes"),
    ]);
client
    .stories()
    .comment(task.clone(), comment)
    .send()
    .await?;

// Asana lists stories oldest first; `latest` reads the newest without walking them all.
for story in client
    .stories()
    .latest(task.clone(), 5)
    .comments_only()
    .fields(fields![Story: text, created_at, created_by.name])
    .send()
    .await?
{
    if story.is_comment() {
        println!("{:?}: {:?}", story.created_at, story.text);
    }
}
```

`Html::raw_trusted` takes markup as it is, unchecked; never pass it text a user wrote, because
Asana refuses malformed markup and unknown tags with a `400`. A comment is never retried after an
unknown outcome, since a second send would post it twice.

## Attachments

An upload is a `multipart/form-data` request to a task, project or project brief. A download is
not an API call: the client reads the attachment's short-lived `download_url` and fetches it
without sending the token, following at most three `https` redirects and refusing a body over
100 MiB unless told otherwise (`.max_bytes(n)`). From `stories_and_files.rs`:

```rust
// An upload is multipart; it is never retried after an unknown outcome.
let attachment = client
    .attachments()
    .upload(
        task.clone(),
        Upload::bytes(
            "acme-figures.csv",
            "month,total\n2026-10,1250.50\n",
            "text/csv",
        ),
    )
    .send()
    .await?
    .gid
    .require("gid")?;
// A download reads the short-lived `download_url` and fetches it without the token.
let bytes = client.attachments().download(attachment).send().await?;
println!("downloaded {} bytes", bytes.len());
```

The transport reads a whole body before answering, so its own cap (16 MiB for
`ReqwestTransport::with_defaults`, `ReqwestTransport::max_body` to change it) also bounds a
download. `attachments().attach_url(parent, url, name)` attaches a link to a file kept elsewhere.

## Templates and jobs

Instantiating a template, duplicating a task or project, and exporting answer at once with a job
and carry on in the background. The answer already names the object being made, but it may lack
subtasks, fields or attachments until the job succeeds. `JobHandle::wait` polls `GET /jobs/{gid}`
(half a second, doubling to five seconds) until the job ends or the timeout passes. From
`stories_and_files.rs`:

```rust
// Templates answer with a job; `wait_for_task` polls it until the task is fully made.
if let Ok(template) = env::var("ASANA_TASK_TEMPLATE_GID") {
    let template: TaskTemplateGid = template.parse()?;
    let job = client
        .task_templates()
        .instantiate(template)
        .name("Month end: November")
        .send()
        .await?;
    // The GID is known at once; the task's subtasks and fields may still be arriving.
    println!("making {:?}", job.new_task_gid());
    let made = job.wait_for_task(&client, Duration::from_secs(60)).await?;
    println!("made {:?}", made.gid);
}
```

`wait` ends with `Error::JobFailed` when the job fails and `Error::JobTimedOut` when it is still
running at the timeout (it may still finish; wait again). Asana runs at most five such jobs at a
time per user, the user's own work in the web app included. `project_templates().instantiate`
and `tasks().duplicate` return the same kind of job.

## Batch

`POST /batch` carries up to ten operations in one request. Any descriptor can go in a batch;
`push` returns a ticket typed by the descriptor, and `take` redeems it for that action's own
result or error. From `batch.rs`:

```rust
let mut batch = client.batch();
// Any descriptor can go in: one taken from a handle's request with `into_endpoint`, or a
// generated one built directly.
let me = batch.push(client.users().me().into_endpoint())?;
let mut tagged = Vec::new();
for task in &tasks {
    let request = client.tasks().add_tag(task.clone(), tag.clone());
    // The eleventh push is refused with `Error::BatchFull`, before anything is sent.
    tagged.push((task, batch.push(request.into_endpoint())?));
}
let first = tasks.first().ok_or("name at least one task")?;
let read_back = batch.push(
    GetTask::builder(first.clone())
        .opt_fields(fields![Task: name, tags.name])
        .build(),
)?;

// One request. A 429 is waited out and retried like any call; one action failing does not
// fail the others, and each result is read with its own ticket.
let mut results = batch.send().await?;
println!("as {:?}", results.take(me)?.name);
for (task, ticket) in tagged {
    match results.take(ticket) {
        Ok(_) => println!("tagged {task}"),
        Err(error) => println!("{task}: {error} (status {:?})", error.status()),
    }
}
```

Asana runs the actions in parallel, in no promised order, and answers in the order they were
pushed. Each action counts against the rate limits as a request of its own, and if any would be
limited the whole batch is answered `429`. After a server error or a lost answer the batch is
sent again only when every action in it is safe to repeat. An upload cannot go in a batch, nor
can a batch.

## Search

`workspaces().search_tasks(workspace)` is Asana's advanced search, with a setter for each filter
(`text`, `projects_any`, `sections_any`, `assignee_any`, `completed`, `due_on_before`, …) and
`custom_field(&gid, CustomFieldFilter::…, value)` for custom field filters. It has limits the
other lists do not:

- **Paid plans only.** Asana answers `402`, which is `Error::PremiumRequired`.
- **Eventually consistent.** A change reaches search 10 to 60 seconds after it is made. For
  read-your-write, list a project's or section's tasks instead.
- **At most 100 results a call, with no offset.** Page by creation time: sort by `created_at`
  ascending and start the next call after the last task seen.
- **60 searches a minute** per token; the client's search bucket keeps under it.

From `search.rs`:

```rust
// Oldest first, with `created_at`, so the next call can start after the last one seen.
let mut search = client
    .workspaces()
    .search_tasks(workspace.clone())
    .text(text.clone())
    .completed(false)
    .sort_by(SearchTasksForWorkspaceSortBy::CreatedAt)
    .sort_ascending(true)
    .limit(100)
    .fields(fields![Task: name, created_at]);
if let Some(field) = &over {
    search = search.custom_field(field, CustomFieldFilter::GreaterThan, "1000");
}
if let Some(moment) = after {
    search = search.created_at_after(moment);
}

let tasks = match search.send().await {
    Ok(tasks) => tasks,
    Err(Error::PremiumRequired(_)) => {
        println!("search needs a paid Asana plan in this workspace");
        return Ok(());
    }
    Err(other) => return Err(other.into()),
};
```

Asana does not say whether `created_at.after` is inclusive, so the example drops tasks it has
already seen.

## Events

The events API reports what changed on a task, project or goal since a sync token, as the same
`asana_webhooks::Event` type a webhook delivers. `client.events().stream(&resource, token)` reads
everything since the token and ends; `poll(.., every)` keeps asking. The first call without a
token is answered with a fresh token and no events; a token that has expired (Asana keeps events
for a limited time) is `Error::SyncTokenExpired`, carrying a fresh token: events were lost, so
read the resource again in full. From `events_poll.rs`:

```rust
let mut items = client
    .events()
    .poll(&project, saved, Duration::from_secs(10));
while let Some(item) = items.next().await {
    match item {
        Ok(EventsItem::Event { event, sync }) => {
            // The same `Event` type a webhook delivers, with the same helpers.
            if let Some((task, section)) = event.section_entered() {
                println!("task {task} entered section {section}");
            } else {
                println!("{:?} on {:?}", event.action, event.resource.gid);
            }
            // Save after handling: a restart may repeat an event, never skip one.
            std::fs::write(&file, sync.as_str())?;
        }
        Ok(other) => std::fs::write(&file, other.sync().as_str())?,
        Err(Error::SyncTokenExpired { sync }) => {
            // Events were lost: read the project again in full, then go on from here.
            println!("the sync token expired; re-read the project");
            std::fs::write(&file, sync.as_str())?;
        }
        // Rate limits and server failures were already retried by the client.
        Err(other) => return Err(other.into()),
    }
}
```

[webhooks.md](webhooks.md) shows the events stream as the catch-up path for a webhook receiver.

## Errors

Every call returns `asana_client::Error`. Match on the variant for what you handle and let the
rest go up. From `client_setup.rs`:

```rust
match client
    .tasks()
    .get(task)
    .fields(fields![Task: name])
    .send()
    .await
{
    Ok(read) => println!("{:?}", read.name),
    // Asana's own messages are kept to read on purpose; `Display` and `Debug` never print them.
    Err(Error::NotFound(errors)) => {
        println!("no such task: {:?}", errors.messages().collect::<Vec<_>>());
    }
    Err(Error::Forbidden(_)) => println!("the token cannot see that task"),
    // Every send already waited out `Retry-After`; this is after the last attempt.
    Err(Error::RateLimited {
        retry_after,
        attempts,
    }) => {
        println!("still limited after {attempts} sends; Asana asks for {retry_after:?}");
    }
    // Never true for a create that may have been carried out: look for it before sending again.
    Err(other) if other.is_retryable() => println!("worth trying later: {other}"),
    Err(other) => return Err(other.into()),
}
```

| Variant | When |
|---|---|
| `BadRequest`, `Unauthorized`, `PremiumRequired`, `Forbidden`, `NotFound`, `LegalBlock` | Asana answered `400`, `401` (after the one OAuth refresh), `402`, `403`, `404`, `451`. Each holds `ApiErrors`, Asana's messages. |
| `RateLimited { retry_after, attempts }` | Still `429` after the retry policy, a `Retry-After` longer than `max_wait`, or more than `max_wait` left of the client's cooldown from an earlier `429` (`attempts: 0` when it was never sent). |
| `Server { status, phrase, attempts, safe_to_repeat }` | A `5xx` after the retries the operation allows. `phrase` is what Asana's support asks for. |
| `Transport { source, attempts, safe_to_repeat }` | No response came back. `source.kind()` says whether a connection was made. |
| `Auth(AuthError)` | No usable token: `Revoked` means the person must connect again. |
| `SyncTokenExpired { sync }` | The events API's `412`; start again from `sync`. |
| `Decode { operation, path, .. }` | A response did not have the shape the specification describes; `path` is the JSON path that failed, such as `data.memberships[0].section`. |
| `MissingField(name)` | `Require` found a field missing, or a response lacked one a helper needed. |
| `JobFailed`, `JobTimedOut` | From `JobHandle::wait`. |
| `BatchFull`, `InvalidRequest`, `Encode` | The request could not be made: an eleventh batch action, a list with no filter, a body that would not serialise. Nothing was sent. |
| `UnexpectedStatus { status, errors }` | Any other status. |

`status()` gives the HTTP status when there was one, `is_not_found()` tests for `404`, and
`api_errors()` gives Asana's messages. `outcome()` says what became of the request at Asana:
`NotSent` (it never got there), `Refused` (Asana answered and did nothing), `Unknown` (a `5xx`, a
timeout or a lost connection: Asana may have carried it out) or `Applied` (it was carried out and
what came after failed, such as reading the answer). `is_retryable()` says whether sending the same
request again later could succeed without risk; it is never true for a request that creates
something when its outcome is `Unknown`, because a second send could create a second one, so look
for what it would have made first. `safe_to_repeat` on `Server` and `Transport` says which kind of
request it was. `Error` is `#[non_exhaustive]`, so a `match` needs a catch-all arm.

No `Display` or `Debug` of an error in this crate includes a response body, Asana's messages, a
query value, a header or a token, because those carry customer data and `Debug` is what
`{error:?}`, `tracing`'s `?error` and a `main` that returns `Result` print. `Debug` of `ApiErrors`
writes `<redacted>` for each message and help text, and `Debug` of a `SyncToken` hides the token.
Asana's messages are in `ApiErrors` for a developer to read on purpose (`api_errors()`, then
`messages()`), and `ApiError::phrase`, which `Debug` keeps, is what Asana's support asks for on a
`500`.

## Calling any operation

Handles cover the common work. Every other operation is a descriptor in
`asana_client::openapi::endpoints` (the type is the operation's `operationId` in UpperCamelCase,
as [coverage.md](coverage.md) lists them), built with its builder and sent with `client.send`.
From the `Client` doc test:

```rust
use asana_client::{Auth, Client};
use asana_client::openapi::endpoints::GetTask;

let client = Client::builder()
    .auth(Auth::personal_access_token("0/abc"))
    .user_agent("acme-sync/1.0")
    .build()?;
let task = client.send(GetTask::builder("1201234567890123".parse()?).build()).await?;
```

A handle's `Request` also hands its descriptor over: `endpoint_mut()` and `map(|e| ..)` set a
parameter no setter covers, and `into_endpoint()` returns it, for a batch. The OAuth scope an
operation needs is its `Endpoint::SCOPE`; `asana_client::oauth::required_scope::<E>()` reads it.
