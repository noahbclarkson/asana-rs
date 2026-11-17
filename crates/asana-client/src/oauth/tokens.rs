//! The values Asana's OAuth endpoints hand out: authorisation codes, access and refresh tokens, the
//! token endpoint's answer and the introspection endpoint's answer.
//!
//! Every token is a [`secrecy`] type whose `Debug` prints nothing of it. Asana says its tokens are
//! opaque and their format may change, so none is ever checked for shape.

use std::fmt;
use std::time::Duration;

use asana_openapi::ids::UserGid;
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use subtle::ConstantTimeEq as _;
use time::OffsetDateTime;

use super::error::{OAuthError, OAuthErrorCode};
use super::pkce::State;
use super::scopes::ScopeSet;
use super::store::StoredTokens;
use crate::auth::AccessToken;

/// How long Asana's access tokens last when an answer does not say: its guide documents one hour.
pub const DEFAULT_ACCESS_TOKEN_LIFETIME: Duration = Duration::from_secs(3600);

/// A long-lived refresh token, exchanged for new access tokens. Its `Debug` never shows it.
#[derive(Clone)]
pub struct RefreshToken(SecretString);

impl RefreshToken {
    /// Wraps a refresh token.
    pub fn new(token: impl Into<SecretString>) -> Self {
        Self(token.into())
    }

    /// The token itself, for storing or sending.
    pub fn secret(&self) -> &SecretString {
        &self.0
    }
}

impl PartialEq for RefreshToken {
    /// Compared in constant time.
    fn eq(&self, other: &Self) -> bool {
        let (a, b) = (self.0.expose_secret(), other.0.expose_secret());
        a.len() == b.len() && bool::from(a.as_bytes().ct_eq(b.as_bytes()))
    }
}

impl Eq for RefreshToken {}

impl fmt::Debug for RefreshToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RefreshToken([REDACTED])")
    }
}

impl From<SecretString> for RefreshToken {
    fn from(token: SecretString) -> Self {
        Self(token)
    }
}

/// The single-use `code` Asana's redirect carries, exchanged for tokens. Its `Debug` never shows
/// it.
#[derive(Clone)]
pub struct AuthorizationCode(SecretString);

impl AuthorizationCode {
    /// Wraps a code read from the redirect.
    pub fn new(code: impl Into<SecretString>) -> Self {
        Self(code.into())
    }

    /// The code itself.
    pub fn secret(&self) -> &SecretString {
        &self.0
    }
}

impl fmt::Debug for AuthorizationCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AuthorizationCode([REDACTED])")
    }
}

/// Reads the query string Asana redirects the person back with, and checks its `state`.
///
/// Asana sends `code` and `state` on success; when the person declines, or the request was wrong
/// (a scope the app is not registered for), it sends `error` and usually `error_description`.
///
/// # Errors
///
/// [`OAuthError::Denied`] when the redirect carries `error`, [`OAuthError::StateMismatch`] when its
/// `state` is missing or is not `expected` (checked before anything else, in constant time), and
/// [`OAuthError::MissingCode`] when it carries no `code`.
pub fn read_redirect(query: &str, expected: &State) -> Result<AuthorizationCode, OAuthError> {
    let query = query.strip_prefix('?').unwrap_or(query);
    let mut code = None;
    let mut state = None;
    let mut error = None;
    let mut description = None;
    for (name, value) in url::form_urlencoded::parse(query.as_bytes()) {
        match name.as_ref() {
            "code" => code = Some(value.into_owned()),
            "state" => state = Some(value.into_owned()),
            "error" => error = Some(value.into_owned()),
            "error_description" => description = Some(value.into_owned()),
            _ => {}
        }
    }
    if !state.is_some_and(|state| expected.matches(&state)) {
        return Err(OAuthError::StateMismatch);
    }
    if let Some(error) = error {
        return Err(OAuthError::Denied {
            code: OAuthErrorCode::parse(&error),
            description,
        });
    }
    code.map(|code| AuthorizationCode(SecretString::from(code)))
        .ok_or(OAuthError::MissingCode)
}

/// The token endpoint's answer to exchanging a code or a refresh token.
///
/// Asana's guide documents the refresh token only in the answer to exchanging a code; the answer
/// to a refresh carries a new access token alone, so Asana's refresh tokens are not documented to
/// rotate. If an answer does carry one it is kept, and
/// [`RefreshingTokenSource`](super::RefreshingTokenSource) stores it.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct TokenResponse {
    /// The bearer token for API requests.
    pub access_token: AccessToken,
    /// The refresh token, always present when a code was exchanged.
    pub refresh_token: Option<RefreshToken>,
    /// How long the access token lasts from when it was issued (Asana: typically one hour).
    pub expires_in: Option<Duration>,
    /// The token type, `bearer`.
    pub token_type: String,
    /// The user who authorised the app, from the answer's `data`.
    pub user: Option<TokenUser>,
}

impl TokenResponse {
    /// When the access token expires, counting from `issued_at`; one hour if Asana did not say.
    pub fn expires_at(&self, issued_at: OffsetDateTime) -> OffsetDateTime {
        expiry(issued_at, self.expires_in)
    }

    /// The tokens to store after exchanging a code, with the access token's expiry counted from
    /// `issued_at`.
    ///
    /// # Errors
    ///
    /// [`OAuthError::MissingRefreshToken`] when the answer carries no refresh token.
    pub fn into_stored(self, issued_at: OffsetDateTime) -> Result<StoredTokens, OAuthError> {
        let expires_at = self.expires_at(issued_at);
        let refresh_token = self.refresh_token.ok_or(OAuthError::MissingRefreshToken)?;
        Ok(StoredTokens {
            access_token: self.access_token,
            refresh_token,
            expires_at,
        })
    }
}

/// `issued_at + lifetime`, an hour when the lifetime is unknown; `issued_at` itself (so the token
/// counts as expired) if the sum cannot be represented.
pub(crate) fn expiry(issued_at: OffsetDateTime, lifetime: Option<Duration>) -> OffsetDateTime {
    time::Duration::try_from(lifetime.unwrap_or(DEFAULT_ACCESS_TOKEN_LIFETIME))
        .ok()
        .and_then(|lifetime| issued_at.checked_add(lifetime))
        .unwrap_or(issued_at)
}

/// The user who authorised the app, as the token answer's `data` describes them.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[non_exhaustive]
pub struct TokenUser {
    /// The user's GID.
    pub gid: Option<UserGid>,
    /// The user's name.
    pub name: Option<String>,
    /// The user's email address.
    pub email: Option<String>,
}

/// The token endpoint's answer as JSON. Tokens are wrapped as secrets as soon as it is read.
#[derive(Deserialize)]
pub(crate) struct RawTokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<u64>,
    token_type: Option<String>,
    data: Option<TokenUser>,
}

impl From<RawTokenResponse> for TokenResponse {
    fn from(raw: RawTokenResponse) -> Self {
        Self {
            access_token: AccessToken::new(raw.access_token),
            refresh_token: raw
                .refresh_token
                .map(|t| RefreshToken(SecretString::from(t))),
            expires_in: raw.expires_in.map(Duration::from_secs),
            token_type: raw.token_type.unwrap_or_else(|| "bearer".to_owned()),
            user: raw.data,
        }
    }
}

/// What the introspection endpoint (`/-/token_info`) says about a token.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct TokenInfo {
    /// Whether the token is valid now: `false` once it has expired or been revoked.
    pub active: bool,
    /// `bearer` (an OAuth access token, personal access token or service account token) or
    /// `refresh` (an OAuth refresh token).
    pub token_type: Option<String>,
    /// How long the token has left.
    pub expires_in: Option<Duration>,
    /// When the token expires.
    pub expires_at: Option<OffsetDateTime>,
    /// The scopes the token carries.
    pub scope: ScopeSet,
    /// The OAuth app the token belongs to; only OAuth tokens have one.
    pub client_id: Option<String>,
}

/// The introspection endpoint's answer as JSON.
#[derive(Deserialize)]
pub(crate) struct RawTokenInfo {
    active: bool,
    token_type: Option<String>,
    expires_in: Option<u64>,
    exp: Option<i64>,
    scope: Option<String>,
    client_id: Option<serde_json::Value>,
}

impl From<RawTokenInfo> for TokenInfo {
    fn from(raw: RawTokenInfo) -> Self {
        Self {
            active: raw.active,
            token_type: raw.token_type,
            expires_in: raw.expires_in.map(Duration::from_secs),
            expires_at: raw
                .exp
                .and_then(|exp| OffsetDateTime::from_unix_timestamp(exp).ok()),
            scope: raw
                .scope
                .as_deref()
                .map(ScopeSet::parse)
                .unwrap_or_default(),
            // Asana's client ids are digits; read one sent as a number or a string alike.
            client_id: raw.client_id.and_then(|id| match id {
                serde_json::Value::String(id) => Some(id),
                serde_json::Value::Number(id) => Some(id.to_string()),
                _ => None,
            }),
        }
    }
}
