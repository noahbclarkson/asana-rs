//! The refreshing token source: answering from memory, refreshing before expiry and after a `401`,
//! one refresh for many callers, losing the store's compare-and-swap, and a dead refresh token.

use std::sync::Arc;
use std::time::Duration;

use asana_client::oauth::{
    InMemoryTokenStore, RefreshToken, RefreshingTokenSource, Replaced, StoredTokens, TokenStore,
};
use asana_client::{
    AccessToken, Auth, AuthError, BoxError, Client, Error, ExposeSecret, TokenSource, Transport,
    TransportError,
};
use bytes::Bytes;
use futures_util::future::BoxFuture;

use crate::fixtures::{
    ACCESS, FRESH_ACCESS, REFRESH, START, TestClock, app, assert_no_secret, refresh_answer, stored,
};
use crate::support::{BASE, Scripted, get_task, reply, task_body};

const WINNER_ACCESS: &str = "acme-winner-access-not-real";
const WINNER_REFRESH: &str = "acme-winner-refresh-not-real";

type Source = RefreshingTokenSource<Arc<InMemoryTokenStore>>;

fn source(transport: Arc<Scripted>, store: Arc<InMemoryTokenStore>, clock: &TestClock) -> Source {
    RefreshingTokenSource::new(app(transport), store).with_clock(clock.clone())
}

fn secret(token: &AccessToken) -> &str {
    token.secret().expose_secret()
}

fn winner() -> StoredTokens {
    StoredTokens::new(
        AccessToken::new(WINNER_ACCESS),
        RefreshToken::new(WINNER_REFRESH),
        START + time::Duration::hours(2),
    )
}

/// Answers the token endpoint after a delay, so concurrent callers overlap a refresh.
struct Slow(Arc<Scripted>);

impl Transport for Slow {
    fn send(
        &self,
        request: http::Request<Bytes>,
    ) -> BoxFuture<'_, Result<http::Response<Bytes>, TransportError>> {
        Box::pin(async move {
            tokio::time::sleep(Duration::from_millis(250)).await;
            self.0.send(request).await
        })
    }
}

#[tokio::test(start_paused = true)]
async fn a_fresh_token_is_answered_from_memory_without_a_request() {
    let transport = Scripted::replies(vec![Ok(reply(200, &refresh_answer()))]);
    let clock = TestClock::at(START);
    let source = source(
        transport.clone(),
        Arc::new(InMemoryTokenStore::new(stored())),
        &clock,
    );
    for _ in 0..3 {
        assert_eq!(secret(&source.access_token().await.unwrap()), ACCESS);
    }
    assert_eq!(transport.count(), 0);
}

#[tokio::test(start_paused = true)]
async fn the_token_is_refreshed_once_less_than_the_margin_is_left() {
    let transport = Scripted::replies(vec![Ok(reply(200, &refresh_answer()))]);
    let clock = TestClock::at(START);
    let store = Arc::new(InMemoryTokenStore::new(stored()));
    let source = source(transport.clone(), store.clone(), &clock);

    clock.advance(time::Duration::minutes(54)); // six minutes left
    assert_eq!(secret(&source.access_token().await.unwrap()), ACCESS);
    assert_eq!(transport.count(), 0);

    clock.advance(time::Duration::minutes(2)); // four minutes left: inside the 5-minute margin
    assert_eq!(secret(&source.access_token().await.unwrap()), FRESH_ACCESS);
    assert_eq!(transport.count(), 1);
    assert_eq!(transport.request(0).uri().path(), "/-/oauth_token");

    let saved = store.get().unwrap();
    assert_eq!(secret(&saved.access_token), FRESH_ACCESS);
    // Asana sent no new refresh token, so the old one is kept.
    assert_eq!(saved.refresh_token, RefreshToken::new(REFRESH));
    assert_eq!(saved.expires_at, START + time::Duration::minutes(56 + 60));

    assert_eq!(secret(&source.access_token().await.unwrap()), FRESH_ACCESS);
    assert_eq!(transport.count(), 1);
}

#[tokio::test(start_paused = true)]
async fn ten_callers_finding_the_token_stale_cause_one_refresh() {
    let scripted = Scripted::replies(vec![Ok(reply(200, &refresh_answer()))]);
    let clock = TestClock::at(START + time::Duration::hours(1));
    let app = asana_client::oauth::OAuthApp::builder()
        .client_id(crate::fixtures::CLIENT_ID)
        .client_secret(crate::fixtures::CLIENT_SECRET)
        .redirect_uri(crate::fixtures::REDIRECT)
        .transport(Slow(scripted.clone()))
        .build()
        .unwrap();
    let source = Arc::new(
        RefreshingTokenSource::new(app, InMemoryTokenStore::new(stored()))
            .with_clock(clock.clone()),
    );
    let callers = (0..10).map(|_| {
        let source = source.clone();
        async move { source.access_token().await }
    });
    let tokens = futures_util::future::join_all(callers).await;
    for token in tokens {
        assert_eq!(secret(&token.unwrap()), FRESH_ACCESS);
    }
    assert_eq!(scripted.count(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_rejected_token_is_refreshed_but_a_rejected_old_one_changes_nothing() {
    let transport = Scripted::replies(vec![Ok(reply(200, &refresh_answer()))]);
    let clock = TestClock::at(START);
    let source = source(
        transport.clone(),
        Arc::new(InMemoryTokenStore::new(stored())),
        &clock,
    );
    let first = source.access_token().await.unwrap();
    source.rejected(&first).await;
    assert_eq!(secret(&source.access_token().await.unwrap()), FRESH_ACCESS);
    assert_eq!(transport.count(), 1);

    source.rejected(&first).await;
    assert_eq!(secret(&source.access_token().await.unwrap()), FRESH_ACCESS);
    assert_eq!(transport.count(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_client_refreshes_after_a_401_and_sends_the_request_again() {
    let transport = Scripted::new(|request, _| {
        Ok(if request.uri().path() == "/-/oauth_token" {
            reply(200, &refresh_answer())
        } else if request.headers()["authorization"] == format!("Bearer {ACCESS}").as_str() {
            reply(
                401,
                r#"{"errors":[{"message":"The bearer token has expired."}]}"#,
            )
        } else {
            reply(200, task_body())
        })
    });
    let clock = TestClock::at(START);
    let client = Client::builder()
        .auth(Auth::oauth(source(
            transport.clone(),
            Arc::new(InMemoryTokenStore::new(stored())),
            &clock,
        )))
        .transport(transport.clone())
        .base_url(BASE)
        .build()
        .unwrap();
    client.send(get_task()).await.unwrap();
    let paths: Vec<_> = (0..transport.count())
        .map(|index| transport.request(index).uri().path().to_owned())
        .collect();
    assert_eq!(
        paths,
        [
            "/api/1.0/tasks/1201234567890123",
            "/-/oauth_token",
            "/api/1.0/tasks/1201234567890123"
        ]
    );
    assert_eq!(
        transport.request(2).headers()["authorization"],
        format!("Bearer {FRESH_ACCESS}").as_str()
    );
}

/// A store in which another process saves its refreshed tokens just before this one tries to.
struct Racing {
    inner: InMemoryTokenStore,
}

impl TokenStore for Racing {
    fn load(&self) -> BoxFuture<'_, Result<Option<StoredTokens>, BoxError>> {
        self.inner.load()
    }

    fn replace<'a>(
        &'a self,
        expected: &'a RefreshToken,
        tokens: &'a StoredTokens,
    ) -> BoxFuture<'a, Result<Replaced, BoxError>> {
        self.inner.set(Some(winner()));
        self.inner.replace(expected, tokens)
    }
}

#[tokio::test(start_paused = true)]
async fn losing_the_compare_and_swap_takes_the_winners_tokens() {
    let transport = Scripted::replies(vec![Ok(reply(200, &refresh_answer()))]);
    let clock = TestClock::at(START + time::Duration::hours(1));
    let racing = Arc::new(Racing {
        inner: InMemoryTokenStore::new(stored()),
    });
    let source = RefreshingTokenSource::new(app(transport.clone()), racing.clone())
        .with_clock(clock.clone());
    assert_eq!(secret(&source.access_token().await.unwrap()), WINNER_ACCESS);
    assert_eq!(secret(&source.access_token().await.unwrap()), WINNER_ACCESS);
    assert_eq!(transport.count(), 1);
    // The winner's rotated refresh token was not overwritten.
    assert_eq!(
        racing.inner.get().unwrap().refresh_token,
        RefreshToken::new(WINNER_REFRESH)
    );
}

#[tokio::test(start_paused = true)]
async fn a_dead_refresh_token_is_revoked_and_the_client_says_so() {
    let transport = Scripted::new(|request, _| {
        Ok(if request.uri().path() == "/-/oauth_token" {
            reply(400, r#"{"error":"invalid_grant"}"#)
        } else {
            reply(200, task_body())
        })
    });
    let clock = TestClock::at(START + time::Duration::hours(2));
    let client = Client::builder()
        .auth(Auth::oauth(source(
            transport.clone(),
            Arc::new(InMemoryTokenStore::new(stored())),
            &clock,
        )))
        .transport(transport.clone())
        .base_url(BASE)
        .build()
        .unwrap();
    let error = client.send(get_task()).await.unwrap_err();
    assert!(
        matches!(error, Error::Auth(AuthError::Revoked)),
        "{error:?}"
    );
    assert_eq!(transport.count(), 1);
}

#[tokio::test(start_paused = true)]
async fn invalid_grant_after_another_process_replaced_the_refresh_token_takes_its_tokens() {
    let store = Arc::new(InMemoryTokenStore::new(stored()));
    let other_process = store.clone();
    let transport = Scripted::new(move |_, _| {
        other_process.set(Some(winner()));
        Ok(reply(400, r#"{"error":"invalid_grant"}"#))
    });
    let clock = TestClock::at(START + time::Duration::hours(1));
    let source = source(transport.clone(), store, &clock);
    assert_eq!(secret(&source.access_token().await.unwrap()), WINNER_ACCESS);
    assert_eq!(transport.count(), 1);
}

#[tokio::test(start_paused = true)]
async fn nothing_stored_means_the_person_must_connect_and_nothing_is_sent() {
    let transport = Scripted::replies(vec![Ok(reply(200, &refresh_answer()))]);
    let clock = TestClock::at(START);
    let source = source(
        transport.clone(),
        Arc::new(InMemoryTokenStore::empty()),
        &clock,
    );
    assert!(matches!(
        source.access_token().await,
        Err(AuthError::Revoked)
    ));
    assert_eq!(transport.count(), 0);
}

#[tokio::test(start_paused = true)]
async fn an_unreachable_token_endpoint_is_unavailable_and_shows_no_secret() {
    let transport = Scripted::replies(vec![Ok(reply(503, "busy"))]);
    let clock = TestClock::at(START + time::Duration::hours(1));
    let source = source(
        transport.clone(),
        Arc::new(InMemoryTokenStore::new(stored())),
        &clock,
    );
    let error = source.access_token().await.unwrap_err();
    assert!(matches!(error, AuthError::Unavailable { .. }), "{error:?}");
    assert_no_secret(&format!("{error} {error:?}"));
    // The failure is not remembered: the next call tries again.
    assert!(source.access_token().await.is_err());
    assert_eq!(transport.count(), 2);
}
