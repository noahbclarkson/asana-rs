#![forbid(unsafe_code)]

//! An async client for Asana's REST API.
//!
//! Every operation in Asana's specification is a typed descriptor in [`asana_openapi`]; this crate
//! sends them. Authentication, per-token rate limits, retrying on `Retry-After`, paging as a
//! stream and one error type are written once here and apply to every operation.

mod auth;
mod client;
mod error;
mod limits;
mod multipart;
mod page;
mod require;
mod retry;
mod send;
mod transport;

pub use asana_openapi as openapi;

pub use auth::{AccessToken, Auth, AuthError, TokenSource};
pub use client::{
    AsanaChange, BuildError, Client, ClientBuilder, DEFAULT_BASE_URL, DEFAULT_USER_AGENT,
};
pub use error::{ApiError, ApiErrors, DecodeError, DecodeErrorKind, Error, SyncToken};
pub use limits::Limits;
pub use page::{Items, Page, Pages};
pub use require::Require;
pub use retry::RetryPolicy;
pub use send::decode_response;
pub use transport::{BoxError, ReqwestTransport, Transport, TransportError, TransportErrorKind};

/// Secret strings, re-exported so callers need not depend on `secrecy` to make an [`Auth`].
pub use secrecy::{ExposeSecret, SecretString};
