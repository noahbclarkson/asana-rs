//! The token, revocation and introspection calls: what is sent, how answers and errors are read.

use std::time::Duration;

use asana_client::oauth::{
    AuthorizationCode, OAuthError, OAuthErrorCode, PkceVerifier, RefreshToken, read_redirect,
};
use asana_client::{ExposeSecret, SecretString, TransportError};

use crate::fixtures::{
    ACCESS, CLIENT_SECRET, CODE, FRESH_ACCESS, REFRESH, START, app, assert_no_secret,
    exchange_answer, form, refresh_answer, shown,
};
use crate::support::{Scripted, reply};

const VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";

#[tokio::test]
async fn exchanging_a_code_posts_the_documented_form_and_reads_the_tokens_and_user() {
    let transport = Scripted::replies(vec![Ok(reply(200, &exchange_answer()))]);
    let state = asana_client::oauth::State::new("acme-state");
    let code = read_redirect(&format!("code={CODE}&state=acme-state"), &state).unwrap();
    let tokens = app(transport.clone())
        .exchange(&code, &PkceVerifier::new(VERIFIER).unwrap())
        .await
        .unwrap();

    let request = transport.request(0);
    insta::assert_snapshot!("exchange_request", shown(&request));
    let fields = form(request.body());
    assert!(fields.contains(&("client_secret".into(), CLIENT_SECRET.into())));
    assert!(fields.contains(&("code".into(), CODE.into())));
    assert!(fields.contains(&("code_verifier".into(), VERIFIER.into())));
    assert_eq!(request.headers()["accept"], "application/json");
    assert!(
        request.headers()["user-agent"]
            .to_str()
            .unwrap()
            .starts_with("asana-rs/")
    );
    // Nothing secret travels in the URL.
    assert_eq!(request.uri().query(), None);

    assert_eq!(tokens.access_token.secret().expose_secret(), ACCESS);
    assert_eq!(
        tokens
            .refresh_token
            .as_ref()
            .unwrap()
            .secret()
            .expose_secret(),
        REFRESH
    );
    assert_eq!(tokens.expires_in, Some(Duration::from_secs(3600)));
    assert_eq!(tokens.token_type, "bearer");
    let user = tokens.user.clone().unwrap();
    assert_eq!(user.gid.unwrap().to_string(), "1201234567890999");
    assert_eq!(user.email.as_deref(), Some("user@example.com"));
    insta::assert_snapshot!("exchange_response_debug", format!("{tokens:#?}"));

    let stored = tokens.into_stored(START).unwrap();
    assert_eq!(stored.expires_at, START + time::Duration::hours(1));
}

#[tokio::test]
async fn refreshing_posts_the_refresh_grant_and_reads_an_answer_without_a_refresh_token() {
    let transport = Scripted::replies(vec![Ok(reply(200, &refresh_answer()))]);
    let tokens = app(transport.clone())
        .refresh(&RefreshToken::new(REFRESH))
        .await
        .unwrap();
    let request = transport.request(0);
    insta::assert_snapshot!("refresh_request", shown(&request));
    assert!(form(request.body()).contains(&("refresh_token".into(), REFRESH.into())));
    assert_eq!(tokens.access_token.secret().expose_secret(), FRESH_ACCESS);
    assert!(tokens.refresh_token.is_none());
    assert!(matches!(
        tokens.into_stored(START),
        Err(OAuthError::MissingRefreshToken)
    ));
}

#[tokio::test]
async fn invalid_grant_is_its_own_error_and_says_the_person_must_reconnect() {
    let transport = Scripted::replies(vec![Ok(reply(
        400,
        r#"{"error":"invalid_grant","error_description":"The `refresh_token` is invalid or has been revoked."}"#,
    ))]);
    let error = app(transport)
        .refresh(&RefreshToken::new(REFRESH))
        .await
        .unwrap_err();
    assert!(error.needs_reconnect());
    let OAuthError::InvalidGrant { description } = &error else {
        panic!("{error:?}");
    };
    assert!(description.as_deref().unwrap().contains("revoked"));
    assert!(!error.to_string().contains("refresh_token"), "{error}");
    assert!(!error.is_retryable());
}

#[tokio::test]
async fn other_oauth_errors_keep_their_code_and_status() {
    let transport = Scripted::replies(vec![Ok(reply(
        401,
        r#"{"error":"invalid_client","error_description":"Bad client secret"}"#,
    ))]);
    let error = app(transport)
        .exchange(
            &AuthorizationCode::new(CODE),
            &PkceVerifier::new(VERIFIER).unwrap(),
        )
        .await
        .unwrap_err();
    let OAuthError::Refused {
        status,
        code,
        description,
    } = &error
    else {
        panic!("{error:?}");
    };
    assert_eq!(status.as_u16(), 401);
    assert_eq!(*code, OAuthErrorCode::InvalidClient);
    assert_eq!(description.as_deref(), Some("Bad client secret"));
    assert!(!error.to_string().contains("Bad client secret"));
    assert!(!error.needs_reconnect());

    let unknown = OAuthErrorCode::parse("Bearer acme-access-token-not-real");
    assert_no_secret(&unknown.to_string());
}

#[tokio::test]
async fn a_server_error_without_an_oauth_body_is_a_retryable_status() {
    let transport = Scripted::replies(vec![Ok(reply(503, "<html>busy</html>"))]);
    let error = app(transport)
        .refresh(&RefreshToken::new(REFRESH))
        .await
        .unwrap_err();
    assert!(matches!(error, OAuthError::Status { status } if status.as_u16() == 503));
    assert!(error.is_retryable());

    let unreachable = Scripted::replies(vec![Err(TransportError::connect("refused"))]);
    let error = app(unreachable)
        .refresh(&RefreshToken::new(REFRESH))
        .await
        .unwrap_err();
    assert!(matches!(error, OAuthError::Transport { .. }));
    assert!(error.is_retryable());

    let garbled = Scripted::replies(vec![Ok(reply(200, r#"{"token_type":"bearer"}"#))]);
    let error = app(garbled)
        .refresh(&RefreshToken::new(REFRESH))
        .await
        .unwrap_err();
    assert!(matches!(error, OAuthError::Decode { .. }), "{error:?}");
}

#[tokio::test]
async fn revoking_posts_the_refresh_token_and_treats_not_found_as_done() {
    let transport = Scripted::replies(vec![
        Ok(reply(200, "")),
        Ok(reply(400, r#"{"error":"invalid_request"}"#)),
    ]);
    let app = app(transport.clone());
    app.revoke(&RefreshToken::new(REFRESH)).await.unwrap();
    insta::assert_snapshot!("revoke_request", shown(&transport.request(0)));
    let error = app.revoke(&RefreshToken::new(ACCESS)).await.unwrap_err();
    assert!(matches!(
        error,
        OAuthError::Refused {
            code: OAuthErrorCode::InvalidRequest,
            ..
        }
    ));
}

#[tokio::test]
async fn introspection_sends_only_the_token_and_reads_its_scopes_and_expiry() {
    let transport = Scripted::replies(vec![Ok(reply(
        200,
        r#"{"token_type":"bearer","expires_in":3599,"exp":1772442000,
            "scope":"tasks:read tasks:write projects:read openid brand_new:read","active":true,
            "client_id":1201234567890123}"#,
    ))]);
    let info = app(transport.clone())
        .introspect(&SecretString::from(ACCESS))
        .await
        .unwrap();
    let request = transport.request(0);
    assert_eq!(request.uri().path(), "/-/token_info");
    assert_eq!(form(request.body()), [("token".into(), ACCESS.into())]);
    assert!(info.active);
    assert_eq!(info.expires_in, Some(Duration::from_secs(3599)));
    assert_eq!(info.expires_at, Some(START));
    assert_eq!(info.client_id.as_deref(), Some("1201234567890123"));
    assert_eq!(
        info.scope.to_string(),
        "openid projects:read tasks:read tasks:write brand_new:read"
    );
}
