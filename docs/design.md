# Design

The decisions behind this workspace, and what was considered and rejected, for contributors. The
code is the authority on how each works; this page says why.

## Generate the API, write the behaviour once

Asana publishes an OpenAPI 3.0 document of its whole API: hundreds of operations, a typed list of
the `opt_fields` paths each one accepts, and the OAuth scope each one needs. The workspace
generates from it only what is pure description (models, one descriptor per operation, field
enums, scopes) and writes by hand, once, everything that is behaviour: transport, auth, limits,
retries, paging, errors, webhooks.

A descriptor is plain data that implements `Endpoint`: its method, path template, query, body,
scope, whether it is safe to repeat, where the result sits in the response, and the output type.
`Client::send` is the one function that sends any of them. So every operation, the ones with a
handle and the ones without, gets the same rate limiting, retry rules, token refresh, deprecation
tracking, tracing and redaction, and a fix to any of them is a fix for all.

Rejected:

- **A client generator** (progenitor, openapi-to-rust, openapi-generator). Tried on the real
  document: progenitor did not run on it as published (the multipart upload, then the events
  endpoint's own `412` schema); openapi-to-rust ran but wrote every operation's URL building,
  auth, error decoding and retry inline, tens of thousands of lines that build several times
  slower, and turned the typed `opt_fields` list into `Vec<String>`; openapi-generator's Rust is
  the least idiomatic and needs a Java toolchain to regenerate. Each would have needed a
  hand-written wrapper per operation anyway to get one retry and limit policy.
- **typify for the models.** It merges `allOf` chains correctly, but what this crate needs on
  top (one struct per family, typed GIDs, `time` and `Decimal` scalars, enums that keep unknown
  values, unions told apart by `resource_type`, `bon` builders) meant rewriting its output. The
  `allOf` merge it would have provided is a few lines over raw JSON, so the generator renders
  models itself with `quote`.

## One struct per resource family, every field optional

Asana describes `TaskCompact ⊂ TaskBase ⊂ TaskResponse`, but `opt_fields` can widen a compact
answer or narrow a full one, so no shape is ever guaranteed. Each family is one struct with every
field an `Option`. `fields!` documents and requests; it does not change the return type, and
`Require` turns a missing field into an error where the code needs it.

Rejected: a typestate response whose type says which fields were asked for. It is more machinery
than it removes, and it breaks on `with_raw` paths and on fields Asana adds.

## Typed GIDs

Every resource family with a GID has its own newtype over `Arc<str>`, checked when made (1 to 64
ASCII digits). The compiler then refuses a section's GID where a task's is wanted, a GID is never
parsed as an integer (Asana says GIDs are opaque strings), and text that is not a GID never
reaches a request path. `UserRef` is the one place a user can be a GID, an email address or
`me`, because Asana accepts all three there.

## Handles as sugar

`client.tasks().get(..)` returns `Request<'_, GetTask>`: the generated descriptor and the client.
Each resource module adds inherent setters to `Request<'_, ThatDescriptor>`, so `.fields(..)`,
`.name(..)` and `.custom_field(..)` exist exactly where the operation takes them, and the escape
hatches (`map`, `endpoint_mut`, `into_endpoint`) reach anything else. Handles are added where they
read better; coverage never depends on them.

## The execution path

One hand-written pipeline, per call: search bucket, read or write permit, the client's shared
cooldown, a token from `Auth`, the transport, then the answer's status decides.

- **Limits per client, so per token.** Asana counts concurrent reads and writes separately and
  counts refused requests against the quota, so the client keeps under the ceilings instead of
  learning them from `429`s, and one `429` holds back every task using that client until the
  `Retry-After` time. Defaults (40 reads, 12 writes, 55 searches a minute) sit below Asana's
  (50, 15, 60) to leave room for other tools on the same token.
- **Retry only what is safe.** `429` for anything, after exactly `Retry-After` (never a fixed
  minute). `5xx` and ambiguous transport failures only for `GET`, `PUT`, `DELETE` and the `POST`s
  the generator lists as idempotent. A `POST` that creates is never resent, because a duplicate
  task or comment is worse than an error the caller can see.
- **No tower, no reqwest-middleware.** The policy is the crate's core value and is a fixed set of
  steps; generic layers would add dependencies and type complexity without a second policy to
  compose.
- **`Transport` is a trait** because it has two real implementations: `ReqwestTransport` (which
  wraps any `reqwest::Client`, so a user keeps their own TLS, proxy and redirect policy) and the
  fake. It takes and returns plain `http::Request<Bytes>` / `http::Response<Bytes>`, so neither
  side sees an HTTP library's types.
- **Paging is a stream.** Lists are lazy `Stream`s of items or pages; offsets are passed back
  verbatim.

## One error type, nothing secret in it

`Error` is one `#[non_exhaustive]` enum for every call, with variants for the statuses a caller
handles differently. Its `Display` never includes a response body, Asana's messages, a query
value, a header value or a token, because those carry customer data and errors end up in logs.
Asana's messages are still kept in `ApiErrors` for a developer to read on purpose. `Decode` carries
the JSON path of the failing value, never the value. Tokens, client secrets and webhook secrets
are `secrecy` types whose `Debug` is redacted.

## Custom field values

`CustomFieldValue` is one enum for reading and writing, chosen by the field's kind. Numbers are
`rust_decimal::Decimal`, never `f64`. On the wire they are plain JSON numbers, which Asana stores
as doubles: a number is read through the shortest text of its double and written as an integer
when it is one, otherwise as the nearest double (`asana_openapi::number`). `serde_json`'s
`arbitrary_precision` was rejected: Cargo unifies features across a build, so a library that
enabled it would change how every JSON number is held in every application that depends on it,
and Asana's doubles could not carry the extra digits anyway. CI fails if it comes back. `Empty` is the one way to say "no value": unset when read,
`null` when written. Unknown kinds are kept with their raw JSON.

## Webhooks: protocol apart from transport

`asana-webhooks` is the protocol as functions over headers and bytes, with no HTTP client and no
framework, so any server can use it and its tests need neither. The signature is checked over the
raw bytes before anything is parsed, and only a `VerifiedBody` can be decoded. The handshake is a
small state machine over a `HandshakeStore` the user supplies, because the call that captures the
secret and the call that created the webhook may be in different processes.

The adapters are **separate crates, not features**. Each is small and tied to its framework's
semver, so an axum or actix-web major release can ship without a new version of the protocol or
client crates, and nobody's lock file carries a framework they do not use.

`410 Gone` deletes a webhook, so the adapters answer it only for a route the user marks retired,
and answer an unknown route `404`: a lost database row must never delete live webhooks.

## No cargo features

No library crate declares a feature. The whole API is generated, so coverage is never a feature
decision; every build of a crate is the same build, which suits workspaces that forbid feature
differences between builds; and there is no untested combination. The cost is compile time for
the generated crate, kept down by one struct per family rather than per response shape,
`Deserialize` only on what is read and `Serialize` only on what is written, and no HTTP code in
generated output.

## Asana's data is loose

Asana adds enum values, resource types and fields, and counts a new enum value as a breaking
change on its side. Here an unknown enum value is kept (generated enums have an `Unknown(String)` variant), an
unknown `resource_type` is kept, extra fields are ignored, and an event that cannot be read is
kept with its raw JSON rather than failing its batch.

## Tests need no network

Tests use bodies shaped like Asana's documented examples, `wiremock` for the HTTP layer, and the
fake for behaviour across calls. Nothing in CI holds a token. Time-dependent behaviour (retry
waits, job polling, the section directory's time-to-live) runs on tokio's clock so tests pause
and advance it instead of sleeping.
