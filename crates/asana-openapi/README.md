# asana-openapi

Typed building blocks for [Asana's REST API](https://developers.asana.com/reference/rest-api-reference),
generated from [Asana's OpenAPI specification](https://github.com/Asana/openapi):

- `endpoints`: one descriptor per operation in the specification (`GetTask`, `CreateTask`, …),
  built with a builder and implementing the `Endpoint` trait: method, path, query, body, OAuth
  scope, whether it is safe to repeat, and the type it returns. `endpoints::OPERATIONS` indexes
  them all.
- `models`: one struct per resource (`Task`, `Project`, `Section`, …), every field optional,
  because `opt_fields` decides what Asana returns. Unknown enum values are kept, not errors.
- `fields`: one enum per resource of the `opt_fields` paths Asana accepts, and the `fields!` macro
  that checks a selection when the code compiles.
- `ids`: a GID type per kind of resource (`TaskGid`, `SectionGid`, …), validated when made.
- `scopes`: the OAuth scopes, and which one each operation needs.

It sends nothing and has no HTTP client. To send these, use
[`asana-client`](https://crates.io/crates/asana-client), which re-exports this crate as
`asana_client::openapi`.

```rust
use asana_openapi::fields;
use asana_openapi::fields::TaskField;

let chosen = fields![Task: name, due_on, memberships.section.name];
assert_eq!(chosen.to_query_value().as_deref(), Some("name,due_on,memberships.section.name"));
let same: asana_openapi::fields::Fields<TaskField> = chosen;
```

A path the specification does not list for that resource, such as `fields![Task: nmae]`, does not
compile.

This is an unofficial crate, not affiliated with Asana. It is part of
[asana-rs](https://github.com/noahbclarkson/asana-rs); see the repository for the guide, how the
code is generated, and the pinned version of the specification. Licensed under MIT or Apache-2.0;
the specification it is generated from is Asana's, under Apache-2.0.
