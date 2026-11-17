//! Asana's per-token limits, kept on the client side so requests wait instead of being refused.
//!
//! Asana counts concurrent reads and concurrent writes apart (50 and 15 per token), limits task
//! search to 60 a minute, and counts a refused request against the quota. So each client (which
//! holds one token) has a read semaphore, a write semaphore, a search bucket, and a cooldown: when
//! any request is answered `429`, no request from this client starts before the time Asana gave.
//! A request that would have to wait out more of the cooldown than the retry policy's `max_wait`
//! fails at once with [`Error::RateLimited`](crate::Error::RateLimited) instead.

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

/// The longest cooldown when a wait overflows the clock. `Retry-After` is already capped at
/// [`MAX_RETRY_AFTER`](crate::retry::MAX_RETRY_AFTER), so this is only a backstop.
const MAX_COOLDOWN: Duration = Duration::from_secs(24 * 60 * 60);

/// The operations that count against the search limit.
const SEARCH_OPERATIONS: &[&str] = &["searchTasksForWorkspace"];

/// What one request counts against the limits: one read or one write, and a search token for a
/// search. A batch counts each of its actions, as Asana does.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Admission {
    reads: u32,
    writes: u32,
    searches: u32,
}

impl Admission {
    /// One request of this method and operation.
    pub(crate) fn of(method: Method, operation: &str) -> Self {
        let mut admission = Self::default();
        admission.add(method, operation);
        admission
    }

    /// Counts one more request (a batch action) of this method and operation.
    pub(crate) fn add(&mut self, method: Method, operation: &str) {
        match method {
            Method::Get => self.reads = self.reads.saturating_add(1),
            // Anything but a read counts as a write, a method Asana adds included.
            _ => self.writes = self.writes.saturating_add(1),
        }
        if SEARCH_OPERATIONS.contains(&operation) {
            self.searches = self.searches.saturating_add(1);
        }
    }
}

/// The permits an admitted request holds while it runs; dropping it gives them back.
pub(crate) struct Permits<'a> {
    // Held only to be dropped when the request ends.
    _reads: Option<SemaphorePermit<'a>>,
    _writes: Option<SemaphorePermit<'a>>,
}

/// The live state behind [`Limits`] for one client.
pub(crate) struct Governor {
    reads: Semaphore,
    read_capacity: u32,
    writes: Semaphore,
    write_capacity: u32,
    search: SearchBucket,
    cooldown: Mutex<Option<Instant>>,
}

impl Governor {
    pub(crate) fn new(limits: Limits) -> Self {
        let read_capacity = limits.reads.max(1);
        let write_capacity = limits.writes.max(1);
        Self {
            reads: Semaphore::new(read_capacity as usize),
            read_capacity,
            writes: Semaphore::new(write_capacity as usize),
            write_capacity,
            search: SearchBucket::new(limits.searches_per_minute.max(1)),
            cooldown: Mutex::new(None),
        }
    }

    /// Takes `wanted` permits of `semaphore`, or all of them when it holds fewer (a batch of ten
    /// reads under a limit of five takes the five, so it waits for every read to finish but never
    /// for ever). Reads are always taken before writes, so two batches cannot hold each other up.
    async fn take<'a>(
        semaphore: &'a Semaphore,
        capacity: u32,
        wanted: u32,
    ) -> Option<SemaphorePermit<'a>> {
        if wanted == 0 {
            return None;
        }
        // The semaphores are never closed, so this is always a permit.
        semaphore.acquire_many(wanted.min(capacity)).await.ok()
    }

    /// Waits until a request counting as `admission` may start, and returns the permits it holds
    /// while it runs. The search bucket and the cooldown are waited out as well, the cooldown for at most
    /// `max_wait`.
    ///
    /// # Errors
    ///
    /// How much of the cooldown is left, when that is longer than `max_wait`: the request is not
    /// sent. It is checked before queueing for a permit and again after.
    pub(crate) async fn admit(
        &self,
        admission: Admission,
        max_wait: Duration,
    ) -> Result<Permits<'_>, Duration> {
        self.refuse_a_long_cooldown(max_wait)?;
        for _ in 0..admission.searches {
            self.search.take().await;
        }
        let reads = Self::take(&self.reads, self.read_capacity, admission.reads).await;
        let writes = Self::take(&self.writes, self.write_capacity, admission.writes).await;
        self.wait_out_cooldown(max_wait).await?;
        Ok(Permits {
            _reads: reads,
            _writes: writes,
        })
    }

    /// The cooldown left, when it is longer than `max_wait`.
    fn refuse_a_long_cooldown(&self, max_wait: Duration) -> Result<(), Duration> {
        let now = Instant::now();
        match self.cooldown_until() {
            Some(until) if until.saturating_duration_since(now) > max_wait => {
                Err(until.saturating_duration_since(now))
            }
            _ => Ok(()),
        }
    }

    /// Sleeps until no cooldown is in force, for at most `max_wait` in all. A `429` arriving
    /// while asleep extends the cooldown; past `max_wait` from the start, the rest is refused.
    async fn wait_out_cooldown(&self, max_wait: Duration) -> Result<(), Duration> {
        let deadline = Instant::now().checked_add(max_wait);
        while let Some(until) = self.cooldown_until() {
            let now = Instant::now();
            if until <= now {
                return Ok(());
            }
            if deadline.is_some_and(|deadline| until > deadline) {
                return Err(until.saturating_duration_since(now));
            }
            tokio::time::sleep_until(until).await;
        }
        Ok(())
    }

    fn cooldown_until(&self) -> Option<Instant> {
        *self
            .cooldown
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Asana answered `429`: nothing from this client starts for `wait`. A wait too long for the
    /// clock to represent ends at the furthest instant it can.
    pub(crate) fn cool_down(&self, wait: Duration) {
        let now = Instant::now();
        let until = now
            .checked_add(wait)
            .or_else(|| now.checked_add(MAX_COOLDOWN))
            .unwrap_or(now);
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
