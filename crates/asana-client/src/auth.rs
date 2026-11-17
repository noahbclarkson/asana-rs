//! How the client proves who it is.
//!
//! Asana takes a bearer token from three kinds of credential: a personal access token, a service
//! account's token, and an OAuth access token that expires within the hour. The first two are
//! fixed strings. An OAuth token comes from a [`TokenSource`] the caller supplies, because where
//! tokens are stored and how a refresh is shared between processes is the caller's business.
//! When Asana answers `401`, the client tells the source the token was [rejected], asks it for a
//! token again and retries the request once.
//!
//! [rejected]: TokenSource::rejected

use std::borrow::Cow;
use std::fmt;
use std::sync::Arc;

use futures_util::future::BoxFuture;
use secrecy::{ExposeSecret, SecretString};

use crate::transport::BoxError;

/// The credential the client sends.
#[derive(Clone)]
pub enum Auth {
    /// A personal access token: the user's own access.
    PersonalAccessToken(SecretString),
    /// A service account's token: access to a whole Enterprise domain.
    ServiceAccount(SecretString),
    /// OAuth access tokens, from a source that refreshes them.
    OAuth(Arc<dyn TokenSource>),
}

impl Auth {
    /// A personal access token.
    pub fn personal_access_token(token: impl Into<SecretString>) -> Self {
        Self::PersonalAccessToken(token.into())
    }

    /// A service account's token.
    pub fn service_account(token: impl Into<SecretString>) -> Self {
        Self::ServiceAccount(token.into())
    }

    /// OAuth access tokens from `source`.
    pub fn oauth(source: impl TokenSource) -> Self {
        Self::OAuth(Arc::new(source))
    }

    /// The token to send now.
    pub(crate) async fn token(&self) -> Result<AccessToken, AuthError> {
        match self {
            Self::PersonalAccessToken(token) | Self::ServiceAccount(token) => {
                Ok(AccessToken(token.clone()))
            }
            Self::OAuth(source) => source.access_token().await,
        }
    }

    /// The source to tell about a rejected token, when the token can be replaced.
    pub(crate) fn refreshable(&self) -> Option<&dyn TokenSource> {
        match self {
            Self::OAuth(source) => Some(source.as_ref()),
            Self::PersonalAccessToken(_) | Self::ServiceAccount(_) => None,
        }
    }
}

impl fmt::Debug for Auth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PersonalAccessToken(_) => f.write_str("PersonalAccessToken([REDACTED])"),
            Self::ServiceAccount(_) => f.write_str("ServiceAccount([REDACTED])"),
            Self::OAuth(_) => f.write_str("OAuth(..)"),
        }
    }
}

/// Supplies OAuth access tokens and replaces one Asana has rejected.
///
/// The client calls [`access_token`](Self::access_token) before every attempt, so a source should
/// answer from memory while its token is fresh. On a `401` it calls
/// [`rejected`](Self::rejected) with the token Asana refused, then asks for a token again; a
/// source that shares its token between tasks should refresh only if the rejected token is still
/// the current one, so many tasks failing at once cause one refresh.
pub trait TokenSource: Send + Sync + 'static {
    /// The access token to send now.
    fn access_token(&self) -> BoxFuture<'_, Result<AccessToken, AuthError>>;

    /// Asana answered `401` to `token`: it has expired or been revoked. The next
    /// [`access_token`](Self::access_token) should not return it.
    fn rejected<'a>(&'a self, token: &'a AccessToken) -> BoxFuture<'a, ()>;
}

impl<T: TokenSource + ?Sized> TokenSource for Arc<T> {
    fn access_token(&self) -> BoxFuture<'_, Result<AccessToken, AuthError>> {
        (**self).access_token()
    }

    fn rejected<'a>(&'a self, token: &'a AccessToken) -> BoxFuture<'a, ()> {
        (**self).rejected(token)
    }
}

/// A bearer token as sent to Asana. Its `Debug` never shows it.
#[derive(Clone)]
pub struct AccessToken(SecretString);

impl AccessToken {
    /// Wraps a token.
    pub fn new(token: impl Into<SecretString>) -> Self {
        Self(token.into())
    }

    /// The token itself, for sending.
    pub fn secret(&self) -> &SecretString {
        &self.0
    }
}

impl PartialEq for AccessToken {
    fn eq(&self, other: &Self) -> bool {
        self.0.expose_secret() == other.0.expose_secret()
    }
}

impl Eq for AccessToken {}

impl fmt::Debug for AccessToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AccessToken([REDACTED])")
    }
}

impl From<SecretString> for AccessToken {
    fn from(token: SecretString) -> Self {
        Self(token)
    }
}

/// Why no usable access token could be had.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AuthError {
    /// The grant behind the token is gone (revoked, or the refresh token expired); the user must
    /// authorise the app again.
    #[error("the authorisation was revoked or has expired; the user must authorise the app again")]
    Revoked,
    /// A token could not be had now, for a reason that may pass (the token store or Asana's
    /// token endpoint is unreachable).
    #[error("no access token could be had: {reason}")]
    Unavailable {
        /// What failed, in words that never hold a secret.
        reason: Cow<'static, str>,
        /// The error that caused it.
        #[source]
        source: Option<BoxError>,
    },
    /// The token holds characters an HTTP header cannot carry.
    #[error("the access token cannot be sent in an HTTP header")]
    Malformed,
}

impl AuthError {
    /// A failure that may pass, described by `reason` (which must not hold a secret).
    pub fn unavailable(reason: impl Into<Cow<'static, str>>, source: Option<BoxError>) -> Self {
        Self::Unavailable {
            reason: reason.into(),
            source,
        }
    }
}
