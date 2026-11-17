//! The contract between an endpoint descriptor and whatever sends it.
//!
//! A descriptor is plain data: the parameters of one API operation. It says how to turn itself
//! into a method, a path, a query and a body, and what type comes back. It does not send anything,
//! so retrying, rate limits, paging and authentication are written once, in the client, for every
//! operation alike.

use std::borrow::Cow;
use std::fmt;

use bytes::Bytes;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::scopes::Scope;

/// The HTTP methods Asana's API uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Method {
    /// `GET`
    Get,
    /// `POST`
    Post,
    /// `PUT`
    Put,
    /// `DELETE`
    Delete,
}

impl Method {
    /// The method as it is written on the wire.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Delete => "DELETE",
        }
    }
}

/// Where an operation's result sits in the response body.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Envelope {
    /// Under `data`, Asana's usual envelope. A paged list also carries `next_page` beside it.
    Data,
    /// The whole body is the result, because it carries siblings of `data` the caller needs
    /// (the events stream's `sync` and `has_more`, a webhook's `X-Hook-Secret`).
    Whole,
}

/// One query parameter: its name and its already-rendered value.
pub type QueryPair = (Cow<'static, str>, String);

/// What an operation sends as its body.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum RequestBody {
    /// No body.
    Empty,
    /// A JSON document, already wrapped in Asana's `{"data": ...}` envelope.
    Json(serde_json::Value),
    /// A `multipart/form-data` form (attachment upload).
    Multipart(Vec<Part>),
}

/// One part of a multipart form.
#[derive(Clone, Debug)]
pub struct Part {
    /// The form field's name.
    pub name: Cow<'static, str>,
    /// The field's value.
    pub value: PartValue,
}

/// The value of a multipart form field.
#[derive(Clone)]
pub enum PartValue {
    /// A plain text field.
    Text(String),
    /// A file.
    File {
        /// The name Asana shows for the file.
        file_name: String,
        /// The file's media type, for example `application/pdf`.
        content_type: String,
        /// The file's bytes.
        bytes: Bytes,
    },
}

impl fmt::Debug for PartValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Text(_) => f.write_str("Text(..)"),
            Self::File {
                content_type,
                bytes,
                ..
            } => f
                .debug_struct("File")
                .field("content_type", content_type)
                .field("len", &bytes.len())
                .finish_non_exhaustive(),
        }
    }
}

/// One operation of Asana's API, described as data.
///
/// Every operation in the specification has a generated type in [`crate::endpoints`] that
/// implements this.
#[diagnostic::on_unimplemented(
    message = "`{Self}` does not describe an Asana API operation",
    note = "use a type from `asana_openapi::endpoints`, or implement `Endpoint` for your own"
)]
pub trait Endpoint: Send + Sync {
    /// What the operation returns, with Asana's envelope removed (see [`Endpoint::ENVELOPE`]).
    type Output: DeserializeOwned + Send + 'static;

    /// The operation's `operationId` in the specification, for example `getTask`.
    const OPERATION_ID: &'static str;
    /// The HTTP method.
    const METHOD: Method;
    /// The path template, for example `/tasks/{task_gid}`. Used in traces; never holds a value.
    const ROUTE: &'static str;
    /// The granular OAuth scope the operation needs, where the specification names one.
    const SCOPE: Option<Scope>;
    /// Whether sending the operation twice leaves Asana as sending it once would, so it may be
    /// sent again after a failure that leaves the outcome unknown.
    const IDEMPOTENT: bool;
    /// Where the result sits in the response body.
    const ENVELOPE: Envelope = Envelope::Data;

    /// The path with its parameters filled in, starting with `/`.
    fn path(&self) -> String;

    /// The query parameters, `opt_fields` included, in a stable order.
    fn query(&self) -> Vec<QueryPair> {
        Vec::new()
    }

    /// The request body.
    ///
    /// # Errors
    ///
    /// A JSON error when a value in the request cannot be written as JSON.
    fn body(&self) -> Result<RequestBody, serde_json::Error> {
        Ok(RequestBody::Empty)
    }
}

/// An operation that returns one page of a list and takes an `offset` for the next.
///
/// Its [`Endpoint::Output`] is `Vec<Self::Item>`.
pub trait Paginated: Endpoint {
    /// One element of the list.
    type Item: DeserializeOwned + Send + 'static;

    /// Sets where the next page starts. `None` asks for the first page.
    fn set_offset(&mut self, offset: Option<Offset>);

    /// Sets how many objects a page holds.
    fn set_limit(&mut self, limit: PageLimit);

    /// The page size the descriptor asks for, when one was set. A client reading every page
    /// uses it, and chooses its own only when this is `None` (the default for a descriptor that
    /// does not say).
    fn limit(&self) -> Option<PageLimit> {
        None
    }
}

/// An opaque position in a paged list, handed back to Asana exactly as it was given. It expires.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Offset(String);

impl Offset {
    /// Wraps an offset Asana returned.
    pub fn new(offset: impl Into<String>) -> Self {
        Self(offset.into())
    }

    /// The offset as Asana wrote it.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// How many objects one page holds: 1 to 100.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PageLimit(u8);

impl PageLimit {
    /// The largest page Asana serves.
    pub const MAX: Self = Self(100);

    /// A page size, clamped to Asana's range of 1 to 100.
    pub const fn new(limit: u8) -> Self {
        if limit == 0 {
            Self(1)
        } else if limit > 100 {
            Self(100)
        } else {
            Self(limit)
        }
    }

    /// The page size.
    pub const fn get(self) -> u8 {
        self.0
    }
}

impl Default for PageLimit {
    fn default() -> Self {
        Self::MAX
    }
}

/// The `next_page` object beside a page of results.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[non_exhaustive]
pub struct NextPage {
    /// Where the next page starts.
    pub offset: Offset,
}
