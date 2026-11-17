//! What can go wrong building an OAuth app, sending a person to Asana and calling its token
//! endpoints.
//!
//! Asana's OAuth guide documents no error body for its token endpoints, so the body is read as
//! [RFC 6749 section 5.2](https://www.rfc-editor.org/rfc/rfc6749#section-5.2) defines it:
//! `{"error": "...", "error_description": "..."}`. No `Display` here holds a token, the client
//! secret, an authorisation code or a response body; an error's description is kept in a field for
//! a log the caller controls.

use std::fmt;

use http::StatusCode;

use crate::error::DecodeError;
use crate::transport::TransportError;

/// Why a call to Asana's OAuth endpoints, or reading the redirect back from Asana, failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum OAuthError {
    /// Asana answered `invalid_grant`: the authorisation code or refresh token is invalid, expired,
    /// already used or revoked. For a refresh token this means the person must connect again.
    #[error(
        "Asana refused the grant as invalid, expired or revoked; the person must authorise the app again"
    )]
    InvalidGrant {
        /// Asana's `error_description`, when it sent one.
        description: Option<String>,
    },
    /// Asana answered with an OAuth error other than `invalid_grant`.
    #[error("Asana's OAuth endpoint refused the request ({status}): {code}")]
    Refused {
        /// The HTTP status.
        status: StatusCode,
        /// The `error` code.
        code: OAuthErrorCode,
        /// Asana's `error_description`, when it sent one.
        description: Option<String>,
    },
    /// Asana answered with a status and a body that is not an OAuth error (a `5xx`, a `429`).
    #[error("Asana's OAuth endpoint answered {status}")]
    Status {
        /// The HTTP status.
        status: StatusCode,
    },
    /// A successful answer could not be read.
    #[error("Asana's OAuth answer could not be read: {source}")]
    Decode {
        /// Where and how reading failed.
        #[source]
        source: DecodeError,
    },
    /// No answer came back.
    #[error("the request to Asana's OAuth endpoint failed: {source}")]
    Transport {
        /// What the transport reported.
        #[source]
        source: TransportError,
    },
    /// The person, or Asana, declined the authorisation: the redirect carried `error` instead of
    /// a `code`.
    #[error("the authorisation was not granted: {code}")]
    Denied {
        /// The `error` code from the redirect.
        code: OAuthErrorCode,
        /// The `error_description` from the redirect, when present.
        description: Option<String>,
    },
    /// The redirect's `state` is missing or is not the one this flow sent: the request may be
    /// forged and must not be completed.
    #[error("the redirect's state does not match the one sent; the response was not completed")]
    StateMismatch,
    /// The redirect carried neither a `code` nor an `error`.
    #[error("the redirect carried no authorisation code")]
    MissingCode,
    /// A token answer that must carry a refresh token (the answer to exchanging a code) did not.
    #[error("Asana's token answer carried no refresh token")]
    MissingRefreshToken,
}

impl OAuthError {
    /// Whether the grant is dead and the person must authorise the app again.
    pub fn needs_reconnect(&self) -> bool {
        matches!(self, Self::InvalidGrant { .. })
    }

    /// Whether the same call may succeed if made again later (Asana was busy or unreachable).
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Status { status } => {
                status.is_server_error() || *status == StatusCode::TOO_MANY_REQUESTS
            }
            Self::Transport { source } => source.is_transient(),
            _ => false,
        }
    }
}

/// An OAuth `error` code, from a token endpoint's answer or from the redirect.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum OAuthErrorCode {
    /// `invalid_request`: a parameter is missing, repeated or malformed.
    InvalidRequest,
    /// `invalid_client`: the client id or secret is wrong.
    InvalidClient,
    /// `invalid_grant`: the code or refresh token is invalid, expired or revoked.
    InvalidGrant,
    /// `unauthorized_client`: the app may not use this grant type.
    UnauthorizedClient,
    /// `unsupported_grant_type`.
    UnsupportedGrantType,
    /// `invalid_scope`: a scope is unknown or not allowed.
    InvalidScope,
    /// `access_denied`: the person declined.
    AccessDenied,
    /// `forbidden_scopes`: the app asked for a scope it is not registered for, or for granular
    /// scopes while registered for full permissions (Asana's own code, on the redirect).
    ForbiddenScopes,
    /// A code this crate does not know, kept as Asana sent it.
    Other(String),
}

impl OAuthErrorCode {
    /// Reads an `error` value.
    pub fn parse(code: &str) -> Self {
        match code {
            "invalid_request" => Self::InvalidRequest,
            "invalid_client" => Self::InvalidClient,
            "invalid_grant" => Self::InvalidGrant,
            "unauthorized_client" => Self::UnauthorizedClient,
            "unsupported_grant_type" => Self::UnsupportedGrantType,
            "invalid_scope" => Self::InvalidScope,
            "access_denied" => Self::AccessDenied,
            "forbidden_scopes" => Self::ForbiddenScopes,
            other => Self::Other(other.to_owned()),
        }
    }

    /// The code as Asana writes it.
    pub fn as_str(&self) -> &str {
        match self {
            Self::InvalidRequest => "invalid_request",
            Self::InvalidClient => "invalid_client",
            Self::InvalidGrant => "invalid_grant",
            Self::UnauthorizedClient => "unauthorized_client",
            Self::UnsupportedGrantType => "unsupported_grant_type",
            Self::InvalidScope => "invalid_scope",
            Self::AccessDenied => "access_denied",
            Self::ForbiddenScopes => "forbidden_scopes",
            Self::Other(code) => code,
        }
    }
}

impl fmt::Display for OAuthErrorCode {
    /// Writes the code when it looks like one (lower-case letters and `_`, at most 64), so text
    /// that is not a code never reaches a log through `Display`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let code = self.as_str();
        let looks_like_a_code = !code.is_empty()
            && code.len() <= 64
            && code.bytes().all(|b| b.is_ascii_lowercase() || b == b'_');
        if looks_like_a_code {
            f.write_str(code)
        } else {
            f.write_str("an unrecognised error code")
        }
    }
}

/// Why an [`OAuthApp`](super::OAuthApp) could not be built.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum OAuthBuildError {
    /// The client id is empty.
    #[error("the OAuth client id is empty")]
    EmptyClientId,
    /// The redirect URI is not an absolute `https` URL, an `http` URL on the loopback host, or
    /// Asana's out-of-band URN.
    #[error(
        "the redirect URI must be an https URL, an http URL on localhost, or urn:ietf:wg:oauth:2.0:oob"
    )]
    InvalidRedirectUri,
    /// The origin is not an absolute `http` or `https` URL without a path, query or credentials.
    #[error("the OAuth origin must be an absolute http(s) URL with no path, query or credentials")]
    InvalidOrigin,
    /// The user agent holds characters a header cannot carry.
    #[error("the user agent cannot be sent in an HTTP header")]
    InvalidUserAgent,
    /// The default transport could not be built.
    #[error("the default transport could not be built: {0}")]
    Transport(#[source] TransportError),
}
