//! The two values that tie Asana's redirect back to the flow that started it: PKCE's verifier and
//! challenge ([RFC 7636](https://www.rfc-editor.org/rfc/rfc7636)), and the CSRF `state`.
//!
//! Asana supports only the `S256` challenge method (its OpenID discovery document lists no other)
//! and requires PKCE for `localhost`, `127.0.0.1` and out-of-band redirects. Its guide writes the
//! challenge as `BASE64URL-ENCODE(SHA256(ASCII(code_verifier)))`, which is RFC 7636's; the hex
//! string in the guide's example is illustrative and is not what Asana checks.

use std::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::Rng as _;
use secrecy::{ExposeSecret, SecretString};
use sha2::{Digest as _, Sha256};
use subtle::ConstantTimeEq as _;

/// The random bytes behind a generated verifier or state: 256 bits, 43 characters once encoded.
const RANDOM_BYTES: usize = 32;

/// A PKCE verifier and its challenge, made fresh for each authorisation.
///
/// Send the [challenge](Self::challenge) in the authorisation URL; keep the
/// [verifier](Self::verifier) on the server, tied to the person's session, and send it when
/// exchanging the code.
#[derive(Clone, Debug)]
pub struct PkcePair {
    verifier: PkceVerifier,
    challenge: PkceChallenge,
}

impl PkcePair {
    /// A new pair from a cryptographically secure generator: a 43-character verifier and its
    /// `S256` challenge.
    pub fn generate() -> Self {
        let verifier = PkceVerifier(SecretString::from(random_token()));
        Self::from_verifier(verifier)
    }

    /// The pair for a verifier kept from earlier.
    pub fn from_verifier(verifier: PkceVerifier) -> Self {
        let challenge = PkceChallenge::for_verifier(&verifier);
        Self {
            verifier,
            challenge,
        }
    }

    /// The secret half, sent only to the token endpoint.
    pub fn verifier(&self) -> &PkceVerifier {
        &self.verifier
    }

    /// The public half, sent in the authorisation URL.
    pub fn challenge(&self) -> &PkceChallenge {
        &self.challenge
    }

    /// Splits the pair.
    pub fn into_parts(self) -> (PkceVerifier, PkceChallenge) {
        (self.verifier, self.challenge)
    }
}

/// PKCE's `code_verifier`: 43 to 128 characters of `A-Z a-z 0-9 - . _ ~`. Its `Debug` never shows
/// it.
#[derive(Clone)]
pub struct PkceVerifier(SecretString);

impl PkceVerifier {
    /// Wraps a verifier kept from earlier, checking it has RFC 7636's length and alphabet.
    ///
    /// # Errors
    ///
    /// [`InvalidVerifier`] when it does not.
    pub fn new(verifier: impl Into<SecretString>) -> Result<Self, InvalidVerifier> {
        let verifier = verifier.into();
        let text = verifier.expose_secret();
        let unreserved =
            |b: u8| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~');
        if (43..=128).contains(&text.len()) && text.bytes().all(unreserved) {
            Ok(Self(verifier))
        } else {
            Err(InvalidVerifier)
        }
    }

    /// The verifier itself, for storing it with the person's session.
    pub fn secret(&self) -> &SecretString {
        &self.0
    }
}

impl fmt::Debug for PkceVerifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PkceVerifier([REDACTED])")
    }
}

/// A PKCE verifier that is not 43 to 128 characters of `A-Z a-z 0-9 - . _ ~`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("a PKCE verifier is 43 to 128 characters of A-Z, a-z, 0-9, '-', '.', '_' and '~'")]
pub struct InvalidVerifier;

/// PKCE's `code_challenge` with the `S256` method: public, sent in the authorisation URL.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PkceChallenge(String);

impl PkceChallenge {
    /// The method this crate uses, `S256`.
    pub const METHOD: &'static str = "S256";

    /// `BASE64URL(SHA256(verifier))`, unpadded.
    pub fn for_verifier(verifier: &PkceVerifier) -> Self {
        let digest = Sha256::digest(verifier.0.expose_secret().as_bytes());
        Self(URL_SAFE_NO_PAD.encode(digest))
    }

    /// The challenge as sent.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The `state` sent to Asana and checked when the person comes back, so a redirect cannot be forged
/// into someone else's session. Keep it with the person's session; its `Debug` never shows it.
#[derive(Clone)]
pub struct State(String);

impl State {
    /// A new unguessable state from a cryptographically secure generator (43 characters).
    pub fn generate() -> Self {
        Self(random_token())
    }

    /// Wraps a state kept from earlier.
    pub fn new(state: impl Into<String>) -> Self {
        Self(state.into())
    }

    /// The state as sent, for storing it with the session.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether `received` is this state, compared in constant time.
    pub fn matches(&self, received: &str) -> bool {
        // Lengths are compared first: a state's length is not secret (generated ones are all 43).
        self.0.len() == received.len() && bool::from(self.0.as_bytes().ct_eq(received.as_bytes()))
    }
}

impl fmt::Debug for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("State([REDACTED])")
    }
}

/// 32 bytes from the thread's CSPRNG (ChaCha, seeded from the operating system), base64url-encoded.
fn random_token() -> String {
    let mut bytes = [0_u8; RANDOM_BYTES];
    rand::rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}
