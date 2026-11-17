# Working in this repository

`asana-rs` is a Rust client for Asana's REST API. Most of its surface is generated from Asana's
published OpenAPI specification; the parts that make a client good to use are written once by hand.

## Layout

| Path | What it is |
|---|---|
| `spec/asana_oas.yaml` | Asana's specification, vendored unmodified. `spec/UPSTREAM` names the commit. |
| `spec/overlays/` | Corrections applied before generating, one per file, each saying what is wrong upstream. |
| `xtask/` | The generator. `cargo xtask gen` rewrites `crates/asana-openapi/src/generated/`. |
| `crates/asana-openapi` | Generated models, endpoint descriptors, `opt_fields` enums and scopes, plus the hand-written `ids`, `endpoint` and `fields` modules. No HTTP. |
| `crates/asana-webhooks` | The webhook protocol: handshake, signature check, typed events. No HTTP client, no framework. |
| `crates/asana-client` | The client: transport, auth and OAuth, limits, retries, paging, errors, resource handles. |
| `crates/asana-fake` | An in-memory Asana behind the client's transport, for consumers' tests. |
| `crates/asana-webhooks-axum`, `-actix` | Thin handlers for those frameworks. |

Dependencies point one way: `asana-openapi` ← `asana-webhooks` ← `asana-client` ← `asana-fake`.

## Rules

- **Never edit `crates/asana-openapi/src/generated/` by hand.** Change the generator or add an
  overlay, run `cargo xtask gen`, and commit the result. CI runs `cargo xtask gen --check`.
- **Every dependency is declared once**, in the root `[workspace.dependencies]`.
- **No cargo features** in the library crates: the whole API is generated, so coverage is never a
  feature decision, and a consumer's build never differs from ours.
- **Asana's data is opaque.** A GID is never parsed as a number; an offset or a token is passed back
  exactly as it came. Enums read from Asana keep unknown values instead of failing.
- **Nothing secret is printed.** Tokens and webhook secrets are `secrecy` types; `Display` and
  `Debug` of errors never include a response body, a query value or a header value.
- **No `unwrap` or `expect` outside tests** (clippy denies both). A suppression is
  `#[expect(lint, reason = "...")]`, never a bare `#[allow]`.
- **Files stay under 1,500 lines.** Split by responsibility: `foo.rs` plus `foo/`.
- **Tests need no network and no token.** They use recorded or documented response bodies and the
  fake. The live smoke tests are `#[ignore]` and read `ASANA_TEST_TOKEN`; CI never holds a token.
- **Examples and fixtures are invented**: `example.com`, "Acme", made-up GIDs.

## Checks

```
cargo fmt --all
cargo xtask gen --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Commits are [Conventional Commits](https://www.conventionalcommits.org/).
