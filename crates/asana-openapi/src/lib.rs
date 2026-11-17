#![forbid(unsafe_code)]

//! Typed building blocks for Asana's REST API, generated from its OpenAPI specification.
//!
//! This crate describes the API and sends nothing. It holds:
//!
//! - [`ids`]: one GID type per kind of resource, so a section's GID cannot be passed as a task's.
//! - [`endpoint`]: the [`Endpoint`](endpoint::Endpoint) trait every operation implements.
//! - [`endpoints`]: one descriptor per operation in the specification.
//! - [`models`]: one struct per resource, with every field optional because `opt_fields` decides
//!   what Asana returns.
//! - [`mod@fields`]: one enum per resource of the `opt_fields` paths Asana accepts, and the
//!   [`fields!`](macro@fields) macro that checks a selection when the code is compiled.
//! - [`scopes`]: the OAuth scopes, and which each operation needs.
//! - [`number`]: Asana's numbers as `rust_decimal::Decimal`, read from and written to plain JSON
//!   numbers.
//!
//! Everything under `generated/` is written by `cargo xtask gen` from `spec/asana_oas.yaml` and is
//! never edited by hand.
//!
//! A descriptor is data: its path, query, scope and output type, and nothing that sends it
//! (`asana-client` does that).
//!
//! ```
//! use asana_openapi::endpoint::Endpoint;
//! use asana_openapi::endpoints::GetTask;
//! use asana_openapi::fields;
//! use asana_openapi::scopes::Scope;
//!
//! # fn main() -> Result<(), asana_openapi::ids::GidError> {
//! let get = GetTask::builder("1200000000000001".parse()?)
//!     .opt_fields(fields![Task: name, due_on])
//!     .build();
//! assert_eq!(get.path(), "/tasks/1200000000000001");
//! assert!(get.query().iter().any(|(name, value)| name == "opt_fields" && value == "name,due_on"));
//! assert_eq!(GetTask::SCOPE, Some(Scope::TasksRead));
//! # Ok(())
//! # }
//! ```

pub mod endpoint;
pub mod fields;
pub mod ids;
pub mod number;

mod generated;

pub use generated::endpoints;
pub use generated::models;
pub use generated::scopes;
