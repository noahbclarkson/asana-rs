# Generating the API

Most of `asana-openapi` is written by a generator in [`xtask/`](../xtask/) from Asana's OpenAPI
document. This page is for contributors: how the generator works, how to correct the document
with an overlay, and how to move to a newer document.

## What is generated, and what is not

```text
spec/asana_oas.yaml ──▶ overlays ──▶ xtask gen ──▶ crates/asana-openapi/src/generated/
                                                      models/      one struct per resource family
                                                      endpoints/   one descriptor per operation
                                                      fields/      one opt_fields enum per family
                                                      ids.rs       the typed GID list
                                                      scopes.rs    the OAuth scope enum
```

Everything under `generated/` is written by `cargo xtask gen` and never edited by hand; each file
starts with a line saying so. The rest of `asana-openapi` is hand-written and small: `ids.rs` (the
`Gid` trait, `GidError`, `UserRef` and the `gid!` macro the generated list calls), `endpoint.rs`
(the `Endpoint` and `Paginated` traits, `Method`, `RequestBody`, `Offset`, `PageLimit`) and
`fields.rs` (`Fields`, `FieldPath`, `HasFields` and the `fields!` macro). Everything in
`asana-client` and `asana-webhooks` is hand-written.

## How the generator works

`cargo xtask gen` loads `spec/asana_oas.yaml`, applies every overlay in `spec/overlays/` in file
name order, and runs two halves that agree on names through `xtask/src/naming.rs`:

- **Models** (`xtask/src/models/`). Asana describes a resource as a family of shapes:
  `TaskCompact` is a subset of `TaskBase`, which is a subset of `TaskResponse`. Since `opt_fields`
  can widen a compact answer or narrow a full one, no shape is guaranteed, so each family becomes
  **one** struct (`Task`) holding the union of its shapes' properties, every one an `Option`.
  Request shapes (`TaskRequest`, `TaskUpdateRequest`) stay types of their own, without their
  read-only properties, with `bon` builders. Dates become `time::Date`, date-times
  `time::OffsetDateTime`, numbers that can be fractional `rust_decimal::Decimal` (read and written
  through the hand-written `asana_openapi::number`), GIDs the family's
  typed GID. String enums keep values they do not know (`Unknown(String)`), and unions are told
  apart by `resource_type`. The step is classify (which family each schema belongs to), build
  (an intermediate form), then render and lay out one file per family.
- **Endpoints** (`xtask/src/endpoints/`, `fields.rs`, `scopes.rs`). Each operation becomes a
  descriptor in the module of its tag, with a builder whose required arguments are the path's
  parameters, implementing `Endpoint` (and `Paginated` when it returns pages). The response's
  `{data: ...}` envelope is removed in the descriptor's `Output`. The `opt_fields` enum of each
  family is the union of the paths every operation returning that family lists. The scope table
  comes from the document's `oauth2` security scheme, plus the scopes only operations name.
  `endpoints::OPERATIONS` indexes every operation for coverage tables and tests.

Two lists in the generator hold judgements the document cannot:

- `IDEMPOTENT_POSTS` in `xtask/src/endpoints.rs`: the `POST` operations that leave Asana as one
  send would if sent twice (adding a tag, moving a task into a section, …). Their descriptors get
  `IDEMPOTENT = true`, so the client retries them after a server error. Every other `POST`
  creates something and is never resent. Add an operation here only when sending it twice is
  harmless, and say why in the comment beside it.
- `OVERRIDES`, `FAMILY_OVERRIDES` and `WRITE_OVERRIDES` in `xtask/src/naming.rs`: schemas whose
  generated name or family the naming rule gets wrong. Keep them sorted and say why for each.

Generated modules are split when one would pass 1,400 lines (`MAX_LINES`), so no file passes the
repository's 1,500-line limit.

## Overlays

The vendored document is never edited. Where it is wrong, a file in `spec/overlays/` corrects it
before generation. Each is an [OpenAPI Overlay 1.0](https://spec.openapis.org/overlay/v1.0.0.html)
document holding one correction, with a comment at the top saying what upstream gets wrong and
how that was established. For example, `spec/overlays/50-dates-without-a-format.yaml` opens:

```yaml
# These properties hold dates (`YYYY-MM-DD`) or date-times (RFC 3339), as their examples and
# descriptions show, but declare no `format`, unlike every other date in the specification. Without
# one they would be plain strings, so a caller would parse each by hand and a mistyped date would
# reach Asana instead of failing to compile.
overlay: 1.0.0
info:
  title: Dates without a format
  version: 1.0.0
actions:
  - target: $.components.schemas.CustomFieldCompact.properties.date_value.properties.date
    update:
      format: date
```

The generator supports the subset of the Overlay specification these files need: a `target` is a
path from the root made of `.name`, `['name']` and `[index]` steps (no wildcards or filters), and
an action either has `remove: true` or an `update` that is merged in (objects key by key,
anything else replaced). When Asana fixes or moves something an overlay corrected, the next
generation should fail and say which overlay and action, instead of applying it silently. Three
things make it fail:

- **A target that names nothing.**
- **An `update` that changes nothing**: upstream already says what the overlay says.
- **A node that is not as the action's `x-expect` says.** `x-expect` is an object of keys the
  target must hold with those values, `null` meaning the key must be absent. An overlay states it
  where upstream could change things and still leave the target naming a node: defining a
  property the overlay adds (`70-…` expects no `memberships`), or reordering an `allOf` that an
  `[index]` step walks (`40-…` expects `allOf[1]` to be the inline `type: object` part, not the
  `$ref`).

An upstream change that none of these sees (a new value for a key the overlay replaces, say) still
applies the overlay; prefer targets without `[index]` steps, and an `x-expect` where they cannot be
avoided. Delete or update a failing overlay.

The overlays in place:

| File | Corrects |
|---|---|
| `10-custom-field-values.yaml` | Custom field values in requests are any JSON value, not only strings. |
| `20-follower-and-member-lists.yaml` | Followers and members in add/remove requests are arrays, not one comma-joined string. |
| `30-placeholder-resource-type.yaml` | A placeholder's `resource_type` example says `user`. |
| `40-project-status-is-read-only.yaml` | A project's current status cannot be written by a project request. |
| `50-dates-without-a-format.yaml` | Dates and date-times that declare no `format`. |
| `60-story-number-values-are-decimal.yaml` | A story's old and new number values are decimals, not integers. |
| `70-task-create-memberships.yaml` | A new task can be created into a section through `memberships`. |
| `80-set-parent-nullable.yaml` | `parent: null` on `setParent` removes a task's parent. |

Each file's own comment is the authority on what it does and why.

## Running it

```text
cargo xtask gen                    # rewrite crates/asana-openapi/src/generated/
cargo xtask gen --check            # fail if the checked-in code differs from what gen would write
cargo xtask gen --only models      # one half, while working on it
cargo xtask gen --only endpoints
cargo xtask spec-diff old.yaml new.yaml   # what Asana changed between two documents, as Markdown
```

`spec-diff` compares two upstream documents as published, without overlays: the operations
added, removed, changed in shape or changed in documentation only, and the component changes.

`gen` formats everything with `prettyplease` and removes generated files that are no longer
produced. `--check` writes nothing and fails, naming the files, when the checked-in code differs
from what the document, the overlays and the generator would produce. CI runs it on every push,
so a hand edit under `generated/`, a generator change without regenerating, or an overlay without
its output all fail the build.

## Updating to a newer document

The workflow `.github/workflows/update-spec.yml` does this every Monday (and on demand from the
Actions tab): it finds upstream's latest commit of the document, and when that differs from
`spec/UPSTREAM`'s, does steps 1 to 3 and 5 below, summarising the change with
`cargo xtask spec-diff`, runs fmt, clippy and the tests, and opens or updates a pull request on
the `spec-update` branch labelled `spec update`, with the summary and any failed checks in its
body. An overlay whose target is gone fails `cargo xtask gen`, which fails the run and opens
nothing. Review the pull request as any other change.

By hand:

1. Fetch `defs/asana_oas.yaml` from [Asana/openapi](https://github.com/Asana/openapi) at the
   commit you want, and see what changed: `cargo xtask spec-diff spec/asana_oas.yaml new.yaml`.
2. Replace `spec/asana_oas.yaml` with the new file, unmodified, and update `spec/UPSTREAM`: the
   commit, its date, and the date it was fetched.
3. Run `cargo xtask gen`. If an overlay's target no longer exists, read upstream's change: delete
   the overlay if Asana fixed the problem, or update its target.
4. Run the checks in [`CONTRIBUTING.md`](../CONTRIBUTING.md#checks) and read the diff under
   `generated/`. A removed operation, model or field, a new required (path) parameter, or a
   changed field type is a breaking change for users. A new field on a model, a new enum value
   or a new optional parameter is not (models and enums are `#[non_exhaustive]`, and optional
   parameters are builder methods). [`CONTRIBUTING.md`](../CONTRIBUTING.md#versioning) has the
   table of which change needs which release.
5. Regenerate the coverage page, which also gives the operation count the README quotes:
   `cargo run -p asana-client --example coverage > docs/coverage.md`. Update the count in
   `README.md` if it changed. CI fails when `docs/coverage.md` is out of date.
6. Note the update in `CHANGELOG.md`.
