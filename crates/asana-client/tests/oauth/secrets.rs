//! No secret reaches `Debug` or `Display`.

use std::sync::Arc;

use asana_client::oauth::{
    AuthorizationCode, InMemoryTokenStore, OAuthError, OAuthErrorCode, PkcePair, PkceVerifier,
    RefreshToken, RefreshingTokenSource, State,
};
use asana_client::{AccessToken, Auth};

use crate::fixtures::{ACCESS, CODE, REFRESH, app, assert_no_secret, stored};
use crate::support::{Scripted, reply};

#[test]
fn no_debug_shows_a_token_a_secret_a_code_or_a_verifier() {
    let transport = Scripted::new(|_, _| Ok(reply(500, "")));
    let app = app(transport);
    let pkce = PkcePair::from_verifier(
        PkceVerifier::new("acme-verifier-not-real-acme-verifier-not-real").unwrap(),
    );
    let store = Arc::new(InMemoryTokenStore::new(stored()));
    let source = RefreshingTokenSource::new(app.clone(), store.clone());
    let shown = [
        format!("{app:?}"),
        format!("{pkce:?}"),
        format!("{:?}", pkce.verifier()),
        format!("{:?}", State::new("acme-state-not-real")),
        format!("{:?}", AuthorizationCode::new(CODE)),
        format!("{:?}", RefreshToken::new(REFRESH)),
        format!("{:?}", AccessToken::new(ACCESS)),
        format!("{:?}", stored()),
        format!("{store:?}"),
        format!("{source:?}"),
        format!("{:?}", Auth::oauth(source)),
    ];
    for text in &shown {
        assert_no_secret(text);
    }
    assert!(shown[0].contains("[REDACTED]") && shown[0].contains("1201234567890123"));
    insta::assert_snapshot!("app_debug", shown[0]);
}

#[test]
fn no_error_display_holds_a_description_or_an_unexpected_code() {
    let errors = [
        OAuthError::InvalidGrant {
            description: Some(format!("token {REFRESH} revoked")),
        },
        OAuthError::Denied {
            code: OAuthErrorCode::Other(format!("x {ACCESS}")),
            description: Some(ACCESS.into()),
        },
    ];
    for error in &errors {
        assert_no_secret(&error.to_string());
    }
}
