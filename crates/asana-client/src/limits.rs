//! Asana's per-token limits, kept on the client side so requests wait instead of being refused.
//!
//! Asana counts concurrent reads and concurrent writes apart (50 and 15 per token), limits task
//! search to 60 a minute, and counts a refused request against the quota. So each client (which
//! holds one token) has a read semaphore, a write semaphore, a search bucket, and a cooldown: when
//! any request is answered `429`, no request from this client starts before the time Asana gave.

use std::sync::Mutex;
use std::time::Duration;

use asana_openapi::endpoint::Method;
use tokio::sync::{Semaphore, SemaphorePermit};
use tokio::time::Instant;

/// The client's own ceilings, set below Asana's so another tool sharing the token has room.
#[derive(Clone, Copy, Debug, PartialEq, Eq, bon::Builder)]
#[non_exhaustive]
pub struct Limits {
    /// Concurrent `GET` requests. Asana allows 50 per token. Default 40.
    #[builder(default = 40)]
    pub reads: u32,
    /// Concurrent `POST`, `PUT` and `DELETE` requests. Asana allows 15 per token. Default 12.
    #[builder(default = 12)]
    pub writes: u32,
    /// Workspace task searches a minute. Asana allows 60. Default 55.
    #[builder(default = 55)]
    pub searches_per_minute: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self::builder().build()
    }
}

/// The operations that count against the search limit.
const SEARCH_OPERATIONS: &[&str] = &["searchTasksForWorkspace"];

/// The live state behind [`Limits`] for one client.
pub(crate) struct Governor {
    reads: Semaphore,
    writes: Semaphore,
    search: SearchBucket,
    cooldown: Mutex<Option<Instant>>,
}

impl Governor {
    pub(crate) fn new(limits: Limits) -> Self {
        Self {
            reads: Semaphore::new(limits.reads.max(1) as usize),
            writes: Semaphore::new(limits.writes.max(1) as usize),
            search: SearchBucket::new(limits.searches_per_minute.max(1)),
            cooldown: Mutex::new(None),
        }
    }

    /// Waits until a request of this kind may start, and returns the permit it holds while it
    /// runs. The search bucket and the cooldown are waited out as well.
    pub(crate) async fn admit(
        &self,
        method: Method,
        operation: &str,
    ) -> Option<SemaphorePermit<'_>> {
        if SEARCH_OPERATIONS.contains(&operation) {
            self.search.take().await;
        }
        let semaphore = match method {
            Method::Get => &self.reads,
            Method::Post | Method::Put | Method::Delete => &self.writes,
        };
        // The semaphores are never closed, so this is always a permit.
        let permit = semaphore.acquire().await.ok();
        self.wait_out_cooldown().await;
        permit
    }

    /// Sleeps until no cooldown is in force. A `429` arriving while asleep extends the wait.
    async fn wait_out_cooldown(&self) {
        while let Some(until) = self.cooldown_until() {
            if until <= Instant::now() {
                return;
            }
            tokio::time::sleep_until(until).await;
        }
    }

    fn cooldown_until(&self) -> Option<Instant> {
        *self
            .cooldown
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Asana answered `429`: nothing from this client starts for `wait`.
    pub(crate) fn cool_down(&self, wait: Duration) {
        let until = Instant::now() + wait;
        let mut cooldown = self
            .cooldown
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if cooldown.is_none_or(|current| current < until) {
            *cooldown = Some(until);
        }
    }
}

/// A token bucket refilled evenly over a minute.
struct SearchBucket {
    per_minute: u32,
    state: Mutex<BucketState>,
}

struct BucketState {
    tokens: f64,
    refilled_at: Instant,
}

impl SearchBucket {
    fn new(per_minute: u32) -> Self {
        Self {
            per_minute,
            state: Mutex::new(BucketState {
                tokens: f64::from(per_minute),
                refilled_at: Instant::now(),
            }),
        }
    }

    async fn take(&self) {
        loop {
            let wait = {
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let now = Instant::now();
                let rate = f64::from(self.per_minute) / 60.0;
                let elapsed = now.duration_since(state.refilled_at).as_secs_f64();
                state.tokens = (state.tokens + elapsed * rate).min(f64::from(self.per_minute));
                state.refilled_at = now;
                if state.tokens >= 1.0 {
                    state.tokens -= 1.0;
                    return;
                }
                Duration::from_secs_f64((1.0 - state.tokens) / rate)
            };
            tokio::time::sleep(wait).await;
        }
    }
}
