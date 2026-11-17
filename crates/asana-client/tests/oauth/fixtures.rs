//! Made-up credentials, an app on a scripted transport, a clock the tests move, and Asana's
//! documented token answers with invented values.

use std::sync::{Arc, Mutex};

use asana_client::AccessToken;
use asana_client::oauth::{Clock, OAuthApp, RefreshToken, StoredTokens};
use bytes::Bytes;
use time::OffsetDateTime;
use time::macros::datetime;

use crate::support::Scripted;

pub(crate) const CLIENT_ID: &str = "1201234567890123";
pub(crate) const CLIENT_SECRET: &str = "acme-client-secret-not-real";
pub(crate) const REDIRECT: &str = "https://acme.example.com/asana/callback";
pub(crate) const ACCESS: &str = "acme-access-token-not-real";
pub(crate) const FRESH_ACCESS: &str = "acme-fresh-access-not-real";
pub(crate) const REFRESH: &str = "acme-refresh-token-not-real";
pub(crate) const CODE: &str = "acme-auth-code-not-real";

/// Every secret the tests use, for checking none is printed.
pub(crate) const SECRETS: [&str; 6] = [CLIENT_SECRET, ACCESS, FRESH_ACCESS, REFRESH, CODE, "acme-"];

/// When the tests start.
pub(crate) const START: OffsetDateTime = datetime!(2026-03-02 09:00 UTC);

pub(crate) fn app(transport: Arc<Scripted>) -> OAuthApp {
    OAuthApp::builder()
        .client_id(CLIENT_ID)
        .client_secret(CLIENT_SECRET)
        .redirect_uri(REDIRECT)
        .transport(transport)
        .build()
        .unwrap()
}

/// A clock the test moves by hand.
#[derive(Clone)]
pub(crate) struct TestClock(Arc<Mutex<OffsetDateTime>>);

impl TestClock {
    pub(crate) fn at(now: OffsetDateTime) -> Self {
        Self(Arc::new(Mutex::new(now)))
    }

    pub(crate) fn advance(&self, by: time::Duration) {
        *self.0.lock().unwrap() += by;
    }
}

impl Clock for TestClock {
    fn now(&self) -> OffsetDateTime {
        *self.0.lock().unwrap()
    }
}

/// Tokens issued at [`START`], lasting an hour.
pub(crate) fn stored() -> StoredTokens {
    StoredTokens::new(
        AccessToken::new(ACCESS),
        RefreshToken::new(REFRESH),
        START + time::Duration::hours(1),
    )
}

/// The token endpoint's answer to exchanging a code, shaped as Asana's guide documents it.
pub(crate) fn exchange_answer() -> String {
    format!(
        r#"{{
            "access_token": "{ACCESS}",
            "expires_in": 3600,
            "token_type": "bearer",
            "data": {{"id": 1201234567890999, "gid": "1201234567890999", "name": "Acme User", "email": "user@example.com"}},
            "refresh_token": "{REFRESH}"
        }}"#
    )
}

/// The answer to a refresh: a new access token and no refresh token.
pub(crate) fn refresh_answer() -> String {
    format!(
        r#"{{"access_token": "{FRESH_ACCESS}", "expires_in": 3600, "token_type": "bearer",
            "data": {{"id": 1201234567890999, "gid": "1201234567890999", "name": "Acme User", "email": "user@example.com"}}}}"#
    )
}

/// The form fields of a request body, in order.
pub(crate) fn form(body: &Bytes) -> Vec<(String, String)> {
    url::form_urlencoded::parse(body)
        .map(|(name, value)| (name.into_owned(), value.into_owned()))
        .collect()
}

/// A request as a snapshot shows it: method, URL, content type and form fields, with every value
/// that is a secret replaced by `[REDACTED]`.
pub(crate) fn shown(request: &http::Request<Bytes>) -> String {
    let mut text = format!(
        "{} {}\ncontent-type: {}\n",
        request.method(),
        request.uri(),
        request.headers()["content-type"].to_str().unwrap()
    );
    for (name, value) in form(request.body()) {
        let secret = matches!(
            name.as_str(),
            "client_secret" | "code" | "code_verifier" | "refresh_token" | "token"
        );
        let value = if secret {
            "[REDACTED]".to_owned()
        } else {
            value
        };
        text.push_str(&format!("{name}={value}\n"));
    }
    text
}

/// Fails if `text` contains any test secret.
pub(crate) fn assert_no_secret(text: &str) {
    for secret in SECRETS {
        assert!(!text.contains(secret), "{text}");
    }
}
