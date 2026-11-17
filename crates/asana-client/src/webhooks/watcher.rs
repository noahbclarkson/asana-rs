//! Polling one section for tasks that arrive in it, for receivers that cannot wait for Asana's
//! webhook latency, and the seen-set it shares with the [`Resolver`](super::Resolver).

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use asana_openapi::endpoint::PageLimit;
use asana_openapi::endpoints::GetTasksForSection;
use asana_openapi::fields::{Fields, TaskField};
use asana_openapi::ids::{SectionGid, TaskGid};
use asana_openapi::models::Task;
use asana_webhooks::{Clock, system_clock};
use time::OffsetDateTime;

use crate::{Client, Error};

/// Task arrivals already reported, by task and section, each remembered for a time to live.
///
/// Shared (it is a cheap handle; clones see the same set) between a [`SectionWatcher`] and a
/// [`Resolver`](super::Resolver) built with [`seen`](super::ResolverBuilder::seen), so whichever
/// notices a task entering a section first reports it and the other stays quiet. A departure
/// removes the task, so a later return is reported again. Meant to stay small: the tasks that
/// entered the watched sections within the time to live.
#[derive(Clone)]
pub struct SeenTasks {
    entries: Arc<Mutex<HashMap<(TaskGid, SectionGid), OffsetDateTime>>>,
    ttl: Duration,
    clock: Clock,
}

impl SeenTasks {
    /// An empty set whose entries last `ttl`, on the system clock. Make `ttl` longer than the
    /// longest a webhook event can lag behind the watcher (an hour covers almost all deliveries).
    #[must_use]
    pub fn new(ttl: Duration) -> Self {
        Self::with_clock(ttl, system_clock())
    }

    /// An empty set reading the time from `clock` (see
    /// [`ManualClock`](asana_webhooks::ManualClock)).
    #[must_use]
    pub fn with_clock(ttl: Duration, clock: Clock) -> Self {
        Self {
            entries: Arc::new(Mutex::new(HashMap::new())),
            ttl,
            clock,
        }
    }

    fn entries(&self) -> MutexGuard<'_, HashMap<(TaskGid, SectionGid), OffsetDateTime>> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Records that `task` entered `section`, and says whether this is the first report of it
    /// within the time to live. Check and record are one step.
    pub fn first_sighting(&self, task: &TaskGid, section: &SectionGid) -> bool {
        let now = (self.clock)();
        let mut entries = self.entries();
        entries.retain(|_, deadline| *deadline > now);
        let key = (task.clone(), section.clone());
        if entries.contains_key(&key) {
            return false;
        }
        entries.insert(key, now + self.ttl);
        true
    }

    /// Whether `task` entering `section` has been reported within the time to live.
    #[must_use]
    pub fn contains(&self, task: &TaskGid, section: &SectionGid) -> bool {
        let now = (self.clock)();
        self.entries()
            .get(&(task.clone(), section.clone()))
            .is_some_and(|deadline| *deadline > now)
    }

    /// Forgets that `task` entered `section`: it left, so a return is news.
    pub fn forget(&self, task: &TaskGid, section: &SectionGid) {
        self.entries().remove(&(task.clone(), section.clone()));
    }

    /// How many arrivals are remembered, expired ones included until the next record.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries().len()
    }

    /// Whether nothing is remembered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries().is_empty()
    }
}

impl fmt::Debug for SeenTasks {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SeenTasks")
            .field("entries", &self.len())
            .field("ttl", &self.ttl)
            .finish_non_exhaustive()
    }
}

/// Watches one section by listing it, for a receiver that must react within seconds rather than
/// Asana's webhook latency (about a minute on average).
///
/// Each [`poll`](Self::poll) is **one request** (`GET /sections/{gid}/tasks`, one page of up to
/// 100 tasks with the chosen fields) and returns the tasks that have appeared since the previous
/// poll and that the shared [`SeenTasks`] has not already reported. It has no timer and no
/// background task: call it on your own schedule. Every poll counts against the token's read
/// limits like any other request, so a poll every 5 seconds is 12 requests a minute per section;
/// keep the webhook for everything else, and the watcher for the one section where seconds matter.
///
/// The first poll records what is already in the section and returns nothing, unless
/// [`include_existing`](Self::include_existing) says otherwise. Arrivals while nothing polls are
/// the webhook's (or the events stream's) to report.
#[derive(Debug)]
pub struct SectionWatcher {
    section: SectionGid,
    fields: Fields<TaskField>,
    seen: SeenTasks,
    listed: Option<HashSet<TaskGid>>,
    include_existing: bool,
}

/// What one [`SectionWatcher::poll`] found.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct Arrivals {
    /// The tasks that appeared in the section since the previous poll, not reported before, in
    /// the section's order.
    pub tasks: Vec<Task>,
    /// Whether the listing held the whole section. A section of more than 100 tasks is only seen
    /// in part: arrivals beyond the first page are missed, and are the webhook's to report.
    pub complete: bool,
}

impl SectionWatcher {
    /// A watcher for `section`, sharing `seen` with the resolver.
    #[must_use]
    pub fn new(section: SectionGid, seen: SeenTasks) -> Self {
        Self {
            section,
            fields: Fields::none(),
            seen,
            listed: None,
            include_existing: false,
        }
    }

    /// The fields each task is listed with; by default Asana's compact record.
    #[must_use]
    pub fn fields(mut self, fields: Fields<TaskField>) -> Self {
        self.fields = fields;
        self
    }

    /// Whether the first poll reports the tasks already in the section (not already in the
    /// seen-set) as arrivals. Off by default.
    #[must_use]
    pub fn include_existing(mut self, include: bool) -> Self {
        self.include_existing = include;
        self
    }

    /// The section watched.
    #[must_use]
    pub fn section(&self) -> &SectionGid {
        &self.section
    }

    /// Lists the section once and returns the tasks that have arrived since the previous poll.
    ///
    /// A task that has left since the previous poll is removed from the seen-set, so its return
    /// is reported.
    ///
    /// # Errors
    ///
    /// The [`Error`] of the listing; the watcher's state is unchanged, so the next poll compares
    /// against the last successful one.
    pub async fn poll(&mut self, client: &Client) -> Result<Arrivals, Error> {
        let page = client
            .send_page(
                GetTasksForSection::builder(self.section.clone())
                    .limit(PageLimit::MAX)
                    .opt_fields(self.fields.clone())
                    .build(),
            )
            .await?;
        let complete = page.next.is_none();
        let first = self.listed.is_none();
        let previous = self.listed.take().unwrap_or_default();
        let mut listed = HashSet::with_capacity(page.items.len());
        let mut tasks = Vec::new();
        for task in page.items {
            let Some(gid) = task.gid.clone() else {
                continue;
            };
            let new = !previous.contains(&gid) && (!first || self.include_existing);
            if new && self.seen.first_sighting(&gid, &self.section) {
                tasks.push(task);
            }
            listed.insert(gid);
        }
        if complete {
            for gone in previous.difference(&listed) {
                self.seen.forget(gone, &self.section);
            }
        } else {
            // Tasks past the first page were not listed; they have not necessarily left.
            listed.extend(previous);
        }
        self.listed = Some(listed);
        Ok(Arrivals { tasks, complete })
    }
}
