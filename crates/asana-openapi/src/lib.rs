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
//! - [`fields`]: one enum per resource of the `opt_fields` paths Asana accepts, and the
//!   [`fields!`] macro that checks a selection when the code is compiled.
//! - [`scopes`]: the OAuth scopes, and which each operation needs.
//!
//! Everything under `generated/` is written by `cargo xtask gen` from `spec/asana_oas.yaml` and is
//! never edited by hand.

pub mod endpoint;
pub mod fields;
pub mod ids;

mod generated;

pub use generated::endpoints;
pub use generated::models;
pub use generated::scopes;
