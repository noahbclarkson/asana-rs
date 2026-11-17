//! When a failed request is sent again, and how long the client waits first.
//!
//! - A `429` is retried for any method, because Asana did not act on the request, after exactly
//!   the `Retry-After` it gave.
//! - A `500`, `502`, `503` or `504`, and a transport failure that may have reached Asana, are
//!   retried with jittered exponential back-off only when sending twice is safe: `GET`, `PUT`,
//!   `DELETE`, or an operation marked idempotent. A `POST` that creates something is not.
//! - A transport failure that certainly never reached Asana (no connection) is retried for any
//!   method.

use std::time::Duration;

use asana_openapi::endpoint::Method;
use rand::RngExt as _;
use time::OffsetDateTime;
use time::macros::format_description;

/// How many times a request is sent, and the longest the client waits before sending it again.
#[derive(Clone, Copy, Debug, PartialEq, Eq, bon::Builder)]
#[non_exhaustive]
pub struct RetryPolicy {
    /// The most times one request is sent, the first included. At least 1. Default 5.
    #[builder(default = 5)]
    pub max_attempts: u32,
    /// The longest single wait before a retry. A `Retry-After` longer than this ends the call with
    /// [`Error::RateLimited`](crate::Error::RateLimited) instead of waiting. Default 60 seconds.
    #[builder(default = Duration::from_secs(60))]
    pub max_wait: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self::builder().build()
    }
}

impl RetryPolicy {
    /// Sends every request once and never retries.
    pub const fn never() -> Self {
        Self {
            max_attempts: 1,
            max_wait: Duration::ZERO,
        }
    }

    /// Whether another attempt is allowed after `attempts` were made.
    pub(crate) fn allows_another(&self, attempts: u32) -> bool {
        attempts < self.max_attempts
    }

    /// The jittered back-off before attempt `attempts + 1`, after a server or transport failure:
    /// half a second doubling each time, capped at 30 seconds and at `max_wait`, then drawn from
    /// its upper half.
    pub(crate) fn backoff(&self, attempts: u32) -> Duration {
        let exponent = attempts.saturating_sub(1).min(16);
        let ceiling = BACKOFF_BASE
            .saturating_mul(1 << exponent)
            .min(BACKOFF_CAP)
            .min(self.max_wait);
        let floor = ceiling / 2;
        if ceiling <= floor {
            return ceiling;
        }
        rand::rng().random_range(floor..=ceiling)
    }
}

const BACKOFF_BASE: Duration = Duration::from_millis(500);
const BACKOFF_CAP: Duration = Duration::from_secs(30);

/// Whether sending a request of this method twice leaves Asana as sending it once would.
pub(crate) fn safe_to_repeat(method: Method, idempotent: bool) -> bool {
    idempotent || matches!(method, Method::Get | Method::Put | Method::Delete)
}

/// Whether a status is a server failure worth retrying.
pub(crate) fn retryable_status(status: http::StatusCode) -> bool {
    matches!(status.as_u16(), 500 | 502 | 503 | 504)
}

/// Reads a `Retry-After` header: whole (or fractional) seconds, or an HTTP date.
pub(crate) fn retry_after(headers: &http::HeaderMap) -> Option<Duration> {
    let value = headers
        .get(http::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim();
    parse_retry_after(value, OffsetDateTime::now_utc())
}

fn parse_retry_after(value: &str, now: OffsetDateTime) -> Option<Duration> {
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    if let Ok(seconds) = value.parse::<f64>() {
        return Duration::try_from_secs_f64(seconds).ok();
    }
    let format = format_description!(
        "[weekday repr:short], [day] [month repr:short] [year] [hour]:[minute]:[second] GMT"
    );
    let at = time::PrimitiveDateTime::parse(value, format)
        .ok()?
        .assume_utc();
    Some(Duration::try_from(at - now).unwrap_or(Duration::ZERO))
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    #[test]
    fn retry_after_reads_seconds_and_http_dates() {
        let now = datetime!(2026-11-17 10:00:00 UTC);
        assert_eq!(parse_retry_after("7", now), Some(Duration::from_secs(7)));
        assert_eq!(
            parse_retry_after("1.5", now),
            Some(Duration::from_millis(1500))
        );
        assert_eq!(
            parse_retry_after("Tue, 17 Nov 2026 10:00:30 GMT", now),
            Some(Duration::from_secs(30))
        );
        assert_eq!(
            parse_retry_after("Tue, 17 Nov 2026 09:00:00 GMT", now),
            Some(Duration::ZERO)
        );
        assert_eq!(parse_retry_after("soon", now), None);
        assert_eq!(parse_retry_after("-3", now), None);
    }

    #[test]
    fn back_off_grows_and_stays_within_its_caps() {
        let policy = RetryPolicy::default();
        for attempts in 1..=10 {
            let wait = policy.backoff(attempts);
            let ceiling = (BACKOFF_BASE * (1 << (attempts - 1))).min(BACKOFF_CAP);
            assert!(
                wait >= ceiling / 2 && wait <= ceiling,
                "{attempts}: {wait:?}"
            );
        }
        let short = RetryPolicy::builder()
            .max_wait(Duration::from_millis(100))
            .build();
        assert!(short.backoff(8) <= Duration::from_millis(100));
    }

    #[test]
    fn only_methods_safe_to_repeat_are_retried_on_server_failures() {
        assert!(safe_to_repeat(Method::Get, false));
        assert!(safe_to_repeat(Method::Put, false));
        assert!(safe_to_repeat(Method::Delete, false));
        assert!(!safe_to_repeat(Method::Post, false));
        assert!(safe_to_repeat(Method::Post, true));
    }
}
