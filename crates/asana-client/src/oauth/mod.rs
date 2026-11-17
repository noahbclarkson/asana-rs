//! OAuth: the authorisation URL with PKCE, exchanging and refreshing tokens, revoking and
//! introspecting them, keeping them fresh for a [`Client`](crate::Client), and choosing scopes.
//!
//! Asana's flow is the authorisation code grant
//! ([its guide](https://developers.asana.com/docs/oauth)):
//!
//! 1. Send the person to [`OAuthApp::authorize_url`] with a fresh [`State`], a fresh [`PkcePair`]'s
//!    challenge and the [`ScopeSet`] the app needs ([`scopes_for!`](crate::scopes_for) works it
//!    out from the operations the app calls). Keep the state and the verifier with their session.
//! 2. Asana redirects them back. [`read_redirect`] checks the `state` and returns the code.
//! 3. [`OAuthApp::exchange`] trades the code and verifier for an access token (an hour), a refresh
//!    token and who the person is. Store them ([`TokenResponse::into_stored`]).
//! 4. Build the client with `Auth::oauth(RefreshingTokenSource::new(app, store))`. It refreshes
//!    the access token before it expires, and again whenever Asana answers `401`; when the refresh
//!    token is dead it returns [`AuthError::Revoked`](crate::AuthError::Revoked) and the person
//!    must connect again.
//! 5. [`OAuthApp::revoke`] disconnects them.
//!
//! Nothing here stores anything: [`TokenStore`] is implemented over the caller's own storage.

mod app;
mod error;
mod pkce;
mod scopes;
mod source;
mod store;
mod tokens;

pub use app::{DEFAULT_ORIGIN, OAuthApp, OAuthAppBuilder, OUT_OF_BAND_REDIRECT_URI};
pub use error::{OAuthBuildError, OAuthError, OAuthErrorCode};
pub use pkce::{InvalidVerifier, PkceChallenge, PkcePair, PkceVerifier, State};
pub use scopes::{ScopeSet, required_scope};
pub use source::{DEFAULT_REFRESH_MARGIN, RefreshingTokenSource};
pub use store::{Clock, InMemoryTokenStore, Replaced, StoredTokens, SystemClock, TokenStore};
pub use tokens::{
    AuthorizationCode, DEFAULT_ACCESS_TOKEN_LIFETIME, RefreshToken, TokenInfo, TokenResponse,
    TokenUser, read_redirect,
};
