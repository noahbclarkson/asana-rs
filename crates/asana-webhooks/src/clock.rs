//! Where the in-memory stores read the time, so tests can move it instead of sleeping.

use std::sync::{Arc, Mutex, PoisonError};

use time::{OffsetDateTime, PrimitiveDateTime};

/// A source of the current time, shared by a store and whoever drives it.
pub type Clock = Arc<dyn Fn() -> OffsetDateTime + Send + Sync>;

/// The system clock, in UTC.
#[must_use]
pub fn system_clock() -> Clock {
    Arc::new(OffsetDateTime::now_utc)
}

/// `now` plus `ttl`, or the latest time there is when that is past it, so that a time to live of
/// `Duration::MAX` ("never forget") never panics.
pub(crate) fn deadline_after(now: OffsetDateTime, ttl: std::time::Duration) -> OffsetDateTime {
    time::Duration::try_from(ttl)
        .ok()
        .and_then(|ttl| now.checked_add(ttl))
        .unwrap_or_else(|| PrimitiveDateTime::MAX.assume_utc())
}

/// A clock that only moves when told to, for tests.
///
/// ```
/// use asana_webhooks::ManualClock;
/// use std::time::Duration;
///
/// let clock = ManualClock::new(time::macros::datetime!(2026-01-01 00:00 UTC));
/// clock.advance(Duration::from_secs(90));
/// assert_eq!(clock.now(), time::macros::datetime!(2026-01-01 00:01:30 UTC));
/// ```
#[derive(Clone, Debug)]
pub struct ManualClock(Arc<Mutex<OffsetDateTime>>);

impl ManualClock {
    /// A clock reading `start`.
    #[must_use]
    pub fn new(start: OffsetDateTime) -> Self {
        Self(Arc::new(Mutex::new(start)))
    }

    /// The time it reads.
    #[must_use]
    pub fn now(&self) -> OffsetDateTime {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Moves it forward by `by`, at most to the latest time there is.
    pub fn advance(&self, by: std::time::Duration) {
        let mut now = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        *now = deadline_after(*now, by);
    }

    /// This clock as a [`Clock`] for a store; it keeps following [`advance`](Self::advance).
    #[must_use]
    pub fn clock(&self) -> Clock {
        let shared = self.clone();
        Arc::new(move || shared.now())
    }
}
