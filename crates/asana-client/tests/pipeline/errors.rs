//! Each status Asana answers with becomes the matching error, without a body in its `Display`.

use asana_client::{Error, SyncToken, decode_response};
use http::StatusCode;

use crate::support::{AddTag, GetEvents, Scripted, client, get_task, reply};

const ERRORS: &str = r#"{"errors":[{"message":"task: Not a recognized ID: Acme secret","help":"For more information on API status codes and how to handle them, read the docs on errors: https://developers.asana.com/docs/errors"}]}"#;

fn status(code: u16) -> Error {
    decode_response::<AddTag>(StatusCode::from_u16(code).unwrap(), ERRORS.as_bytes()).unwrap_err()
}

#[test]
fn each_status_maps_to_its_error() {
    assert!(matches!(status(400), Error::BadRequest(_)));
    assert!(matches!(status(401), Error::Unauthorized(_)));
    assert!(matches!(status(402), Error::PremiumRequired(_)));
    assert!(matches!(status(403), Error::Forbidden(_)));
    assert!(matches!(status(404), Error::NotFound(_)));
    assert!(matches!(status(451), Error::LegalBlock(_)));
    assert!(matches!(status(418), Error::UnexpectedStatus { status, .. } if status == 418));
    assert!(matches!(status(412), Error::UnexpectedStatus { status, .. } if status == 412));
    assert!(
        matches!(status(503), Error::Server { status, phrase: None, attempts: 1, .. } if status == 503)
    );
    for code in [400, 401, 402, 403, 404, 418, 451, 500] {
        let error = status(code);
        assert_eq!(error.status().map(|status| status.as_u16()), Some(code));
        assert!(!error.to_string().contains("Acme secret"), "{error}");
    }
    assert!(status(404).is_not_found());
    assert!(!status(400).is_retryable());
    assert_eq!(
        status(400)
            .api_errors()
            .unwrap()
            .messages()
            .collect::<Vec<_>>(),
        ["task: Not a recognized ID: Acme secret"]
    );
}

#[test]
fn a_500_keeps_asanas_support_phrase() {
    let body = r#"{"errors":[{"message":"Server Error","phrase":"6 sad squid snuggle softly"}]}"#;
    let error =
        decode_response::<AddTag>(StatusCode::INTERNAL_SERVER_ERROR, body.as_bytes()).unwrap_err();
    assert!(
        matches!(error, Error::Server { phrase: Some(ref phrase), .. } if phrase == "6 sad squid snuggle softly")
    );
    assert!(error.is_retryable());
}

#[test]
fn a_body_that_is_not_asanas_error_shape_still_maps_by_status() {
    let error = decode_response::<AddTag>(StatusCode::FORBIDDEN, b"<html>nope</html>").unwrap_err();
    assert!(matches!(error, Error::Forbidden(ref errors) if errors.errors.is_empty()));
}

#[tokio::test(start_paused = true)]
async fn the_events_412_carries_the_new_sync_token() {
    let transport = Scripted::new(|_, _| {
        Ok(reply(
            412,
            r#"{"errors":[{"message":"Sync token invalid or too old. If you are attempting to keep resources in sync, you must fetch the full dataset for this query now and use the new sync token for the next sync."}],"sync":"de4774f6915eae04714ca93bb2f5ee81"}"#,
        ))
    });
    let error = client(transport)
        .send(GetEvents { query: Vec::new() })
        .await
        .unwrap_err();
    match error {
        Error::SyncTokenExpired { sync } => {
            assert_eq!(sync, SyncToken::new("de4774f6915eae04714ca93bb2f5ee81"));
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test(start_paused = true)]
async fn a_404_through_the_client_is_not_found_and_not_retried() {
    let transport = Scripted::new(|_, _| Ok(reply(404, ERRORS)));
    let error = client(transport.clone())
        .send(get_task())
        .await
        .unwrap_err();
    assert!(error.is_not_found());
    assert_eq!(transport.count(), 1);
}
