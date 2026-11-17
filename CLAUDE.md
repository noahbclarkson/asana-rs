# Working in this repository

`asana-rs` is a Rust client for Asana's REST API. Most of its surface is generated from Asana's
published OpenAPI specification; the parts that make a client good to use are written once by hand.
It is a public, general-purpose library.

## Layout

| Path | What it is |
|---|---|
| `spec/asana_oas.yaml` | Asana's specification, vendored unmodified. `spec/UPSTREAM` names the commit; `spec/NOTICE` the licence. |
| `spec/overlays/` | Corrections applied before generating, one per file, each saying what is wrong upstream. |
| `xtask/` | The generator. `cargo xtask gen` rewrites `crates/asana-openapi/src/generated/`. Never published. |
| `crates/asana-openapi` | Generated models, endpoint descriptors, `opt_fields` enums, scopes and the `OPERATIONS` index, plus the hand-written `ids`, `endpoint` and `fields` modules. No HTTP. |
| `crates/asana-webhooks` | The webhook protocol: classify, handshake store, signature check, typed events, filters, dedupe, and the framework-neutral `Receiver` (`src/receive.rs`) that gives every answer. No HTTP client, no framework. |
| `crates/asana-client` | The client: transport, auth and OAuth, limits, retries, paging, errors, resource handles (`src/resources/`), the client side of webhooks (`src/webhooks/`). Its `examples/` are the user-facing examples. |
| `crates/asana-fake` | An in-memory Asana behind the client's transport, for consumers' tests. |
| `crates/asana-webhooks-axum`, `-actix` | Mount `asana_webhooks::Receiver` in those frameworks (`router(&receiver, path)`, `resource(&receiver, path)`), each with an `examples/receiver.rs`. Status rules belong in `Receiver`, not here. |
| `docs/` | The guide, webhooks in production, OAuth, testing, generating, design, and `coverage.md` (generated). |

Dependencies point one way: `asana-openapi` ← `asana-webhooks` ← `asana-client` ← `asana-fake`;
the adapters depend on `asana-webhooks` (and on `asana-client` only for their examples).

`docs/generating.md` explains the generator and overlays; `docs/design.md` records the decisions
and what was rejected. Read the second before proposing a structural change.

## Rules

- **Never edit `crates/asana-openapi/src/generated/` by hand.** Change the generator or add an
  overlay, run `cargo xtask gen`, and commit the result. CI runs `cargo xtask gen --check`.
- **Every dependency is declared once**, in the root `[workspace.dependencies]`; a crate takes it
  with `workspace = true`.
- **No cargo features** in the library crates: the whole API is generated, so coverage is never a
  feature decision, and a consumer's build never differs from ours.
- **Never switch on a dependency feature that changes a consumer's other crates**, above all
  `serde_json`'s `arbitrary_precision`: Cargo unifies features across a build, so it would change
  how every JSON number is held in an application that uses us. Decimals go through
  `asana_openapi::number`; CI and a test in `asana-client` fail if the feature comes back.
- **Asana's data is opaque.** A GID is never parsed as a number; an offset or a token is passed back
  exactly as it came. Enums read from Asana keep unknown values instead of failing.
- **Nothing secret is printed.** Tokens and webhook secrets are `secrecy` types; `Display` and
  `Debug` of errors never include a response body, Asana's messages, a query value or a header
  value.
- **Retry only what is safe.** A `POST` that creates something is never resent after an unknown
  outcome. An operation is marked idempotent only in `IDEMPOTENT_POSTS` in
  `xtask/src/endpoints.rs`, with a reason.
- **No `unwrap` or `expect` outside tests** (clippy denies both; examples included). A suppression is
  `#[expect(lint, reason = "...")]`, never a bare `#[allow]`.
- **Every public item has a doc comment** (`missing_docs` warns, and warnings fail CI). A handle
  method's doc names the operation it sends, its scope, and whether it is retried.
- **Files stay under 1,500 lines.** Split by responsibility: `foo.rs` plus `foo/`.
- **Tests need no network and no token.** They use recorded or documented response bodies,
  `wiremock` and the fake; time-dependent code runs on tokio's clock so tests pause it. A test that
  calls the live API is `#[ignore]` and reads `ASANA_TEST_TOKEN`; CI never holds a token.
- **Nothing private.** This repository is public: no company, client, person, real GID, workspace,
  email or token anywhere, in code, tests, fixtures, examples, docs or commit messages. Use
  "Acme", `example.com` and made-up GIDs such as `1200000000000001`.

## Documentation

- Every code block in `README.md`, `docs/` and the crate READMEs is copied from a compiled
  example or a doc test, and the text above it says which. Change the example first, then copy
  it. A new snippet needs a new example (in `crates/asana-client/examples/`, reading its inputs
  from the environment) or a doc test.
- Examples are compiled in CI and never run there. Each opens with a `//!` saying what it does
  and which environment variables it reads.
- `docs/coverage.md` and the operation count in the README come from
  `cargo run -p asana-client --example coverage > docs/coverage.md`; regenerate after a spec
  update. CI runs it and fails when the checked-in page differs.
- Plain, direct technical English; British spelling in prose. No marketing voice, no emoji, no
  exclamation marks.

## Checks

The same list as `CONTRIBUTING.md`, which CI runs:

```
cargo fmt --all -- --check
cargo xtask gen --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --workspace --examples
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --exclude xtask --no-deps
cargo deny check
```

`cargo test` runs the doc tests. For one crate, put `-p <crate>` in place of `--workspace`.

Commits are [Conventional Commits](https://www.conventionalcommits.org/).
