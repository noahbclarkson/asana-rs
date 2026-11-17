# asana-openapi

Models, endpoint descriptors, typed `opt_fields` selectors and OAuth scopes generated from
[Asana's OpenAPI specification](https://github.com/Asana/openapi), plus the small hand-written core
they stand on (typed GIDs and the `Endpoint` trait). It contains no HTTP client: use
[`asana-client`](https://crates.io/crates/asana-client) to send these.
