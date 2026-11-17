//! Failures a test asks for, to check how its code handles Asana misbehaving.

use std::time::Duration;

use http::StatusCode;

/// One failure, answered to the next request it is queued for in place of the real answer.
///
/// Queue one with [`FakeAsana::fail_next`](crate::FakeAsana::fail_next) (for one operation) or
/// [`FakeAsana::fail_next_request`](crate::FakeAsana::fail_next_request) (for whatever comes
/// next). A request that meets a fault is still recorded, and a fault that answers changes
/// nothing in the fake, as a refused request changes nothing in Asana.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Fault {
    /// `429 Too Many Requests` with `Retry-After` in whole seconds (rounded up).
    RateLimited {
        /// How long Asana asks the caller to wait.
        retry_after: Duration,
    },
    /// Any status, with `message` in Asana's error shape.
    Status {
        /// The status to answer.
        status: StatusCode,
        /// The error message.
        message: String,
    },
    /// No answer: the transport reports a timeout. The request is not carried out.
    Timeout,
    /// The real answer, after a wait (on tokio's clock, so a paused test clock skips it).
    Slow(Duration),
}

impl Fault {
    /// `429` with `Retry-After`.
    pub fn rate_limited(retry_after: Duration) -> Self {
        Self::RateLimited { retry_after }
    }

    /// `500` with Asana's server-error body.
    pub fn server_error() -> Self {
        Self::Status {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: "Server Error".to_owned(),
        }
    }

    /// `402 Payment Required`, as Asana answers a premium-only call on a free workspace.
    pub fn premium_required() -> Self {
        Self::Status {
            status: StatusCode::PAYMENT_REQUIRED,
            message: "Search is only available to premium users.".to_owned(),
        }
    }

    /// Any status with `message`.
    pub fn status(status: StatusCode, message: impl Into<String>) -> Self {
        Self::Status {
            status,
            message: message.into(),
        }
    }

    /// No answer at all: the transport times out.
    pub fn timeout() -> Self {
        Self::Timeout
    }

    /// The real answer, after `delay`.
    pub fn slow(delay: Duration) -> Self {
        Self::Slow(delay)
    }
}

/// A fault waiting for a request: for one operation, or for any.
#[derive(Clone, Debug)]
pub(crate) struct QueuedFault {
    pub(crate) operation: Option<&'static str>,
    pub(crate) fault: Fault,
}

/// The error a [`Fault::Timeout`] hands the client as the timeout's source.
#[derive(Debug, thiserror::Error)]
#[error("the fake was told not to answer")]
pub(crate) struct FakeTimeout;
