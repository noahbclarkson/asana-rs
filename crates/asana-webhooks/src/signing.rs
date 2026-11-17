//! The webhook's shared secret, delivery signatures, and the answer to a handshake.
//!
//! Asana makes a secret when a webhook is created, hands it to the receiver once in the
//! `X-Hook-Secret` header of the handshake, and from then on signs every delivery with it:
//! `X-Hook-Signature` is the lowercase hex HMAC-SHA256 of the request body's exact bytes, keyed by
//! the secret's bytes exactly as they appeared in the header. [`verify`] checks that in constant
//! time and is the only way to obtain a [`VerifiedBody`], which is the only thing that decodes into
//! events, so nothing reaches the JSON parser before its signature has been checked.

use std::fmt;

use hmac::{Hmac, KeyInit, Mac};
use secrecy::{ExposeSecret, SecretSlice};
use sha2::Sha256;
use subtle::ConstantTimeEq;

use crate::events::{DecodeError, EventBatch, Limits};
use crate::headers::HOOK_SECRET;
use crate::inbound::InboundError;

/// A webhook's shared secret: the bytes of the `X-Hook-Secret` header, exactly as Asana sent them.
///
/// Held in a [`secrecy`] box, so it is zeroed when dropped, and its `Debug` prints nothing of it.
/// Asana documents no format for it (today it is 64 hex characters), so nothing here checks one:
/// the bytes are the key, whatever they are. Store it with [`expose`](Self::expose) and bring it
/// back with [`from_bytes`](Self::from_bytes), encrypted at rest.
pub struct WebhookSecret(SecretSlice<u8>);

impl WebhookSecret {
    /// The secret from a handshake's `X-Hook-Secret` header value.
    ///
    /// # Errors
    ///
    /// [`InboundError::EmptySecret`] when the value is empty: there is nothing to sign with.
    pub fn from_header(value: &str) -> Result<Self, InboundError> {
        if value.is_empty() {
            return Err(InboundError::EmptySecret);
        }
        Ok(Self::from_bytes(value.as_bytes().to_vec()))
    }

    /// The secret from bytes stored earlier with [`expose`](Self::expose), or from the
    /// `X-Hook-Secret` field of the response to the request that created the webhook.
    #[must_use]
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        Self(SecretSlice::from(bytes))
    }

    /// The secret's bytes, for storing it. Keep them out of logs.
    #[must_use]
    pub fn expose(&self) -> &[u8] {
        self.0.expose_secret()
    }

    /// Whether `other` is the same secret, compared in constant time.
    ///
    /// The creator of a webhook compares the secret its receiver captured during the handshake with
    /// the one in the creating response; see [`settle`](crate::settle).
    #[must_use]
    pub fn ct_eq(&self, other: &Self) -> bool {
        self.expose().ct_eq(other.expose()).into()
    }

    fn mac(&self) -> Hmac<Sha256> {
        #[expect(
            clippy::expect_used,
            reason = "HMAC takes a key of any length: a long one is hashed and a short one padded"
        )]
        <Hmac<Sha256> as KeyInit>::new_from_slice(self.expose())
            .expect("HMAC-SHA256 accepts a key of any length")
    }
}

impl Clone for WebhookSecret {
    fn clone(&self) -> Self {
        Self::from_bytes(self.expose().to_vec())
    }
}

impl fmt::Debug for WebhookSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("WebhookSecret([REDACTED])")
    }
}

/// A delivery's signature: the 32 bytes the `X-Hook-Signature` header spells in hex.
///
/// Not secret (anyone on the wire sees it), but its `Debug` still prints only its length, so a
/// logged request never pairs a body with a valid signature for it.
#[derive(Clone, PartialEq, Eq)]
pub struct Signature([u8; 32]);

impl Signature {
    /// Parses the header's value: exactly 64 hexadecimal digits, in either case, with surrounding
    /// whitespace ignored.
    ///
    /// # Errors
    ///
    /// [`InboundError::MalformedSignature`] for anything else.
    pub fn from_hex(text: &str) -> Result<Self, InboundError> {
        let text = text.trim();
        let mut bytes = [0_u8; 32];
        if text.len() != 64 || hex::decode_to_slice(text, &mut bytes).is_err() {
            return Err(InboundError::MalformedSignature);
        }
        Ok(Self(bytes))
    }

    /// The signature as Asana writes it: 64 lowercase hexadecimal digits.
    #[must_use]
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    /// The signature's 32 bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for Signature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Signature(32 bytes)")
    }
}

/// Why a delivery's signature was refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum VerifyError {
    /// The signature is not the HMAC of this body under this secret: the call is not from Asana,
    /// or the body was altered, or the secret is the wrong one for this route.
    #[error("the delivery's signature does not match its body")]
    Mismatch,
}

impl VerifyError {
    /// The status a receiver should answer: `401`. Asana never sends a bad signature itself, so a
    /// refusal must not look like an acceptance to whoever did.
    #[must_use]
    pub fn status_hint(&self) -> u16 {
        401
    }
}

/// Signs `body` as Asana would: HMAC-SHA256 under the secret.
///
/// For producing deliveries in tests and fakes; a receiver uses [`verify`].
#[must_use]
pub fn sign(secret: &WebhookSecret, body: &[u8]) -> Signature {
    let mut mac = secret.mac();
    mac.update(body);
    Signature(mac.finalize().into_bytes().into())
}

/// Checks a delivery's signature over its exact raw bytes, in constant time.
///
/// Call it on the body as received, before parsing or re-encoding anything: the signature covers
/// bytes, not JSON, so any transformation (a framework's JSON extractor, a re-serialisation, a
/// changed line ending) breaks it.
///
/// # Errors
///
/// [`VerifyError::Mismatch`] when the signature is not this body's under this secret.
pub fn verify<'a>(
    secret: &WebhookSecret,
    signature: &Signature,
    body: &'a [u8],
) -> Result<VerifiedBody<'a>, VerifyError> {
    let mut mac = secret.mac();
    mac.update(body);
    mac.verify_slice(signature.as_bytes())
        .map_err(|_| VerifyError::Mismatch)?;
    Ok(VerifiedBody(body))
}

/// A delivery body whose signature has been checked. Only [`verify`] makes one.
#[derive(Clone, Copy)]
pub struct VerifiedBody<'a>(&'a [u8]);

impl<'a> VerifiedBody<'a> {
    /// The verified bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &'a [u8] {
        self.0
    }

    /// Decodes the events with the default [`Limits`].
    ///
    /// # Errors
    ///
    /// See [`decode_with`](Self::decode_with).
    pub fn decode(&self) -> Result<EventBatch, DecodeError> {
        self.decode_with(&Limits::default())
    }

    /// Decodes the events, refusing a batch longer than `limits.max_events`.
    ///
    /// One event that cannot be read does not fail the batch: it is kept in
    /// [`EventBatch::unreadable`] with its raw JSON. An empty body is a heartbeat.
    ///
    /// # Errors
    ///
    /// [`DecodeError`] when the body is not JSON, has no `events` array, or holds too many events.
    pub fn decode_with(&self, limits: &Limits) -> Result<EventBatch, DecodeError> {
        EventBatch::decode(self.0, limits)
    }
}

impl fmt::Debug for VerifiedBody<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "VerifiedBody({} bytes)", self.0.len())
    }
}

/// What a receiver must answer to a handshake, as plain data: status `200` with the secret echoed
/// back in the `X-Hook-Secret` header. Asana also accepts `204`.
///
/// Asana's request to create the webhook stays open until this answer arrives, so answer it at
/// once; and only after the secret has been saved where the creator will look for it
/// ([`HandshakeStore::capture`](crate::HandshakeStore::capture)).
#[derive(Clone)]
pub struct HandshakeReply {
    secret: WebhookSecret,
}

impl HandshakeReply {
    /// The status to answer: `200`.
    #[must_use]
    pub fn status(&self) -> u16 {
        200
    }

    /// The header to set: `X-Hook-Secret`.
    #[must_use]
    pub fn header_name(&self) -> &'static str {
        HOOK_SECRET
    }

    /// The header's value: the secret, byte for byte.
    #[must_use]
    pub fn header_value(&self) -> &[u8] {
        self.secret.expose()
    }
}

impl fmt::Debug for HandshakeReply {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HandshakeReply")
            .field("status", &self.status())
            .field("header", &self.header_name())
            .finish_non_exhaustive()
    }
}

/// The answer to a handshake that carried `secret`.
#[must_use]
pub fn handshake_reply(secret: &WebhookSecret) -> HandshakeReply {
    HandshakeReply {
        secret: secret.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_signature_parser_takes_64_hex_digits_and_nothing_else() {
        let hex = "ab".repeat(32);
        assert_eq!(Signature::from_hex(&hex).unwrap().to_hex(), hex);
        assert_eq!(
            Signature::from_hex(&format!(" {} ", hex.to_uppercase()))
                .unwrap()
                .to_hex(),
            hex
        );
        for bad in ["", &hex[..63], &format!("{hex}0"), &"zz".repeat(32)] {
            assert_eq!(
                Signature::from_hex(bad),
                Err(InboundError::MalformedSignature)
            );
        }
    }

    #[test]
    fn debug_output_never_shows_the_secret_or_the_signature() {
        let secret = WebhookSecret::from_header("s3cr3t-value").unwrap();
        let signature = sign(&secret, b"{}");
        let reply = handshake_reply(&secret);
        for text in [
            format!("{secret:?}"),
            format!("{reply:?}"),
            format!("{signature:?}"),
        ] {
            assert!(!text.contains("s3cr3t"), "{text}");
            assert!(!text.contains(&signature.to_hex()[..8]), "{text}");
        }
        assert_eq!(reply.header_value(), b"s3cr3t-value");
        assert_eq!(reply.status(), 200);
        assert_eq!(reply.header_name(), "X-Hook-Secret");
    }

    #[test]
    fn an_empty_secret_is_refused_and_equal_secrets_compare_equal() {
        assert_eq!(
            WebhookSecret::from_header("").unwrap_err(),
            InboundError::EmptySecret
        );
        let a = WebhookSecret::from_header("abc").unwrap();
        assert!(a.ct_eq(&WebhookSecret::from_bytes(b"abc".to_vec())));
        assert!(!a.ct_eq(&WebhookSecret::from_bytes(b"abd".to_vec())));
        assert!(!a.ct_eq(&WebhookSecret::from_bytes(b"ab".to_vec())));
    }
}
