//! Where a person's tokens are kept between requests and between processes, and the clock their
//! expiry is read against.
//!
//! Storage is the caller's: a database row, a secrets manager, a file. [`TokenStore`] is the small
//! contract [`RefreshingTokenSource`](super::RefreshingTokenSource) needs from it. Saving is a
//! compare-and-swap on the refresh token, so when two processes refresh the same person's tokens at
//! once, the one that loses reads back the winner's tokens instead of overwriting them. Asana does
//! not document rotating refresh tokens, but if it ever sends a new one, overwriting it with a
//! stale one would leave the person disconnected.

use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};

use futures_util::future::BoxFuture;
use time::OffsetDateTime;

use super::tokens::RefreshToken;
use crate::auth::AccessToken;
use crate::transport::BoxError;

/// One person's tokens, as stored.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct StoredTokens {
    /// The current access token.
    pub access_token: AccessToken,
    /// The refresh token that replaces it.
    pub refresh_token: RefreshToken,
    /// When the access token expires.
    pub expires_at: OffsetDateTime,
}

impl StoredTokens {
    /// Tokens as read back from storage.
    pub fn new(
        access_token: AccessToken,
        refresh_token: RefreshToken,
        expires_at: OffsetDateTime,
    ) -> Self {
        Self {
            access_token,
            refresh_token,
            expires_at,
        }
    }
}

/// The outcome of [`TokenStore::replace`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Replaced {
    /// The stored refresh token was the expected one, and the new tokens are now stored.
    Saved,
    /// The stored refresh token was not the expected one (another process refreshed first, or the
    /// person reconnected), and nothing was written.
    Conflict,
}

/// Keeps one person's tokens.
///
/// A store holds the tokens of one connection; a service with many connected people makes one
/// store (and one [`RefreshingTokenSource`](super::RefreshingTokenSource)) per person.
pub trait TokenStore: Send + Sync + 'static {
    /// The stored tokens, or `None` when the person has not connected (or has disconnected).
    ///
    /// The error must not hold a token; it is passed on as
    /// [`AuthError::Unavailable`](crate::AuthError::Unavailable).
    fn load(&self) -> BoxFuture<'_, Result<Option<StoredTokens>, BoxError>>;

    /// Stores `tokens` only if the stored refresh token is still `expected`, atomically (for
    /// example `UPDATE … WHERE refresh_token = $expected`).
    fn replace<'a>(
        &'a self,
        expected: &'a RefreshToken,
        tokens: &'a StoredTokens,
    ) -> BoxFuture<'a, Result<Replaced, BoxError>>;
}

impl<T: TokenStore + ?Sized> TokenStore for Arc<T> {
    fn load(&self) -> BoxFuture<'_, Result<Option<StoredTokens>, BoxError>> {
        (**self).load()
    }

    fn replace<'a>(
        &'a self,
        expected: &'a RefreshToken,
        tokens: &'a StoredTokens,
    ) -> BoxFuture<'a, Result<Replaced, BoxError>> {
        (**self).replace(expected, tokens)
    }
}

/// A [`TokenStore`] in memory, for tests, examples and single-process tools.
#[derive(Default)]
pub struct InMemoryTokenStore {
    tokens: Mutex<Option<StoredTokens>>,
}

impl InMemoryTokenStore {
    /// A store holding `tokens`.
    pub fn new(tokens: StoredTokens) -> Self {
        Self {
            tokens: Mutex::new(Some(tokens)),
        }
    }

    /// A store holding nothing: the person has not connected.
    pub fn empty() -> Self {
        Self::default()
    }

    /// The stored tokens.
    pub fn get(&self) -> Option<StoredTokens> {
        self.lock().clone()
    }

    /// Replaces the stored tokens unconditionally (the person connected again).
    pub fn set(&self, tokens: Option<StoredTokens>) {
        *self.lock() = tokens;
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<StoredTokens>> {
        // The data is a plain value that is always whole, so a poisoned lock still holds it.
        self.tokens.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl fmt::Debug for InMemoryTokenStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InMemoryTokenStore")
            .field("tokens", &self.get())
            .finish()
    }
}

impl TokenStore for InMemoryTokenStore {
    fn load(&self) -> BoxFuture<'_, Result<Option<StoredTokens>, BoxError>> {
        let tokens = self.get();
        Box::pin(async move { Ok(tokens) })
    }

    fn replace<'a>(
        &'a self,
        expected: &'a RefreshToken,
        tokens: &'a StoredTokens,
    ) -> BoxFuture<'a, Result<Replaced, BoxError>> {
        let outcome = {
            let mut stored = self.lock();
            match stored.as_ref() {
                Some(current) if current.refresh_token == *expected => {
                    *stored = Some(tokens.clone());
                    Replaced::Saved
                }
                _ => Replaced::Conflict,
            }
        };
        Box::pin(async move { Ok(outcome) })
    }
}

/// The wall clock token expiry is read against. Tests supply one they can move.
pub trait Clock: Send + Sync + 'static {
    /// The time now.
    fn now(&self) -> OffsetDateTime;
}

/// The system's clock, in UTC.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }
}
