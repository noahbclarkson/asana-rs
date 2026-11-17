#![forbid(unsafe_code)]

//! An async client for Asana's REST API.
//!
//! Every operation in Asana's specification is a typed descriptor in [`asana_openapi`]; this crate
//! sends them. Authentication, per-token rate limits, retrying on `Retry-After`, paging as a
//! stream and one error type are written once here and apply to every operation.
//!
//! ```no_run
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! use asana_client::{Auth, Client, fields};
//!
//! let client = Client::builder()
//!     .auth(Auth::personal_access_token(std::env::var("ASANA_ACCESS_TOKEN")?))
//!     .build()?;
//! let task = client
//!     .tasks()
//!     .get("1200000000000001".parse()?)
//!     .fields(fields![Task: name, memberships.section.name])
//!     .send()
//!     .await?;
//! println!("{:?}", task.name);
//! # Ok(())
//! # }
//! ```
//!
//! Start at [`Client::builder`]. Handles for the common work are in [`resources`] (reached as
//! `client.tasks()`, `client.sections()` and so on), the client side of webhooks in [`webhooks`],
//! OAuth in [`oauth`], and every generated operation in [`openapi::endpoints`], sent with
//! [`Client::send`]. The guide, the webhooks guide and runnable examples are in the
//! [repository](https://github.com/noahbclarkson/asana-rs).

pub mod custom_field_value;
pub mod oauth;
pub mod resources;
pub mod webhooks;

mod auth;
mod client;
mod error;
mod html;
mod limits;
mod multipart;
mod page;
mod require;
mod retry;
mod send;
mod transport;

pub use asana_openapi as openapi;
/// Chooses the fields Asana returns, checked when compiled: `fields![Task: name, due_on]`.
pub use asana_openapi::fields;
/// The webhook protocol crate, re-exported so establishing a webhook (its filters and handshake
/// store) needs no second dependency.
pub use asana_webhooks;

pub use auth::{AccessToken, Auth, AuthError, TokenSource};
pub use client::{
    AsanaChange, BuildError, Client, ClientBuilder, DEFAULT_BASE_URL, DEFAULT_USER_AGENT,
};
pub use custom_field_value::{
    CustomDate, CustomFieldValue, EnumOptionRef, PersonRef, ResourceRef, TaskCustomField,
    TaskCustomFields, custom_fields_map,
};
pub use error::{ApiError, ApiErrors, DecodeError, DecodeErrorKind, Error, SyncToken};
pub use html::Html;
pub use limits::Limits;
pub use page::{Items, Page, Pages};
pub use require::Require;
pub use resources::custom_fields::NameMatch;
pub use resources::jobs::JobHandle;
pub use resources::workspaces::CustomFieldFilter;
pub use retry::RetryPolicy;
pub use send::decode_response;
pub use transport::{
    BoxError, RequestTimeout, ReqwestTransport, Transport, TransportError, TransportErrorKind,
};

/// Secret strings, re-exported so callers need not depend on `secrecy` to make an [`Auth`].
pub use secrecy::{ExposeSecret, SecretString};
