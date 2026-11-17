# Changelog

All notable changes to the crates in this repository are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the crates follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html) as described in
[CONTRIBUTING.md](CONTRIBUTING.md#versioning). Every crate in the workspace shares one version and
is released together.

## [0.1.0] - Unreleased

The first release. Generated from Asana's OpenAPI document at upstream commit `65bd6e7`
(committed 2026-11-09), with the corrections in `spec/overlays/`.

### Added

- `asana-openapi`: a typed descriptor for each of the 271 operations in Asana's specification,
  with a builder whose required arguments are the path's parameters, the response envelope
  removed, and pagination marked. One model struct per resource family, every field optional
  (`opt_fields` decides what Asana returns) and `#[non_exhaustive]`. String enums keep values they
  do not know as `Unknown(String)`. Typed GIDs, one per kind of resource. An `opt_fields` enum per
  family with the `fields!` macro, which checks a field selection at compile time. The OAuth
  scope table, saying which scopes each operation needs. No HTTP client.
- `asana-webhooks`: the webhook protocol with no HTTP client and no web framework. It tells a
  handshake from a delivery, answers the handshake through a `HandshakeStore` (an in-memory one
  is included), and checks the HMAC-SHA256 signature over the raw body in constant time before
  anything is parsed. It also decodes typed events, recognises heartbeats, and gives event keys
  for deduplication and filters.
- `asana-client`: the async client.
  - Requests go through a `Transport` trait, with a `reqwest` and rustls implementation.
  - Authentication is a personal access token or OAuth.
  - Rate limits are kept per token, requests are retried on `Retry-After` with a configurable
    policy, and paging is a stream of items or pages.
  - There is one `Error` type, which carries Asana's error body without ever printing a token,
    a header value or a response body.
  - Resource helpers cover tasks, sections, projects, custom fields (typed reading and writing of
    values), tags, users, workspaces and their search, stories (with an `Html` builder that
    escapes), attachments (multipart upload and download), templates, jobs, the batch API, and the
    events stream.
  - OAuth: an authorisation URL with PKCE S256 and `state`, exchanging, refreshing, revoking and
    introspecting tokens, a `RefreshingTokenSource` over the caller's `TokenStore`, and scope
    checking against the generated table.
  - Webhooks: `establish` across the handshake, health, listing and deleting, a `Resolver` that
    turns events into the work they mean, and a section watcher.
- `asana-fake`: an in-memory Asana behind the client's transport, for consumers' tests. It holds
  projects, sections, tasks, custom fields, tags, stories and webhooks, answers in Asana's
  envelope and error shapes, performs the webhook handshake and signs deliveries.
- `asana-webhooks-axum` and `asana-webhooks-actix`: webhook receivers for axum 0.8 and
  actix-web 4. Each caps the body size, captures the handshake secret, verifies deliveries and
  hands each verified batch to a sink. They answer `404` for an unknown route and `410` only for
  a retired one.
- Numbers (`rust_decimal::Decimal`) are read from and written to plain JSON numbers through
  `asana_openapi::number`, without `serde_json`'s `arbitrary_precision`, which a library must not
  switch on for its users' whole build. A number reads as the shortest text of the double Asana
  sends; it is written as an integer when it is one, otherwise as the nearest double (about 15
  significant digits, which is what Asana stores), so `1250.50` goes out as `1250.5`.
- The minimum supported Rust version is 1.89 (the actix-web releases with current security fixes
  need it).
- Repository: `cargo xtask gen` to regenerate (`--check` in CI), and `cargo xtask spec-diff` to
  summarise a change in Asana's document. A weekly workflow proposes spec updates as pull
  requests. There is also a guide, an OAuth guide, a webhooks guide and a coverage table under
  `docs/`, and examples in each crate.

### Changed

These change what code built against an earlier revision of `main` sees or compiles against.

- `asana-client`: `Resolver::coalesce` groups section events per task *and section*, not per
  task. A task in several projects is in several sections at once, and the old grouping kept only
  the newest of all its section events, so in one delivery with moves in two projects only one
  move survived, and a shared `SeenTasks` never forgot the sections it left. A move now gives a
  `SectionLeft` for the old section and a `SectionEntered` for the new one (the departure first
  at the same instant); a drag through several sections gives a `SectionLeft` for each section
  passed. A task removed from a section and added back within a delivery is one
  `SectionEntered`, reported even when a shared `SeenTasks` had seen the earlier arrival. Groups
  at the same instant now come out departures first.
- `asana-client`: `Debug` of an error no longer prints Asana's messages or other request
  values. `ApiErrors` and `ApiError` have hand-written `Debug` that writes `<redacted>` for each
  `message` and `help` (the count and each `phrase` stay); `SyncToken`'s `Debug` hides the token;
  `OAuthError`'s `Debug` hides `error_description`; `OAuthErrorCode`'s `Debug` hides an unknown
  code that does not look like one, as its `Display` already did. Read them on purpose with
  `Error::api_errors`, `ApiErrors::messages`, `SyncToken::as_str` and the new
  `OAuthError::description`.
- `asana-client`: `RetryPolicy::max_wait` bounds every wait for a rate limit. A call that finds
  more than `max_wait` left of the client's shared cooldown from an earlier `429` now fails at
  once with `Error::RateLimited { retry_after: <what is left>, attempts }` (`attempts: 0` when it
  was never sent) instead of waiting out the whole cooldown while holding a permit.
  `RetryPolicy::never()` therefore never waits for a cooldown. A `Retry-After` over a day is read
  as a day; before, a value too large for the clock panicked the calling task.
- `asana-client`: `Error::Server` and `Error::Transport` gain a field, `safe_to_repeat: bool`
  (a pattern that lists every field needs `..` or the new one). `Error::is_retryable()` is no
  longer true for a request that creates something when Asana may have carried it out (a `5xx`,
  a timeout or a connection lost after it was made); before, a caller retrying on it made the
  duplicate the client itself refuses to make. New: `Error::outcome()` and the `Outcome` enum
  (`NotSent`, `Refused`, `Unknown`, `Applied`). A `DELETE` resent after an attempt whose answer
  was lost, and answered `404`, now succeeds instead of returning `Error::NotFound` for a delete
  that worked.
- `asana-client`: a batch counts against the client's limits action by action, as Asana counts
  it: a read or write permit per action and a search token per search. Before, a batch of ten
  took one write permit and no search token, so concurrent batches overran Asana's limits.
- `asana-client`: `Page::is_last()` is true only when Asana offers no next page, and the
  streams, `Client::pages` and `collect_max` follow a next page past a page with no items. Before,
  any empty page ended the list and silently dropped the rest. The audit log
  (`getAuditLogEvents`), which offers a next page for ever, still ends at its first empty page.
- `asana-openapi`: `Paginated` has a new provided method, `limit(&self) -> Option<PageLimit>`,
  which every generated list descriptor implements. `asana-client`: `Client::pages`,
  `Client::stream`, `Request::stream` and `Request::pages` keep a page size the caller set
  (`limit`) and ask for 100 only when none was set, and `collect_max` keeps its pages within it.
  Before, they always asked for 100.
- `#[non_exhaustive]` on the public types Asana or this workspace will grow, so that adding a
  field, an action or a method is not a breaking change. A struct expression of one of the
  structs no longer compiles outside its crate, a pattern needs `..`, and a `match` on one of the
  enums needs a catch-all arm.
  - `asana-webhooks`: the structs `Event`, `Change`, `ChangeValue`, `EventBatch`,
    `UnreadableEvent`, `ResourceRef` and `EventUser`; the enums `EventAction`, `Inbound`,
    `HandshakeOutcome` and `RouteSecret`. New constructors: `Event::new(resource, action,
    created_at)` (then set `user`, `parent` and `change`), `Change::new(field, action)`,
    `ChangeValue::from_json(raw)`, `EventUser::new(gid)` and `EventBatch::new(events)`;
    `ResourceRef::from(gid)` already existed. `EventAction`'s documentation says to match a value
    it does not name with `as_str()`, because a value Asana adds may later get its own variant
    and stop arriving as `Unknown`.
  - `asana-client`: the enums `Auth` (build it with its constructors) and `Replaced`.
  - `asana-openapi`: the struct `Part` (new: `Part::new(name, value)`) and the enums `Method`,
    `Envelope` and `PartValue`. The client refuses, before sending, a method the `http` crate
    cannot carry and a multipart part of a kind it cannot write.
- `asana-webhooks` and `asana-client`: a time to live too long for the clock (`Duration::MAX`
  for "never forget") in `MemoryDedupe`, `MemoryHandshakeStore`, `SeenTasks` and
  `ManualClock::advance` now runs to the latest time there is instead of panicking.

[0.1.0]: https://github.com/noahbclarkson/asana-rs/releases/tag/v0.1.0
