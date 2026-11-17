//! Processing each event at most once, by its [`EventKey`].
//!
//! Asana retries a delivery that was not answered `2xx` within ten seconds, so a receiver that
//! answered late, or crashed after acting but before answering, sees the same events again. A
//! [`Dedupe`] remembers keys for a while and says whether a key is new.

use std::collections::HashMap;
use std::fmt;
use std::future::{Future, ready};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use time::OffsetDateTime;

use crate::clock::{Clock, system_clock};
use crate::events::EventKey;
use crate::handshake::StoreError;

/// Whether an event key had been seen before.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sighting {
    /// Not seen within its time to live: process the event. The key is now remembered.
    First,
    /// Seen already: skip the event.
    Repeat,
}

/// A memory of recently seen [`EventKey`]s. Implement it over shared storage when several
/// processes receive; [`MemoryDedupe`] serves one process.
pub trait Dedupe: Send + Sync {
    /// Records `key` for `ttl` and says whether it was new, as one atomic step (so two deliveries
    /// racing on the same key cannot both see [`Sighting::First`]).
    ///
    /// Asana retries for up to 24 hours, so a `ttl` of a day covers every redelivery.
    fn observe(
        &self,
        key: &EventKey,
        ttl: Duration,
    ) -> impl Future<Output = Result<Sighting, StoreError>> + Send;
}

/// A [`Dedupe`] in this process's memory, with time from an injected [`Clock`].
///
/// Expired keys are swept when the map has doubled since the last sweep, so memory stays
/// proportional to the keys seen within the longest `ttl`.
pub struct MemoryDedupe {
    state: Mutex<State>,
    clock: Clock,
}

struct State {
    seen: HashMap<EventKey, OffsetDateTime>,
    sweep_at: usize,
}

const FIRST_SWEEP: usize = 1_024;

impl MemoryDedupe {
    /// An empty memory on the system clock.
    #[must_use]
    pub fn new() -> Self {
        Self::with_clock(system_clock())
    }

    /// An empty memory reading the time from `clock`.
    #[must_use]
    pub fn with_clock(clock: Clock) -> Self {
        Self {
            state: Mutex::new(State {
                seen: HashMap::new(),
                sweep_at: FIRST_SWEEP,
            }),
            clock,
        }
    }

    fn observe_now(&self, key: &EventKey, ttl: Duration) -> Sighting {
        let now = (self.clock)();
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.seen.len() >= state.sweep_at {
            state.seen.retain(|_, deadline| *deadline > now);
            state.sweep_at = (state.seen.len() * 2).max(FIRST_SWEEP);
        }
        match state.seen.get(key) {
            Some(deadline) if *deadline > now => Sighting::Repeat,
            _ => {
                state.seen.insert(*key, now + ttl);
                Sighting::First
            }
        }
    }
}

impl Default for MemoryDedupe {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for MemoryDedupe {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        f.debug_struct("MemoryDedupe")
            .field("keys", &state.seen.len())
            .finish_non_exhaustive()
    }
}

impl Dedupe for MemoryDedupe {
    fn observe(
        &self,
        key: &EventKey,
        ttl: Duration,
    ) -> impl Future<Output = Result<Sighting, StoreError>> + Send {
        ready(Ok(self.observe_now(key, ttl)))
    }
}
