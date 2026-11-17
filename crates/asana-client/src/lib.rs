#![forbid(unsafe_code)]

//! An async client for Asana's REST API.
//!
//! Every operation in Asana's specification is a typed descriptor in [`asana_openapi`]; this crate
//! sends them. Authentication, per-token rate limits, retrying on `Retry-After`, paging as a
//! stream and one error type are written once here and apply to every operation.

pub use asana_openapi as openapi;
