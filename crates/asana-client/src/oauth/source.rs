//! A [`TokenSource`] that keeps one person's OAuth tokens fresh.
//!
//! It answers from memory while the access token has more than a margin left, and otherwise
//! refreshes it before Asana would refuse it. Asana's access tokens last an hour. Refreshing is
//! single-flight: the source holds one async lock while it reads the store and refreshes, so ten
//! tasks that find the token stale at once cause one refresh, and the other nine take its result.
//! Between processes there is no lock. Before refreshing, the source reads the store in case
//! another process already has, which avoids most second refreshes. The [`TokenStore`]'s
//! compare-and-swap is on the refresh token, which Asana does not change when it refreshes, so
//! two processes that refresh at the same moment both save and the last one wins; both access
//! tokens stay valid. The swap is lost, and the source takes what is stored, only when the
//! refresh token itself changed: the person connected again, or Asana rotated it.
//!
//! A dead refresh token (`invalid_grant`) becomes [`AuthError::Revoked`]: the person must connect
//! again. Before saying so the source reads the store once more, because `invalid_grant` is also
//! what a refresh token that another process has just replaced would get, if Asana rotated them.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use futures_util::future::BoxFuture;

use super::app::OAuthApp;
use super::error::OAuthError;
use super::store::{Clock, Replaced, StoredTokens, SystemClock, TokenStore};
use crate::auth::{AccessToken, AuthError, TokenSource};
use crate::transport::BoxError;

/// How long before expiry an access token is refreshed, unless told otherwise.
pub const DEFAULT_REFRESH_MARGIN: Duration = Duration::from_secs(5 * 60);

/// Supplies a client with one person's access tokens, refreshing them through an [`OAuthApp`] and
/// keeping them in a [`TokenStore`].
///
/// ```no_run
/// # fn run(app: asana_client::oauth::OAuthApp, store: asana_client::oauth::InMemoryTokenStore)
/// # -> Result<(), Box<dyn std::error::Error>> {
/// use asana_client::oauth::RefreshingTokenSource;
/// use asana_client::{Auth, Client};
///
/// let client = Client::builder()
///     .auth(Auth::oauth(RefreshingTokenSource::new(app, store)))
///     .build()?;
/// # Ok(()) }
/// ```
pub struct RefreshingTokenSource<S: TokenStore> {
    app: OAuthApp,
    store: S,
    clock: Arc<dyn Clock>,
    margin: Duration,
    state: tokio::sync::Mutex<Cache>,
}

/// What the source remembers between calls.
#[derive(Default)]
struct Cache {
    /// The tokens last read or refreshed.
    tokens: Option<StoredTokens>,
    /// The access token Asana last refused, which is never handed out again.
    rejected: Option<AccessToken>,
}

impl<S: TokenStore> RefreshingTokenSource<S> {
    /// A source refreshing through `app` and keeping tokens in `store`, on the system clock, with
    /// a [`DEFAULT_REFRESH_MARGIN`].
    pub fn new(app: OAuthApp, store: S) -> Self {
        Self {
            app,
            store,
            clock: Arc::new(SystemClock),
            margin: DEFAULT_REFRESH_MARGIN,
            state: tokio::sync::Mutex::new(Cache::default()),
        }
    }

    /// The source reading expiry against `clock`.
    #[must_use]
    pub fn with_clock(mut self, clock: impl Clock) -> Self {
        self.clock = Arc::new(clock);
        self
    }

    /// The source refreshing a token once it has `margin` or less left.
    #[must_use]
    pub fn with_margin(mut self, margin: Duration) -> Self {
        self.margin = margin;
        self
    }

    /// The store the source keeps tokens in.
    pub fn store(&self) -> &S {
        &self.store
    }

    /// The app the source refreshes through.
    pub fn app(&self) -> &OAuthApp {
        &self.app
    }

    async fn current(&self) -> Result<AccessToken, AuthError> {
        let mut cache = self.state.lock().await;
        if let Some(tokens) = &cache.tokens
            && self.usable(tokens, cache.rejected.as_ref())
        {
            return Ok(tokens.access_token.clone());
        }
        // Another process may have refreshed since this one last looked.
        let stored = self.load().await?.ok_or(AuthError::Revoked)?;
        let tokens = if self.usable(&stored, cache.rejected.as_ref()) {
            stored
        } else {
            self.refresh(stored).await?
        };
        let access_token = tokens.access_token.clone();
        cache.tokens = Some(tokens);
        Ok(access_token)
    }

    /// Whether `tokens` may be handed out: not refused by Asana, and with more than the margin
    /// left.
    fn usable(&self, tokens: &StoredTokens, rejected: Option<&AccessToken>) -> bool {
        if rejected == Some(&tokens.access_token) {
            return false;
        }
        time::Duration::try_from(self.margin)
            .ok()
            .and_then(|margin| self.clock.now().checked_add(margin))
            .is_some_and(|deadline| deadline < tokens.expires_at)
    }

    /// Refreshes `stored` and saves the result, or takes what another process saved.
    async fn refresh(&self, stored: StoredTokens) -> Result<StoredTokens, AuthError> {
        // Expiry counts from before the request, so a slow answer errs towards refreshing early.
        let issued_at = self.clock.now();
        match self.app.refresh(&stored.refresh_token).await {
            Ok(response) => {
                let next = StoredTokens {
                    expires_at: response.expires_at(issued_at),
                    refresh_token: response
                        .refresh_token
                        .unwrap_or_else(|| stored.refresh_token.clone()),
                    access_token: response.access_token,
                };
                let outcome = self
                    .store
                    .replace(&stored.refresh_token, &next)
                    .await
                    .map_err(store_failed)?;
                match outcome {
                    Replaced::Saved => Ok(next),
                    Replaced::Conflict => {
                        tracing::debug!("asana.oauth: another refresh won; using its tokens");
                        self.load().await?.ok_or(AuthError::Revoked)
                    }
                }
            }
            Err(OAuthError::InvalidGrant { .. }) => match self.load().await? {
                Some(current) if current.refresh_token != stored.refresh_token => Ok(current),
                _ => Err(AuthError::Revoked),
            },
            Err(error) => Err(AuthError::unavailable(
                "refreshing the OAuth access token failed",
                Some(Box::new(error)),
            )),
        }
    }

    async fn load(&self) -> Result<Option<StoredTokens>, AuthError> {
        self.store.load().await.map_err(store_failed)
    }
}

fn store_failed(error: BoxError) -> AuthError {
    AuthError::unavailable("the OAuth token store failed", Some(error))
}

impl<S: TokenStore> TokenSource for RefreshingTokenSource<S> {
    fn access_token(&self) -> BoxFuture<'_, Result<AccessToken, AuthError>> {
        Box::pin(self.current())
    }

    fn rejected<'a>(&'a self, token: &'a AccessToken) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let mut cache = self.state.lock().await;
            // A refusal of a token already replaced says nothing about the current one.
            let current = cache
                .tokens
                .as_ref()
                .is_none_or(|tokens| tokens.access_token == *token);
            if current {
                cache.rejected = Some(token.clone());
            }
        })
    }
}

impl<S: TokenStore> fmt::Debug for RefreshingTokenSource<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RefreshingTokenSource")
            .field("app", &self.app)
            .field("margin", &self.margin)
            .finish_non_exhaustive()
    }
}
