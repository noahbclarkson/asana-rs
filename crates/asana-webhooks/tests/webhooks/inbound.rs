//! Classifying requests and checking signatures.

use asana_webhooks::{
    Inbound, InboundError, Limits, Signature, VerifyError, classify, classify_with, sign, verify,
};

use crate::documented_secret;

/// Computed independently: `hmac.new(secret, b'{"events":[]}', hashlib.sha256).hexdigest()` in
/// Python, and `openssl dgst -sha256 -hmac`, with the secret from Asana's documentation.
const HEARTBEAT_SIGNATURE: &str =
    "10468027b67df18c739cc4794c6fc2d2806dcaaf2259fc3e1ed9d26433659853";

/// The same, over `{"events":[{"action":"changed"}]}`.
const ONE_EVENT_SIGNATURE: &str =
    "5f67204555e6ed6fa32c912f3d6ae7aad9e9686b11fc7be044f7cafe67b63f53";

#[test]
fn signing_matches_an_independently_computed_hmac() {
    let secret = documented_secret();
    assert_eq!(
        sign(&secret, br#"{"events":[]}"#).to_hex(),
        HEARTBEAT_SIGNATURE
    );
    assert_eq!(
        sign(&secret, br#"{"events":[{"action":"changed"}]}"#).to_hex(),
        ONE_EVENT_SIGNATURE
    );
}

#[test]
fn a_handshake_carries_the_secret_and_no_signature() {
    let headers = [("x-hook-secret", "b537207f20cbfa02357cf448134da559")];
    let Ok(Inbound::Handshake { secret }) = classify(&headers, b"") else {
        panic!("not a handshake");
    };
    assert_eq!(secret.expose(), b"b537207f20cbfa02357cf448134da559");
}

#[test]
fn a_delivery_verifies_over_its_exact_bytes() {
    let body = br#"{"events":[]}"#;
    let headers = [("X-Hook-Signature", HEARTBEAT_SIGNATURE)];
    let Ok(Inbound::Delivery { signature, body }) = classify(&headers, body) else {
        panic!("not a delivery");
    };
    let verified = verify(&documented_secret(), &signature, body).unwrap();
    assert!(verified.decode().unwrap().is_heartbeat());

    // The same JSON, spaced differently, is different bytes and fails.
    assert_eq!(
        verify(&documented_secret(), &signature, br#"{"events": []}"#).unwrap_err(),
        VerifyError::Mismatch
    );
}

#[test]
fn a_signature_from_another_secret_or_body_is_refused_with_401() {
    let body = br#"{"events":[{"action":"changed"}]}"#;
    let heartbeat = Signature::from_hex(HEARTBEAT_SIGNATURE).unwrap();
    let error = verify(&documented_secret(), &heartbeat, body).unwrap_err();
    assert_eq!(error, VerifyError::Mismatch);
    assert_eq!(error.status_hint(), 401);

    let other = asana_webhooks::WebhookSecret::from_bytes(b"another".to_vec());
    let right = Signature::from_hex(ONE_EVENT_SIGNATURE).unwrap();
    assert!(verify(&other, &right, body).is_err());
    assert!(verify(&documented_secret(), &right, body).is_ok());
}

#[test]
fn malformed_requests_are_refused_before_parsing() {
    let cases: [(&[(&str, &str)], InboundError); 5] = [
        (
            &[
                ("X-Hook-Secret", "abc"),
                ("X-Hook-Signature", HEARTBEAT_SIGNATURE),
            ],
            InboundError::BothHeaders,
        ),
        (
            &[("Content-Type", "application/json")],
            InboundError::NeitherHeader,
        ),
        (
            &[("X-Hook-Signature", &HEARTBEAT_SIGNATURE[..63])],
            InboundError::MalformedSignature,
        ),
        (
            &[("X-Hook-Signature", "not hex")],
            InboundError::MalformedSignature,
        ),
        (&[("X-Hook-Secret", "")], InboundError::EmptySecret),
    ];
    for (headers, expected) in cases {
        let error = classify(headers, b"{}").unwrap_err();
        assert_eq!(error, expected);
        assert_eq!(error.status_hint(), 400);
    }
}

#[test]
fn an_oversized_body_is_refused_with_413() {
    let headers = [("X-Hook-Signature", HEARTBEAT_SIGNATURE)];
    let mut limits = Limits::default();
    assert_eq!(limits.max_body_bytes, 256 * 1024);
    assert!(classify(&headers, &vec![b' '; 256 * 1024]).is_ok());
    let error = classify(&headers, &vec![b' '; 256 * 1024 + 1]).unwrap_err();
    assert_eq!(
        error,
        InboundError::BodyTooLarge {
            len: 256 * 1024 + 1,
            limit: 256 * 1024
        }
    );
    assert_eq!(error.status_hint(), 413);

    limits.max_body_bytes = 8;
    assert!(classify_with(&headers, b"{\"events\":[]}", &limits).is_err());
}

#[test]
fn errors_never_repeat_a_header_or_the_body() {
    let headers = [("X-Hook-Signature", "zz-secretish-zz")];
    let error = classify(&headers, b"{\"password\":1}").unwrap_err();
    let text = format!("{error} {error:?}");
    assert!(
        !text.contains("secretish") && !text.contains("password"),
        "{text}"
    );
}
