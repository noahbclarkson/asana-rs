//! The one error every call returns, and how a response's status becomes one.
//!
//! Asana answers a failed request with `{"errors": [{"message", "help", "phrase"}]}`. Those
//! messages are kept in [`ApiErrors`] for a developer to read, but no `Display` here prints them,
//! a response body, a query value or a header value: bodies and messages can carry customer data.

use std::fmt;
use std::time::Duration;

use http::StatusCode;
use serde::{Deserialize, Serialize};

use crate::auth::AuthError;
use crate::transport::TransportError;

/// Everything that can go wrong sending an operation to Asana.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// `400`: Asana found the request malformed.
    #[error("Asana refused the request as malformed (400)")]
    BadRequest(ApiErrors),
    /// `401`: the token is missing, invalid or expired (after one refresh, for OAuth).
    #[error("Asana did not accept the token (401)")]
    Unauthorized(ApiErrors),
    /// `402`: the object or the change needs a paid Asana plan.
    #[error("the request needs a paid Asana plan (402)")]
    PremiumRequired(ApiErrors),
    /// `403`: no access to the object, or a scope missing for a field asked for.
    #[error("the token has no access to this (403)")]
    Forbidden(ApiErrors),
    /// `404`: the object does not exist, or the token cannot see it.
    #[error("Asana found no such object (404)")]
    NotFound(ApiErrors),
    /// `429` on every attempt, or a `Retry-After` longer than the retry policy waits.
    #[error(
        "Asana's rate limit was still in force (attempts: {attempts}, retry after {retry_after:?})"
    )]
    RateLimited {
        /// How long Asana asked the client to wait.
        retry_after: Duration,
        /// How many times the request was sent.
        attempts: u32,
    },
    /// A `5xx` answer, after any retries the method allows.
    #[error("Asana failed with {status} (attempts: {attempts})")]
    Server {
        /// The status code.
        status: StatusCode,
        /// The phrase Asana gives a `500` so its support can find the failure.
        phrase: Option<String>,
        /// How many times the request was sent.
        attempts: u32,
    },
    /// `451`: Asana will not serve requests from where this one came from.
    #[error("Asana refuses requests from this location (451)")]
    LegalBlock(ApiErrors),
    /// `412` from the events endpoint: the sync token is missing or too old. Start reading from
    /// `sync`, and re-read whatever the events were standing in for.
    #[error("the events sync token is missing or has expired (412)")]
    SyncTokenExpired {
        /// The sync token to read events from now on.
        sync: SyncToken,
    },
    /// A field the caller needs was not in the response (see [`Require`](crate::Require)).
    #[error("the response has no `{0}`")]
    MissingField(&'static str),
    /// A response body did not have the shape the operation describes.
    #[error("could not read the response to {operation} at `{path}`")]
    Decode {
        /// The operation's `operationId`.
        operation: &'static str,
        /// The JSON path of the value that failed, for example `data.memberships[0].section`.
        path: String,
        /// What was wrong there.
        source: DecodeError,
    },
    /// The request body could not be written as JSON.
    #[error("could not write the request body for {operation}")]
    Encode {
        /// The operation's `operationId`.
        operation: &'static str,
        /// The serialisation error.
        source: serde_json::Error,
    },
    /// The request could not be built (a path that does not make a URL).
    #[error("the request for {operation} could not be built: {reason}")]
    InvalidRequest {
        /// The operation's `operationId`.
        operation: &'static str,
        /// What was wrong, in words that hold no value from the request.
        reason: &'static str,
    },
    /// No response came back, after any retries the method allows.
    #[error("no response came back from Asana (attempts: {attempts})")]
    Transport {
        /// What went wrong.
        source: TransportError,
        /// How many times the request was sent.
        attempts: u32,
    },
    /// No usable token could be had.
    #[error(transparent)]
    Auth(#[from] AuthError),
    /// Any other status Asana answered with.
    #[error("Asana answered with unexpected status {status}")]
    UnexpectedStatus {
        /// The status code.
        status: StatusCode,
        /// Asana's messages, if the body held any.
        errors: ApiErrors,
    },
}

impl Error {
    /// The HTTP status Asana answered with, when the error came from one.
    pub fn status(&self) -> Option<StatusCode> {
        Some(match self {
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::Unauthorized(_) => StatusCode::UNAUTHORIZED,
            Self::PremiumRequired(_) => StatusCode::PAYMENT_REQUIRED,
            Self::Forbidden(_) => StatusCode::FORBIDDEN,
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::RateLimited { .. } => StatusCode::TOO_MANY_REQUESTS,
            Self::LegalBlock(_) => StatusCode::UNAVAILABLE_FOR_LEGAL_REASONS,
            Self::SyncTokenExpired { .. } => StatusCode::PRECONDITION_FAILED,
            Self::Server { status, .. } | Self::UnexpectedStatus { status, .. } => *status,
            Self::MissingField(_)
            | Self::Decode { .. }
            | Self::Encode { .. }
            | Self::InvalidRequest { .. }
            | Self::Transport { .. }
            | Self::Auth(_) => return None,
        })
    }

    /// Whether the same request could succeed later: a rate limit, a server failure or a
    /// transport failure. The client has already retried what its policy allows.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::RateLimited { .. } | Self::Server { .. } => true,
            Self::Transport { source, .. } => source.is_transient(),
            Self::Auth(AuthError::Unavailable { .. }) => true,
            _ => false,
        }
    }

    /// Whether Asana answered `404`.
    pub fn is_not_found(&self) -> bool {
        matches!(self, Self::NotFound(_))
    }

    /// Asana's messages, when the error came from a response that carried them.
    pub fn api_errors(&self) -> Option<&ApiErrors> {
        match self {
            Self::BadRequest(errors)
            | Self::Unauthorized(errors)
            | Self::PremiumRequired(errors)
            | Self::Forbidden(errors)
            | Self::NotFound(errors)
            | Self::LegalBlock(errors)
            | Self::UnexpectedStatus { errors, .. } => Some(errors),
            _ => None,
        }
    }

    /// The error for a final, non-success answer. `attempts` is how many times it was sent.
    pub(crate) fn from_status(status: StatusCode, body: &[u8], attempts: u32) -> Self {
        let errors = ApiErrors::parse(body);
        match status.as_u16() {
            400 => Self::BadRequest(errors),
            401 => Self::Unauthorized(errors),
            402 => Self::PremiumRequired(errors),
            403 => Self::Forbidden(errors),
            404 => Self::NotFound(errors),
            451 => Self::LegalBlock(errors),
            412 => match serde_json::from_slice::<SyncBody>(body) {
                Ok(SyncBody { sync: Some(sync) }) => Self::SyncTokenExpired { sync },
                _ => Self::UnexpectedStatus { status, errors },
            },
            500..=599 => Self::Server {
                status,
                phrase: errors.errors.iter().find_map(|error| error.phrase.clone()),
                attempts,
            },
            _ => Self::UnexpectedStatus { status, errors },
        }
    }
}

/// The body of the events endpoint's `412`, which carries a fresh sync token.
#[derive(Deserialize)]
struct SyncBody {
    sync: Option<SyncToken>,
}

/// The messages in Asana's error body.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[non_exhaustive]
pub struct ApiErrors {
    /// Each error Asana reported.
    #[serde(default)]
    pub errors: Vec<ApiError>,
}

impl ApiErrors {
    /// Reads Asana's error body, or nothing when the body is not one.
    fn parse(body: &[u8]) -> Self {
        serde_json::from_slice(body).unwrap_or_default()
    }

    /// The messages, in the order Asana gave them.
    pub fn messages(&self) -> impl Iterator<Item = &str> {
        self.errors
            .iter()
            .filter_map(|error| error.message.as_deref())
    }
}

/// One error Asana reported.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[non_exhaustive]
pub struct ApiError {
    /// What went wrong, for a developer.
    pub message: Option<String>,
    /// Where to read more.
    pub help: Option<String>,
    /// On a `500`, a phrase Asana's support can use to find the failure.
    pub phrase: Option<String>,
}

/// The events endpoint's opaque position in a resource's stream of events, passed back exactly
/// as Asana gave it.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SyncToken(String);

impl SyncToken {
    /// Wraps a sync token Asana returned.
    pub fn new(token: impl Into<String>) -> Self {
        Self(token.into())
    }

    /// The token as Asana wrote it.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// What was wrong with a response body, without the value that was wrong.
///
/// A JSON library's message often quotes the offending value, and a response body holds
/// customer data, so only the kind of failure, its position and (for a missing field) the field's
/// name are kept.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodeError {
    kind: DecodeErrorKind,
    line: usize,
    column: usize,
}

/// The kind of a [`DecodeError`].
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum DecodeErrorKind {
    /// The body is not JSON.
    Syntax,
    /// The body ended early.
    Eof,
    /// A field the type needs is absent.
    MissingField(String),
    /// A value has the wrong type or an unacceptable value.
    Data,
}

impl DecodeError {
    pub(crate) fn from_json(error: &serde_json::Error) -> Self {
        let kind = match error.classify() {
            serde_json::error::Category::Syntax | serde_json::error::Category::Io => {
                DecodeErrorKind::Syntax
            }
            serde_json::error::Category::Eof => DecodeErrorKind::Eof,
            serde_json::error::Category::Data => {
                missing_field(error).map_or(DecodeErrorKind::Data, DecodeErrorKind::MissingField)
            }
        };
        Self {
            kind,
            line: error.line(),
            column: error.column(),
        }
    }

    /// The kind of failure.
    pub fn kind(&self) -> &DecodeErrorKind {
        &self.kind
    }

    /// The line of the body where it failed, from 1.
    pub fn line(&self) -> usize {
        self.line
    }

    /// The column of the body where it failed, from 1.
    pub fn column(&self) -> usize {
        self.column
    }
}

/// The field named by serde's "missing field `x`" message: a name from our own types, never data.
fn missing_field(error: &serde_json::Error) -> Option<String> {
    let message = error.to_string();
    let rest = message.strip_prefix("missing field `")?;
    let end = rest.find('`')?;
    Some(rest[..end].to_owned())
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            DecodeErrorKind::Syntax => f.write_str("the body is not valid JSON")?,
            DecodeErrorKind::Eof => f.write_str("the body ended early")?,
            DecodeErrorKind::MissingField(field) => write!(f, "missing field `{field}`")?,
            DecodeErrorKind::Data => f.write_str("a value has the wrong type or form")?,
        }
        write!(f, " (line {}, column {})", self.line, self.column)
    }
}

impl std::error::Error for DecodeError {}
