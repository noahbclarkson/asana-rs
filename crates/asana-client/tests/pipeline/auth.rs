//! The token on each request, the one refresh after a `401`, and secrets kept out of `Debug`.

use std::sync::{Arc, Mutex};

use asana_client::{AccessToken, Auth, AuthError, Client, Error, TokenSource};
use futures_util::future::BoxFuture;
use secrecy::ExposeSecret;

use crate::support::{BASE, Scripted, TOKEN, client, get_task, reply, task_body};

/// Hands out `token-1`, then `token-2` once `token-1` is rejected.
#[derive(Default)]
struct Rotating {
    current: Mutex<u32>,
    rejected: Mutex<Vec<String>>,
}

impl TokenSource for Rotating {
    fn access_token(&self) -> BoxFuture<'_, Result<AccessToken, AuthError>> {
        let token = format!("token-{}", *self.current.lock().unwrap() + 1);
        Box::pin(async move { Ok(AccessToken::new(token)) })
    }

    fn rejected<'a>(&'a self, token: &'a AccessToken) -> BoxFuture<'a, ()> {
        self.rejected
            .lock()
            .unwrap()
            .push(token.secret().expose_secret().to_owned());
        *self.current.lock().unwrap() += 1;
        Box::pin(async {})
    }
}

fn bearer(request: &http::Request<bytes::Bytes>) -> String {
    request.headers()["authorization"]
        .to_str()
        .unwrap()
        .to_owned()
}

fn oauth_client(transport: Arc<Scripted>, source: Arc<Rotating>) -> Client {
    Client::builder()
        .auth(Auth::oauth(source))
        .transport(transport)
        .base_url(BASE)
        .build()
        .unwrap()
}

#[tokio::test(start_paused = true)]
async fn a_rejected_oauth_token_is_replaced_once_and_the_request_sent_again() {
    let transport = Scripted::new(|request, _| {
        Ok(if bearer(request) == "Bearer token-1" {
            reply(401, r#"{"errors":[{"message":"Not Authorized"}]}"#)
        } else {
            reply(200, task_body())
        })
    });
    let source = Arc::new(Rotating::default());
    oauth_client(transport.clone(), source.clone())
        .send(get_task())
        .await
        .unwrap();
    assert_eq!(*source.rejected.lock().unwrap(), ["token-1"]);
    assert_eq!(bearer(&transport.request(1)), "Bearer token-2");
    assert!(transport.request(0).headers()["authorization"].is_sensitive());
}

#[tokio::test(start_paused = true)]
async fn a_second_401_is_returned_without_another_refresh() {
    let transport =
        Scripted::new(|_, _| Ok(reply(401, r#"{"errors":[{"message":"Not Authorized"}]}"#)));
    let source = Arc::new(Rotating::default());
    let error = oauth_client(transport.clone(), source.clone())
        .send(get_task())
        .await
        .unwrap_err();
    assert!(matches!(error, Error::Unauthorized(_)), "{error:?}");
    assert_eq!(source.rejected.lock().unwrap().len(), 1);
    assert_eq!(transport.count(), 2);
}

#[tokio::test(start_paused = true)]
async fn a_personal_access_token_is_sent_as_a_bearer_and_never_retried_on_401() {
    let transport = Scripted::new(|_, _| Ok(reply(401, "{}")));
    let error = client(transport.clone())
        .send(get_task())
        .await
        .unwrap_err();
    assert!(matches!(error, Error::Unauthorized(_)));
    assert_eq!(transport.count(), 1);
    assert_eq!(bearer(&transport.request(0)), format!("Bearer {TOKEN}"));
}

#[test]
fn no_debug_or_display_shows_a_token() {
    let auth = Auth::personal_access_token(TOKEN);
    let client = Client::builder()
        .auth(auth.clone())
        .transport(Scripted::new(|_, _| Ok(reply(200, "{}"))))
        .base_url(BASE)
        .build()
        .unwrap();
    let shown = [
        format!("{auth:?}"),
        format!("{:?}", Auth::service_account(TOKEN)),
        format!("{:?}", Auth::oauth(Rotating::default())),
        format!("{client:?}"),
        format!("{:?}", AccessToken::new(TOKEN)),
        format!("{:?}", Error::Auth(AuthError::Malformed)),
    ];
    for text in &shown {
        assert!(!text.contains("acme-test-token"), "{text}");
    }
    assert!(shown[3].contains(BASE));
}
