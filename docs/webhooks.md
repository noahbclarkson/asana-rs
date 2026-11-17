# Webhooks in production

How Asana's webhooks behave, what each part of this workspace does about it, and what a receiver
must still do itself. Code blocks are taken from doc tests and from compiled examples, named above
each; the complete receivers are
[`crates/asana-webhooks-axum/examples/receiver.rs`](../crates/asana-webhooks-axum/examples/receiver.rs)
and
[`crates/asana-webhooks-actix/examples/receiver.rs`](../crates/asana-webhooks-actix/examples/receiver.rs).

The pieces:

| Crate | Part |
|---|---|
| `asana-webhooks` | The protocol, with no HTTP client and no framework: `classify` a request, the `HandshakeStore` and `settle`, `verify` a signature over the raw body, decode an `EventBatch`, `Filter`, `Dedupe`; and `Receiver`, which puts them together and gives every answer, with its collaborators `SecretLookup` (`MemorySecrets`), `Sink`, `Delivery` and `Reply`. |
| `asana-client` (`asana_client::webhooks`) | Establishing a webhook across the handshake, `health`, `list`, `delete`, `update_filters`; the `Resolver` that turns events into the few shapes a receiver acts on; the `SectionWatcher`. |
| `asana-webhooks-axum`, `asana-webhooks-actix` | Mount a `Receiver` in each framework: they read the route segment, the headers and the raw body (with a size cap) into it and write its `Reply` back. Both re-export the receiver types. |

## The handshake, and why the store is shared

Creating a webhook is one request that cannot finish on its own. While the creator's
`POST /webhooks` is open, Asana calls the target URL with a new secret in `X-Hook-Secret`; only
once the target has answered `200` echoing it does the create return `201`, with the same secret
in its body. Asana never returns the secret again.

Behind a load balancer the handshake can reach a different instance from the one waiting on the
create, so the two meet in a `HandshakeStore` every instance can reach:

```text
 expect ──▶ Expected ──capture──▶ Captured ──take, settle: same secret──▶ Established
               │                     │
               └─ttl passes─▶ Expired └──take, settle: secret differs──▶ Rejected
```

- The creator makes a fresh `RouteKey` (32 random bytes, 43 base64url characters), calls
  `expect`, and sends the create with the key as the target's last path segment:
  `https://example.com/webhooks/asana/{route_key}`.
- The receiver parses the key from the path, calls `capture` with the secret, and answers with the
  reply it returns. A route nobody opened is refused, so a stranger cannot attach a webhook to
  your receiver.
- When the create returns, the creator calls `take` and `settle`s the captured secret against
  the one in the response, in constant time. A mismatch, or nothing captured, means something
  else answered the handshake: the webhook is deleted.

`MemoryHandshakeStore` is enough only when one process both creates and receives. Otherwise,
implement the trait's three methods over a table keyed by `RouteKey::digest()` (the SHA-256 of
the key, so a leaked table does not list live URLs). `capture` must be safe to call twice with
the same secret, because Asana may retry the handshake.

`client.webhooks().establish(..)` does the creator's side: route, `expect`, the create, `take`,
`settle`, and a delete of the webhook if anything fails after Asana made it. From its doc test:

```rust
use asana_client::{Auth, Client};
use asana_client::openapi::ids::ProjectGid;
use asana_webhooks::{EventAction, Filter, MemoryHandshakeStore};

let client = Client::builder()
    .auth(Auth::personal_access_token("0/made-up"))
    .build()?;
// Shared with the receiver; across instances, a store over a database instead.
let store = MemoryHandshakeStore::new();
let project: ProjectGid = "1201234567890456".parse()?;

let established = client
    .webhooks()
    .establish(project, "https://hooks.example.com/asana")
    .filter(Filter::task().action(EventAction::Added))
    .filter(Filter::task().action(EventAction::Changed).fields(["completed", "due_on"]))
    .store(&store)
    .send()
    .await?;

// Keep all three: the GID to manage it, the secret (encrypted) to verify deliveries, and
// the route the receiver finds the secret by.
let _ = (&established.webhook.gid, established.secret.expose(), established.route.digest());
```

Things that follow from the protocol:

- **The receiver must be serving before `establish` is sent**, since the handshake arrives during
  it. Both example receivers start the server first.
- **The target must be `https`**, and Asana refuses `localhost` targets with `403`; use a tunnel
  in development. The full URL, query included, is used, so `establish_at(resource, url, route)`
  takes a target you shape yourself as long as it contains the route key.
- **The create is slow.** It lasts as long as Asana's call to your receiver, so `establish` gives
  it a timeout of its own (90 seconds, `Establish::timeout`). A custom transport that ignores the
  per-request `RequestTimeout` needs a total timeout of at least a minute.
- **Filters.** An event is delivered if it passes any filter. `fields` is only allowed with
  `action: changed`. Webhooks on a workspace, team, portfolio or goal must have filters and never
  see task, subtask or story events; `asana_webhooks::validate_filters` checks those rules for a
  resource type, and `establish` checks them before sending.

## Verifying a delivery

A delivery's `X-Hook-Signature` is the hex HMAC-SHA256 of the request body under the webhook's
secret. It covers the body's exact bytes, so it must be checked on the bytes as received, before
any framework or middleware parses, re-encodes or trims them, and before your code parses
anything. `verify` takes the raw bytes and returns a `VerifiedBody`; only that can be decoded,
so an unverified body cannot reach the JSON parser by mistake. The comparison is constant time.
The `Receiver` (and so both adapters) reads the raw body with a size cap (256 KiB by default)
and verifies it first; a bad signature is answered `401`, never a success, because Asana never
sends one.

The protocol on its own, without a framework, is the `asana_webhooks` crate-level doc test: a
`receive` function that classifies, captures a handshake, verifies and queues a delivery, and
refuses a forged one.

## Answering within 10 seconds

A delivery not answered `2xx` within 10 seconds is retried with exponential back-off for up to 24
hours. So a receiver verifies, records or queues the batch, answers `200`, and does the work
afterwards. `asana_webhooks::Receiver` takes a `Sink` for exactly this; a bounded
`tokio::sync::mpsc::Sender<Delivery>` is one, queueing with `try_send` so a full queue answers
`503` (and Asana retries) instead of making Asana wait. It also takes the `HandshakeStore` and a
`SecretLookup`, which finds an established webhook's secret by its route (`MemorySecrets` in one
process; across instances, a table keyed by `RouteKey::digest()` with the secrets encrypted at
rest). From the `asana-webhooks-axum` doc test:

```rust
let (queue, mut deliveries) = mpsc::channel::<Delivery>(1024);
let receiver = Receiver::new(MemoryHandshakeStore::new(), MemorySecrets::new(), queue);

// Targets are `https://example.com/webhooks/asana/{route_key}`.
let app: axum::Router =
    axum::Router::new().merge(asana_webhooks_axum::router(&receiver, "/webhooks/asana"));

// Once a webhook is established, record its secret under its route:
// `receiver.secrets().insert(&established.route, established.secret)`.

// A worker, apart from the request, does the work.
tokio::spawn(async move {
    while let Some(delivery) = deliveries.recv().await {
        for event in &delivery.batch.events {
            let _ = event.section_entered(); // re-read the task before acting on it
        }
    }
});
```

and from the `asana-webhooks-actix` doc test, the same receiver as an actix-web service
(`configure(&receiver, path)` does the same through `App::configure`):

```rust
let (queue, mut deliveries) = mpsc::channel::<Delivery>(1024);
let receiver = Receiver::new(MemoryHandshakeStore::new(), MemorySecrets::new(), queue);

// Targets are `https://example.com/webhooks/asana/{route_key}`. In a server, build the
// receiver once, outside `HttpServer::new`, and clone it into each worker's `App`.
let app = test::init_service(App::new().service(resource(&receiver, "/webhooks/asana"))).await;
```

When establishing, pass the receiver's own store, `.store(receiver.handshakes())`, so the
handshake the receiver captures is the one `establish` takes, and afterwards record the secret
with `receiver.secrets().insert(&established.route, established.secret.clone())`; both complete
receivers in the adapters' `examples/` do this. A channel in memory loses what it holds if the
process stops; across restarts and instances, a sink writes to a durable queue or table and a
worker reads from that.

What the `Receiver` answers (its module documentation has the same table):

| The call | The answer |
|---|---|
| the path segment is not a route key | `404` |
| a body over the limit (256 KiB unless set) | `413`, read no further than the limit |
| both Asana headers, or an empty secret | `400` |
| a handshake the store captured | `200`, `X-Hook-Secret` echoed |
| a handshake the store refused | `404`, `409` or `503` |
| neither header, or a malformed or wrong signature | `401` |
| a delivery on a route the `SecretLookup` does not know | `404`, which Asana retries |
| a delivery on a route the lookup says is retired | `410`: Asana deletes the webhook |
| the lookup fails | `503`, which Asana retries |
| a verified body that is not an event batch | `400` (`413` for too many events) |
| a heartbeat | `200`, without reaching the sink |
| a batch the sink took | `200` |
| a batch the sink is too busy for | `503`, which Asana retries |

Neither the secret, the signature nor the body is ever logged.

## Heartbeats

A delivery with no events arrives right after the handshake and about every eight hours. Answer
it `200` like any other (`EventBatch::is_heartbeat`); the adapters do, without calling the sink.
Heartbeats count as deliveries for the 24-hour rule below, so a receiver that only answers when
there are events will lose its webhook.

## `410 Gone`

Answering `410` tells Asana to delete the webhook. Answer it only for a route you have decided
you no longer want, never for one you failed to process or do not recognise: a lost row or a
deploy pointing at the wrong database must not delete live webhooks. That is why a
`SecretLookup` returns `RouteSecret::Active(secret)`, `RouteSecret::Retired` (answered `410`) or
`RouteSecret::Unknown` (answered `404`, which Asana retries), and never infers `Retired`.

## The 24-hour deletion, and watching health

After 24 hours without a successful delivery (heartbeats included) Asana deletes the webhook. A
webhook also goes away when its resource is deleted (after 72 hours) and when the token that
made it is deactivated; one whose creator loses access to the resource stays but delivers
nothing. So check webhooks on a schedule and establish again any that are gone.
`client.webhooks().health(&gid)` reads the delivery fields Asana keeps (`last_success_at`,
`last_failure_at`, `delivery_retry_count`, `next_attempt_after`, `failure_deletion_timestamp`).
From `crates/asana-client/examples/webhook_health.rs`:

```rust
let mut listing = client.webhooks().list(workspace);
if let Ok(project) = env::var("ASANA_PROJECT_GID") {
    listing = listing.resource(project.parse::<ProjectGid>()?);
}
let webhooks: Vec<_> = listing.stream().try_collect().await?;

for webhook in webhooks {
    let Some(gid) = webhook.gid else { continue };
    // One `GET /webhooks/{gid}` with the delivery fields. A webhook Asana has already
    // deleted is `Error::NotFound`: establish it again.
    let health = client.webhooks().health(&gid).await?;
    match health.will_be_deleted_at() {
        Some(deadline) => println!("{gid}: failing, deleted at {deadline} unless it recovers"),
        None if health.is_failing() => println!("{gid}: a delivery failed"),
        None => println!("{gid}: healthy, last success {:?}", health.last_success_at),
    }
```

`last_failure_content()` is what your receiver last answered on a failure, as Asana recorded
it; it is untrusted text and is kept out of `Debug`. `client.webhooks().delete(&gid)` treats an
already deleted webhook as deleted. Asana allows 1,000 webhooks per resource and 10,000 per
token.

## At most once: catching up with the events stream

Asana delivers each event at most once. A delivery it gives up on after 24 hours is lost, and so
is everything while a webhook is deleted and not yet re-established. The events API is the
catch-up path: it reports what changed on a resource since a sync token, as the same `Event` type
a webhook delivers. Keep a sync token per watched resource, read the stream after an outage or
on a slow schedule, and feed the events through the same `Resolver` with
`accept_events`, whose dedupe store drops what the webhook already delivered. The guide's
[Events](guide.md#events) section and `crates/asana-client/examples/events_poll.rs` show the
stream. A sync token expires after a while (`Error::SyncTokenExpired`): then events were lost, so
read the resource again in full.

## From events to work: the resolver

Events are compact (the resource's GID and type, the parent for an `added` or `removed`, the
changed field's name), can arrive minutes late, can repeat when Asana retries, and include your
own integration's writes. Asana expects integrations to re-read the resource. The `Resolver`
drops events by your own user, events older than an age limit, and repeats (by `Event::key`,
through a `Dedupe` store; `MemoryDedupe` in one process), then coalesces what is left, the
newest event winning within each group: `SectionEntered` or `SectionLeft` per task and section,
`TaskAdded` or `TaskRemoved` per task and project, one `TaskChanged` per task with every changed
field, `StoryAdded` per story, `Deleted`, or `Other`. From
`crates/asana-webhooks-axum/examples/receiver.rs`:

```rust
/// Takes deliveries off the queue and acts on them, one at a time.
async fn work(client: Client, me: UserGid, mut deliveries: mpsc::Receiver<Delivery>) {
    // Remembers event keys for a day, the longest Asana retries a delivery.
    let dedupe = MemoryDedupe::new();
    let resolver = Resolver::builder()
        .ignore_user(me)
        .max_age(Duration::from_secs(60 * 60))
        .dedupe(&dedupe)
        .build();

    while let Some(delivery) = deliveries.recv().await {
        for unreadable in &delivery.batch.unreadable {
            eprintln!(
                "event {} could not be read: {}",
                unreadable.index, unreadable.reason
            );
        }
        let fresh = match resolver
            .accept(&delivery.batch, OffsetDateTime::now_utc())
            .await
        {
            Ok(fresh) => fresh,
            Err(error) => {
                eprintln!("the dedupe store failed: {error}");
                continue;
            }
        };
        // The newest event per task and section, per task and project, and per task's fields.
        for change in resolver.coalesce(fresh) {
            if let Err(error) = act(&client, change).await {
                eprintln!("acting on a change failed: {error}");
            }
        }
    }
}

/// Acts on one change, reading the task as it is now first: events are compact and can be
/// minutes old, and the task may have moved again since.
async fn act(client: &Client, change: Coalesced) -> Result<(), asana_client::Error> {
    match change {
        Coalesced::SectionEntered { task, section, .. } => {
            let now = reread(client, &task, fields![Task: name, memberships.section]).await?;
            if confirm_in_section(&now, &section) {
                println!("{} is in section {section}: act on it", name(&task, &now));
            }
        }
        Coalesced::TaskChanged { task, fields, .. } => {
            let now = reread(client, &task, fields![Task: name, completed]).await?;
            println!("{} changed: {}", name(&task, &now), fields.join(", "));
        }
        other => println!("not acted on: {other:?}"),
    }
    Ok(())
}
```

A move between sections arrives as an `added` event on the task with the new section as parent,
and usually a `removed` from the old one. The event does not say which project the section is in,
and a task can be in several projects, so in several sections at once. Coalescing is therefore per
section, not per task:

| In one delivery | The resolver reports |
|---|---|
| A move from "Doing" to "Done" | `SectionLeft` for "Doing", then `SectionEntered` for "Done" |
| Moves in two projects ("Doing" to "Done" in one, "Backlog" to "Ready" in the other) | Both pairs: two `SectionLeft` and two `SectionEntered` |
| A drag through "Doing" on the way to "Done" | `SectionLeft` for each section passed, `SectionEntered` for "Done" |
| Removed from "Doing" and added back | One `SectionEntered` for "Doing" |

With a shared `SeenTasks`, every reported `SectionLeft` forgets that task in that section, and a
task removed and added back within one delivery counts as a new arrival, so a return is reported.
The task may have moved again since the events, which is why `SectionEntered` is confirmed with
`reread` and `confirm_in_section` before acting. Story events can name a story Asana has since
consolidated away, and typing in a text field can produce several near-identical `changed`
events; coalescing folds those into one entry. Events that could not be read are kept in
`EventBatch::unreadable` with their raw JSON, not dropped silently.

## Latency, and the section watcher

Asana documents about a minute between a change and its delivery on average, most within ten
minutes, occasionally longer; `Event::created_at` says when the change happened. Nothing on the
receiving side shortens that, and the parts here add milliseconds. A receiver that must react
within seconds to one thing (a task arriving in a particular section, say) pairs the webhook with
a `SectionWatcher`, which lists that one section on your schedule and reports tasks that have
appeared since the last poll. A `SeenTasks` set shared with the resolver keeps the two from acting
twice on the same arrival. From `crates/asana-client/examples/section_watcher.rs`:

```rust
// Remember arrivals for longer than a webhook event can lag behind the watcher.
let seen = SeenTasks::new(Duration::from_secs(60 * 60));
// The webhook side: its resolver shares the set, so a `SectionEntered` the watcher already
// reported is not reported again when the event arrives.
let _resolver = Resolver::builder().seen(seen.clone()).build();

let mut watcher = SectionWatcher::new(section, seen).fields(fields![Task: name]);
let mut every = tokio::time::interval(Duration::from_secs(5));
loop {
    every.tick().await;
    // One request a poll (12 a minute at this rate), counted against the token's limits.
    // The first poll records what is already there and reports nothing.
    match watcher.poll(&client).await {
        Ok(arrivals) => {
            for task in arrivals.tasks {
                println!("arrived: {:?} {:?}", task.gid, task.name);
            }
            if !arrivals.complete {
                println!("over 100 tasks in the section: later ones are the webhook's");
            }
        }
        // The watcher keeps its last good listing; the next poll compares against it.
        Err(error) => eprintln!("poll failed: {error}"),
    }
}
```

Each poll is one read against the token's limits, so watch only the few sections where seconds
matter and leave the rest to the webhook. A section of more than 100 tasks is seen only in part.

## Checklist

- The handshake store is shared by every instance that creates or receives.
- The receiver is serving before the webhook is established.
- The webhook's GID, its secret (encrypted at rest) and its route digest are stored together.
- The signature is verified over the raw body before anything parses it.
- Deliveries are queued and answered within milliseconds; heartbeats are answered `200`.
- `410` is answered only for routes you retired on purpose.
- A scheduled job checks `health` and re-establishes webhooks that are failing or gone.
- The events stream catches up after outages.
- Your own writes, stale events and repeats are dropped, and each task is re-read before acting.
