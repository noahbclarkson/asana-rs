//! A cache of one project's sections by name and GID, for code that looks a section up often.

use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};

use asana_openapi::ids::{ProjectGid, SectionGid};
use futures_util::TryStreamExt;
use tokio::sync::Mutex;

use super::same_name;
use crate::{Client, Error, NameMatch};

/// Where a [`SectionDirectory`] reads the time, so a test can move it by hand.
pub trait Clock: Send + Sync + 'static {
    /// The present moment.
    fn now(&self) -> Instant;
}

/// The clock a [`SectionDirectory`] uses unless given another: tokio's, which a test can pause
/// and advance with `tokio::time::pause` and `tokio::time::advance`.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        tokio::time::Instant::now().into_std()
    }
}

/// One project's sections as last listed: GID and name, in the project's order.
struct Snapshot {
    listed_at: Instant,
    sections: Vec<(SectionGid, String)>,
}

/// A project's sections by name and GID, listed from Asana at most once per time-to-live.
///
/// Made for a webhook receiver that has to turn "the task moved to section 1201…" into a name,
/// or a name into a GID, on every delivery. The first lookup lists the project's sections
/// (`getSectionsForProject`, scope `projects:read`); later lookups answer from memory until
/// `ttl` has passed, when the next lookup lists them again. Concurrent lookups share one listing.
/// Nothing runs in the background.
///
/// A section renamed or added in Asana is seen once the time-to-live runs out, or at once after
/// [`refresh`](Self::refresh). A failed listing returns its error and leaves the previous
/// listing in place, so the next lookup tries again.
///
/// ```no_run
/// # async fn run(client: asana_client::Client, project: asana_client::openapi::ids::ProjectGid)
/// #     -> Result<(), asana_client::Error> {
/// use std::time::Duration;
///
/// let directory = client.sections().directory(project, Duration::from_secs(300));
/// if let Some(section) = directory.by_name("Send Documents").await?.one() {
///     println!("{section}");
/// }
/// # Ok(()) }
/// ```
pub struct SectionDirectory {
    client: Client,
    project: ProjectGid,
    ttl: Duration,
    clock: Arc<dyn Clock>,
    snapshot: Mutex<Option<Arc<Snapshot>>>,
}

impl fmt::Debug for SectionDirectory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SectionDirectory")
            .field("project", &self.project)
            .field("ttl", &self.ttl)
            .finish_non_exhaustive()
    }
}

impl SectionDirectory {
    /// A directory of `project`'s sections, listed through `client` and kept for `ttl`.
    pub fn new(client: Client, project: ProjectGid, ttl: Duration) -> Self {
        Self {
            client,
            project,
            ttl,
            clock: Arc::new(SystemClock),
            snapshot: Mutex::new(None),
        }
    }

    /// Reads the time from `clock` instead of tokio's.
    #[must_use]
    pub fn with_clock(mut self, clock: impl Clock) -> Self {
        self.clock = Arc::new(clock);
        self
    }

    /// The project whose sections this holds.
    pub fn project(&self) -> &ProjectGid {
        &self.project
    }

    /// The sections named `name`, trimmed and compared exactly, as
    /// [`Sections::find_by_name`](super::Sections::find_by_name) does.
    ///
    /// # Errors
    ///
    /// When the sections had to be listed and listing failed, as [`Client::send`].
    pub async fn by_name(&self, name: &str) -> Result<NameMatch<SectionGid>, Error> {
        self.find(name, false).await
    }

    /// The sections named `name`, trimmed and compared ignoring case.
    ///
    /// # Errors
    ///
    /// As [`by_name`](Self::by_name).
    pub async fn by_name_ignore_case(&self, name: &str) -> Result<NameMatch<SectionGid>, Error> {
        self.find(name, true).await
    }

    /// The name of a section of the project, or `None` when the project had no such section when
    /// last listed.
    ///
    /// # Errors
    ///
    /// As [`by_name`](Self::by_name).
    pub async fn name_of(&self, section: &SectionGid) -> Result<Option<String>, Error> {
        let snapshot = self.current().await?;
        Ok(snapshot
            .sections
            .iter()
            .find(|(gid, _)| gid == section)
            .map(|(_, name)| name.clone()))
    }

    /// Every section, GID and name, in the project's order.
    ///
    /// # Errors
    ///
    /// As [`by_name`](Self::by_name).
    pub async fn sections(&self) -> Result<Vec<(SectionGid, String)>, Error> {
        Ok(self.current().await?.sections.clone())
    }

    /// Lists the sections again now, whatever their age.
    ///
    /// # Errors
    ///
    /// As [`Client::send`]; the previous listing is kept.
    pub async fn refresh(&self) -> Result<(), Error> {
        let mut slot = self.snapshot.lock().await;
        *slot = Some(Arc::new(self.list().await?));
        Ok(())
    }

    /// Forgets the listing, so the next lookup lists the sections again.
    pub async fn invalidate(&self) {
        *self.snapshot.lock().await = None;
    }

    async fn find(&self, name: &str, ignore_case: bool) -> Result<NameMatch<SectionGid>, Error> {
        let snapshot = self.current().await?;
        let found = snapshot
            .sections
            .iter()
            .filter(|(_, candidate)| same_name(candidate, name, ignore_case))
            .map(|(gid, _)| gid.clone())
            .collect();
        Ok(NameMatch::from_matches(found))
    }

    /// The listing, made again first when there is none or it is older than the time-to-live.
    /// The lock is held while listing, so concurrent callers wait for one listing.
    async fn current(&self) -> Result<Arc<Snapshot>, Error> {
        let mut slot = self.snapshot.lock().await;
        let now = self.clock.now();
        if let Some(snapshot) = slot.as_ref()
            && now.saturating_duration_since(snapshot.listed_at) < self.ttl
        {
            return Ok(Arc::clone(snapshot));
        }
        let snapshot = Arc::new(self.list().await?);
        *slot = Some(Arc::clone(&snapshot));
        Ok(snapshot)
    }

    async fn list(&self) -> Result<Snapshot, Error> {
        let sections: Vec<_> = self
            .client
            .sections()
            .list(self.project.clone())
            .fields(asana_openapi::fields![Section: name])
            .stream()
            .try_collect()
            .await?;
        let listed_at = self.clock.now();
        Ok(Snapshot {
            listed_at,
            sections: sections
                .into_iter()
                .filter_map(|section| Some((section.gid?, section.name.unwrap_or_default())))
                .collect(),
        })
    }
}
