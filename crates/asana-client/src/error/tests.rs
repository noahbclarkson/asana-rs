//! Neither `Display` nor `Debug` of any error prints what Asana or a request carried.

use std::time::Duration;

use http::StatusCode;

use super::{DecodeError, Error};
use crate::auth::AuthError;
use crate::oauth::{OAuthError, OAuthErrorCode, State, read_redirect};
use crate::transport::TransportError;
use crate::webhooks::WebhookError;

/// Planted in every value that came from Asana or from a request.
const MARKER: &str = "acme-customer-data";

/// An error body whose every message, help text and sync token holds the marker.
fn body() -> Vec<u8> {
    serde_json::json!({
        "errors": [
            {
                "message": format!("Not a valid GID: {MARKER}"),
                "help": format!("https://example.com/help?q={MARKER}"),
                "phrase": "6 sad squid snuggle softly"
            },
            {"message": format!("{MARKER} is not a member of the workspace")}
        ],
        "sync": format!("{MARKER}-sync-token")
    })
    .to_string()
    .into_bytes()
}

/// Every variant of the client's `Error`, those made from a response made from the marked body.
fn every_error() -> Vec<Error> {
    let body = body();
    let mut errors: Vec<Error> = [400, 401, 402, 403, 404, 412, 418, 451, 500, 503]
        .into_iter()
        .map(|status| Error::from_status(StatusCode::from_u16(status).unwrap(), &body, 1, false))
        .collect();
    let decode = serde_json::from_slice::<u32>(&body).unwrap_err();
    let encode =
        serde_json::to_vec(&std::collections::HashMap::from([((1, 2), MARKER)])).unwrap_err();
    errors.extend([
        Error::RateLimited {
            retry_after: Duration::from_secs(30),
            attempts: 2,
        },
        Error::MissingField("name"),
        Error::Decode {
            operation: "getTask",
            path: "data.name".to_owned(),
            source: DecodeError::from_json(&decode),
        },
        Error::Encode {
            operation: "createTask",
            source: encode,
        },
        Error::InvalidRequest {
            operation: "getTask",
            reason: "a path parameter is empty",
        },
        Error::Transport {
            source: TransportError::timeout("operation timed out"),
            attempts: 1,
            safe_to_repeat: false,
        },
        Error::Auth(AuthError::Revoked),
        Error::Auth(AuthError::Malformed),
        Error::Auth(AuthError::unavailable("the OAuth token store failed", None)),
        Error::JobFailed {
            job: "1200000000000001".parse().unwrap(),
        },
        Error::JobTimedOut {
            job: "1200000000000001".parse().unwrap(),
            waited: Duration::from_secs(60),
        },
        Error::BatchFull { limit: 10 },
    ]);
    errors
}

fn every_oauth_error() -> Vec<OAuthError> {
    let state = State::generate();
    let redirect = format!(
        "state={}&error={MARKER}%20text&error_description=declined%20by%20{MARKER}",
        state.as_str()
    );
    vec![
        OAuthError::InvalidGrant {
            description: Some(format!("refresh token {MARKER} is revoked")),
        },
        OAuthError::Refused {
            status: StatusCode::BAD_REQUEST,
            code: OAuthErrorCode::parse(MARKER),
            description: Some(MARKER.to_owned()),
        },
        OAuthError::Refused {
            status: StatusCode::BAD_REQUEST,
            code: OAuthErrorCode::parse(&format!("{MARKER} code")),
            description: None,
        },
        OAuthError::Status {
            status: StatusCode::BAD_GATEWAY,
        },
        OAuthError::Transport {
            source: TransportError::timeout("operation timed out"),
        },
        read_redirect(&redirect, &state).unwrap_err(),
        OAuthError::StateMismatch,
        OAuthError::MissingCode,
        OAuthError::MissingRefreshToken,
    ]
}

/// Formats `error` both ways and fails, naming the variant, if the marker shows.
fn assert_unmarked(error: &(impl std::fmt::Debug + std::fmt::Display)) {
    let debug = format!("{error:?}");
    let alternate = format!("{error:#?}");
    let display = error.to_string();
    for text in [&debug, &alternate, &display] {
        assert!(!text.contains(MARKER), "the marker leaked: {text}");
    }
}

#[test]
fn no_error_prints_a_body_a_message_or_a_query_value() {
    for error in every_error() {
        assert_unmarked(&error);
        // The same errors as the webhook calls return them.
        assert_unmarked(&WebhookError::Create {
            source: Box::new(error),
            handshake_seen: true,
        });
    }
    for error in every_oauth_error() {
        assert_unmarked(&error);
    }
}

#[test]
fn asanas_messages_stay_readable_on_purpose() {
    let error = Error::from_status(StatusCode::NOT_FOUND, &body(), 1, true);
    let messages: Vec<&str> = error.api_errors().unwrap().messages().collect();
    assert_eq!(messages.len(), 2);
    assert!(messages[0].contains(MARKER));
    // Debug still says how many there were, and the phrase Asana's support asks for.
    let debug = format!("{error:?}");
    assert!(debug.contains("<redacted>"), "{debug}");
    assert!(debug.contains("6 sad squid snuggle softly"), "{debug}");

    let Error::SyncTokenExpired { sync } =
        Error::from_status(StatusCode::PRECONDITION_FAILED, &body(), 1, true)
    else {
        panic!("expected SyncTokenExpired");
    };
    assert!(sync.as_str().contains(MARKER));

    let oauth = OAuthError::InvalidGrant {
        description: Some(MARKER.to_owned()),
    };
    assert_eq!(oauth.description(), Some(MARKER));
    // A code that looks like one is still shown.
    let refused = OAuthError::Refused {
        status: StatusCode::BAD_REQUEST,
        code: OAuthErrorCode::parse("invalid_scope_name"),
        description: None,
    };
    assert!(format!("{refused:?}").contains("invalid_scope_name"));
}
