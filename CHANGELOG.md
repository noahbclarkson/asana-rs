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

[0.1.0]: https://github.com/noahbclarkson/asana-rs/releases/tag/v0.1.0
