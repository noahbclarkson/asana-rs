# Contributing

Thank you for helping. Bug reports, corrections to how the client reads Asana's API, and pull
requests are all welcome. For a large change, open an issue first so we can agree on the shape
before you write it.

Never paste a real access token, OAuth secret or webhook secret into an issue, a pull request, a
log or a test. If you have, revoke it in Asana straight away. Security problems go through
[SECURITY.md](SECURITY.md), not a public issue.

## Where things are

[README.md](README.md) shows the crates from a user's side. [docs/design.md](docs/design.md) says
why the workspace is built the way it is. [docs/generating.md](docs/generating.md) covers the
generator, overlays and spec updates.

- **Generated code is never edited by hand.** Everything under
  `crates/asana-openapi/src/generated/` comes from `cargo xtask gen`. To change it, change the
  generator in `xtask/`, or add an overlay in `spec/overlays/` when Asana's document is wrong (one
  correction per file, with a comment saying what is wrong and how you know). Then regenerate and
  commit the result. `spec/asana_oas.yaml` is Asana's document, vendored unmodified.
- `cargo xtask spec-diff old.yaml new.yaml` lists the operations and components that differ
  between two versions of Asana's document. A weekly workflow uses it to propose updates.

## Checks

CI runs all of these, and a pull request needs them green:

```sh
cargo fmt --all -- --check
cargo xtask gen --check                           # the generated code matches spec + overlays + generator
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace                            # unit, integration and doc tests
cargo build --workspace --examples
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --exclude xtask --no-deps
cargo deny check                                  # advisories, licences, bans, sources (cargo install cargo-deny)
```

CI also checks the libraries on the minimum supported Rust (`rust-version` in the root
`Cargo.toml`), builds on macOS and Windows, and packages every crate and builds it from its
tarball (`cargo package --workspace --exclude xtask`).

## Rules the code follows

- **Tests need no network and no token.** They use response bodies recorded from Asana's
  documentation or invented, served by `wiremock` or by `asana-fake`. Use `example.com`, "Acme"
  and made-up GIDs such as `1201234567890123`, never real ones. A live test against Asana must be
  `#[ignore]`d and read its token from `ASANA_TEST_TOKEN`. CI never holds a token and never runs
  one. Tests do not sleep on real time: use `tokio::time::pause`.
- **Asana's data is opaque and loose.** A GID is never parsed as a number. An offset or sync token
  is passed back exactly as it came. An unknown enum value, `resource_type` or extra field is
  kept, never an error.
- **Nothing secret is printed.** Tokens and secrets are `secrecy` types. No `Display` or `Debug`
  of an error includes a response body, Asana's messages, a query value or a header value.
- **No cargo features** in the library crates, and every dependency is declared once, in the
  root `[workspace.dependencies]`.
- No `unwrap` or `expect` outside tests. A lint suppression is `#[expect(lint, reason = "...")]`.
  Every public item has a doc comment. Files stay under 1,500 lines.

Commit messages follow [Conventional Commits](https://www.conventionalcommits.org/) (`feat:`,
`fix:`, `docs:`, `chore(spec):` and so on). Pull requests go into `main`.

## Versioning

All crates share one version, set in the root `Cargo.toml`, and are released together. They
follow [Semantic Versioning](https://semver.org/). While the version is `0.x`, the minor number
is the breaking one: `0.1.0` to `0.2.0` may break, and `0.1.0` to `0.1.1` must not.

Most changes come from Asana's document, so this is how a spec update maps to a version:

| Change in Asana's document | Effect on the generated code | Release while 0.x (from 1.0) |
|---|---|---|
| New operation, model, field or enum value | New items; models and enums are `#[non_exhaustive]` | patch (minor) |
| New optional parameter on an operation | A new builder method | patch (minor) |
| Documentation only | Doc comments | patch |
| Removed operation, model or field | Items disappear | **minor (major)** |
| New required (path) parameter, or a changed field type | Signatures change | **minor (major)** |

A field or enum value Asana adds is not breaking for code that compiles today. It can still
change behaviour: a value that used to decode as `Unknown("x")` decodes as its own variant once
the document names it. The changelog says so when that happens.

The minimum supported Rust version is `rust-version` in the root `Cargo.toml`. Raising it is
allowed in a minor release (a `0.x` minor while below 1.0) and is noted in the changelog.

## Releasing

Maintainers only. Releases are made by hand from a clean `main`.

1. Choose the version from the table above. Set it in `[workspace.package] version` **and** in
   the `version` of each `asana-*` entry in `[workspace.dependencies]`, which must match.
2. In `CHANGELOG.md`, give the release its heading, `## [x.y.z] - YYYY-MM-DD`, and its link at
   the bottom.
3. Run `cargo publish --dry-run --workspace --exclude xtask --locked`, then open a pull request
   titled `chore(release): x.y.z` and merge it once CI is green.
4. Tag the merge commit `vx.y.z`, push the tag, and make a GitHub release with that version's
   changelog section.
5. Publish: `cargo publish --workspace --exclude xtask --locked`. Cargo publishes in dependency
   order and waits for each crate to reach the index before the next. Done by hand, the order is
   `asana-openapi`, `asana-webhooks`, `asana-client`, then `asana-fake`, `asana-webhooks-axum`
   and `asana-webhooks-actix`.
6. Check that docs.rs built every crate.

A version on crates.io cannot be replaced. If a release is wrong, yank it with
`cargo yank --version x.y.z <crate>` and publish a fixed patch.

## Licence

Unless you explicitly state otherwise, any contribution you intentionally submit for inclusion in
this work, as defined in the Apache-2.0 licence, is dual licensed as MIT OR Apache-2.0, without
any additional terms or conditions.
