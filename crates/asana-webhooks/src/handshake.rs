//! The handshake across processes: routes, the state machine, and the store that holds it.
//!
//! Creating a webhook is one request that cannot finish on its own. While the creator's
//! `POST /webhooks` is still open, Asana calls the target with the new secret, and only once the
//! target has echoed it does the create return `201`, with the same secret in its body. The call
//! may land on a different process (or machine) from the one waiting on the create, so the two
//! meet in a [`HandshakeStore`] the consumer provides, keyed by a [`RouteKey`] the creator put in
//! the target URL:
//!
//! 1. The creator makes a fresh [`RouteKey`], calls [`expect`](HandshakeStore::expect), and sends
//!    `POST /webhooks` with a target such as `https://example.com/webhooks/asana/{route_key}`.
//! 2. The receiver gets the handshake on that path, parses the key from it, calls
//!    [`capture`](HandshakeStore::capture) with the secret, and answers with the reply it returns.
//!    A route nobody is expecting is refused, so a stranger cannot attach a webhook to it.
//! 3. The create returns `201`. The creator calls [`take`](HandshakeStore::take) and [`settle`]s
//!    the captured secret against the one in the response; on a mismatch it deletes the webhook.

use std::collections::HashMap;
use std::fmt;
use std::future::{Future, ready};
use std::str::FromStr;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use crate::clock::{Clock, deadline_after, system_clock};
use crate::signing::{HandshakeReply, WebhookSecret, handshake_reply};

/// The random part of a webhook's target path: 32 random bytes, written as 43 base64url characters.
///
/// It tells the receiver which subscription a call is for, so it can find that subscription's
/// secret without trusting anything in the request. It is not a secret in the sense the webhook's
/// secret is (it travels in a URL), but keys are stored as their [`digest`](Self::digest) so a
/// leaked table does not list live URLs, and its `Debug` shows only the start of that digest.
#[derive(Clone, PartialEq, Eq)]
pub struct RouteKey([u8; 32]);

impl RouteKey {
    /// A new key from the operating system's random source (through `rand`'s thread generator).
    #[must_use]
    pub fn generate() -> Self {
        let mut bytes = [0_u8; 32];
        rand::fill(&mut bytes);
        Self(bytes)
    }

    /// Reads a key from a path segment.
    ///
    /// # Errors
    ///
    /// [`RouteKeyError`] unless `segment` is exactly 43 base64url characters naming 32 bytes.
    pub fn parse(segment: &str) -> Result<Self, RouteKeyError> {
        if segment.len() != 43 {
            return Err(RouteKeyError);
        }
        let decoded = URL_SAFE_NO_PAD.decode(segment).map_err(|_| RouteKeyError)?;
        let bytes: [u8; 32] = decoded.try_into().map_err(|_| RouteKeyError)?;
        Ok(Self(bytes))
    }

    /// The SHA-256 of the key, for storing and looking it up without keeping the key itself.
    #[must_use]
    pub fn digest(&self) -> RouteDigest {
        RouteDigest(Sha256::digest(self.0).into())
    }
}

impl fmt::Display for RouteKey {
    /// The path segment: 43 base64url characters, unpadded.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&URL_SAFE_NO_PAD.encode(self.0))
    }
}

impl fmt::Debug for RouteKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let digest = self.digest().to_string();
        write!(f, "RouteKey(digest {}…)", &digest[..8])
    }
}

impl FromStr for RouteKey {
    type Err = RouteKeyError;

    fn from_str(segment: &str) -> Result<Self, Self::Err> {
        Self::parse(segment)
    }
}

/// A path segment that is not a [`RouteKey`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("the path does not hold a webhook route key")]
pub struct RouteKeyError;

impl RouteKeyError {
    /// The status a receiver should answer: `404`, as for any unknown path.
    #[must_use]
    pub fn status_hint(&self) -> u16 {
        404
    }
}

/// The SHA-256 of a [`RouteKey`]: what a store keeps. `Display` is 64 lowercase hex digits.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RouteDigest([u8; 32]);

impl RouteDigest {
    /// The digest's 32 bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// A digest from 32 stored bytes.
    #[must_use]
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

impl fmt::Display for RouteDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&hex::encode(self.0))
    }
}

impl fmt::Debug for RouteDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RouteDigest({self})")
    }
}

/// Proof that a handshake's secret was stored: holds the answer to send back.
#[derive(Clone, Debug)]
pub struct Captured {
    reply: HandshakeReply,
}

impl Captured {
    /// Made by a [`HandshakeStore`] implementation once it has stored `secret`.
    #[must_use]
    pub fn new(secret: &WebhookSecret) -> Self {
        Self {
            reply: handshake_reply(secret),
        }
    }

    /// The answer to send Asana.
    #[must_use]
    pub fn reply(&self) -> &HandshakeReply {
        &self.reply
    }

    /// The answer to send Asana, by value.
    #[must_use]
    pub fn into_reply(self) -> HandshakeReply {
        self.reply
    }
}

/// Where the creator of a webhook and the receiver of its handshake meet. Implement it over storage
/// every process can reach (a database table keyed by [`RouteDigest`]); [`MemoryHandshakeStore`] is
/// enough when one process both creates and receives.
///
/// Each route moves through these states:
///
/// ```text
///  expect ──▶ Expected ──capture──▶ Captured ──take, settle: same secret──▶ Established
///                │                     │
///                └─ttl passes─▶ Expired └──take, settle: secret differs──▶ Rejected
/// ```
///
/// - [`expect`](Self::expect) opens a route for `ttl`, which must outlast the create request (Asana
///   holds it open until the handshake is answered; allow a minute). A route already open is
///   refused with [`StoreError::RouteInUse`].
/// - [`capture`](Self::capture) is the receiver's side. It stores the secret and returns the reply.
///   A route nobody opened is [`StoreError::NotExpected`]; one whose `ttl` has passed is
///   [`StoreError::Expired`]. Capturing the same secret twice (Asana retrying) succeeds again; a
///   different one is [`StoreError::AlreadyCaptured`].
/// - [`take`](Self::take) is the creator's side, after the create returns. It closes the route and
///   returns the captured secret, `None` if the route is unknown or nothing was captured, or
///   [`StoreError::Expired`] when the `ttl` passed first. The creator then calls [`settle`], and
///   stores the established secret with the webhook's GID.
pub trait HandshakeStore: Send + Sync {
    /// Opens `route` for a handshake for `ttl`.
    fn expect(
        &self,
        route: &RouteKey,
        ttl: Duration,
    ) -> impl Future<Output = Result<(), StoreError>> + Send;

    /// Stores the secret a handshake on `route` carried.
    fn capture(
        &self,
        route: &RouteKey,
        secret: WebhookSecret,
    ) -> impl Future<Output = Result<Captured, StoreError>> + Send;

    /// Closes `route` and returns what was captured on it.
    fn take(
        &self,
        route: &RouteKey,
    ) -> impl Future<Output = Result<Option<WebhookSecret>, StoreError>> + Send;
}

/// How a handshake ended, from the creator's side.
#[derive(Debug)]
#[non_exhaustive]
pub enum HandshakeOutcome {
    /// The secret the receiver captured is the one Asana returned: keep it with the webhook.
    Established(WebhookSecret),
    /// The receiver captured nothing on this route, yet the create succeeded: the handshake was
    /// answered by something else. Delete the webhook.
    NotCaptured,
    /// The receiver captured a different secret from the one Asana returned. Delete the webhook.
    Rejected,
}

/// Compares the secret [`take`](HandshakeStore::take) returned with the one in the `201` response
/// that created the webhook, in constant time.
#[must_use]
pub fn settle(captured: Option<WebhookSecret>, returned: &WebhookSecret) -> HandshakeOutcome {
    match captured {
        None => HandshakeOutcome::NotCaptured,
        Some(captured) if captured.ct_eq(returned) => HandshakeOutcome::Established(captured),
        Some(_) => HandshakeOutcome::Rejected,
    }
}

/// Why a store refused, or could not answer.
///
/// `Display` never contains a secret or a route key.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StoreError {
    /// No handshake is expected on this route.
    #[error("no webhook handshake is expected on this route")]
    NotExpected,
    /// The route was opened, but its time ran out first.
    #[error("the webhook handshake on this route expired")]
    Expired,
    /// A different secret was already captured on this route.
    #[error("a different secret was already captured on this route")]
    AlreadyCaptured,
    /// The route is already open for another handshake.
    #[error("this route is already open for a webhook handshake")]
    RouteInUse,
    /// The storage behind the store failed.
    #[error("the webhook store failed")]
    Backend(#[source] Box<dyn std::error::Error + Send + Sync>),
}

impl StoreError {
    /// Wraps an error from the storage behind a store.
    pub fn backend(error: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::Backend(Box::new(error))
    }

    /// The status a receiver should answer: `404` for a route that is not (or no longer) open,
    /// `409` for a conflict, `503` when the storage failed. Any of them fails Asana's create, which
    /// is the point: never answer a handshake `200` without having stored its secret.
    #[must_use]
    pub fn status_hint(&self) -> u16 {
        match self {
            Self::NotExpected | Self::Expired => 404,
            Self::AlreadyCaptured | Self::RouteInUse => 409,
            Self::Backend(_) => 503,
        }
    }
}

struct Entry {
    deadline: OffsetDateTime,
    secret: Option<WebhookSecret>,
}

/// A [`HandshakeStore`] in this process's memory, for when one process creates webhooks and
/// receives their handshakes, and for tests. Entries past their `ttl` are dropped as new routes
/// are opened.
pub struct MemoryHandshakeStore {
    entries: Mutex<HashMap<RouteDigest, Entry>>,
    clock: Clock,
}

impl MemoryHandshakeStore {
    /// An empty store on the system clock.
    #[must_use]
    pub fn new() -> Self {
        Self::with_clock(system_clock())
    }

    /// An empty store reading the time from `clock` (see [`ManualClock`](crate::ManualClock)).
    #[must_use]
    pub fn with_clock(clock: Clock) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            clock,
        }
    }

    fn entries(&self) -> std::sync::MutexGuard<'_, HashMap<RouteDigest, Entry>> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn expect_now(&self, route: &RouteKey, ttl: Duration) -> Result<(), StoreError> {
        let now = (self.clock)();
        let mut entries = self.entries();
        entries.retain(|_, entry| entry.deadline > now);
        let digest = route.digest();
        if entries.contains_key(&digest) {
            return Err(StoreError::RouteInUse);
        }
        entries.insert(
            digest,
            Entry {
                deadline: deadline_after(now, ttl),
                secret: None,
            },
        );
        Ok(())
    }

    fn capture_now(&self, route: &RouteKey, secret: WebhookSecret) -> Result<Captured, StoreError> {
        let now = (self.clock)();
        let mut entries = self.entries();
        let digest = route.digest();
        let entry = entries.get_mut(&digest).ok_or(StoreError::NotExpected)?;
        if entry.deadline <= now {
            entries.remove(&digest);
            return Err(StoreError::Expired);
        }
        match &entry.secret {
            Some(existing) if existing.ct_eq(&secret) => Ok(Captured::new(existing)),
            Some(_) => Err(StoreError::AlreadyCaptured),
            None => {
                let captured = Captured::new(&secret);
                entry.secret = Some(secret);
                Ok(captured)
            }
        }
    }

    fn take_now(&self, route: &RouteKey) -> Result<Option<WebhookSecret>, StoreError> {
        let now = (self.clock)();
        let Some(entry) = self.entries().remove(&route.digest()) else {
            return Ok(None);
        };
        if entry.deadline <= now {
            return Err(StoreError::Expired);
        }
        Ok(entry.secret)
    }
}

impl Default for MemoryHandshakeStore {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for MemoryHandshakeStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MemoryHandshakeStore")
            .field("routes", &self.entries().len())
            .finish_non_exhaustive()
    }
}

impl HandshakeStore for MemoryHandshakeStore {
    fn expect(
        &self,
        route: &RouteKey,
        ttl: Duration,
    ) -> impl Future<Output = Result<(), StoreError>> + Send {
        ready(self.expect_now(route, ttl))
    }

    fn capture(
        &self,
        route: &RouteKey,
        secret: WebhookSecret,
    ) -> impl Future<Output = Result<Captured, StoreError>> + Send {
        ready(self.capture_now(route, secret))
    }

    fn take(
        &self,
        route: &RouteKey,
    ) -> impl Future<Output = Result<Option<WebhookSecret>, StoreError>> + Send {
        ready(self.take_now(route))
    }
}
