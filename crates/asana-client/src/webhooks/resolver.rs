//! Turning verified events into work: drop what should not be acted on, then group the rest into
//! the few shapes a receiver acts on.
//!
//! The resolver holds no policy of its own. It filters (the integration's own writes, stale
//! events, repeats) and groups (newest per task), and the consumer decides what each group means.

use std::collections::HashMap;
use std::future::{Future, ready};
use std::time::Duration;

use asana_openapi::endpoints::GetTask;
use asana_openapi::fields::{Fields, TaskField};
use asana_openapi::ids::{CustomFieldGid, ProjectGid, SectionGid, StoryGid, TaskGid, UserGid};
use asana_openapi::models::Task;
use asana_webhooks::{
    Dedupe, Event, EventAction, EventBatch, EventKey, ResourceGid, Sighting, StoreError,
};
use time::OffsetDateTime;

use super::watcher::SeenTasks;
use crate::{Client, Error};

/// How long a [`Dedupe`] remembers an event by default: a day, the longest Asana retries a
/// delivery.
const DEFAULT_DEDUPE_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// The [`Dedupe`] of a resolver built without one: every event is new.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoDedupe;

impl Dedupe for NoDedupe {
    fn observe(
        &self,
        _key: &EventKey,
        _ttl: Duration,
    ) -> impl Future<Output = Result<Sighting, StoreError>> + Send {
        ready(Ok(Sighting::First))
    }
}

/// Filters verified events and groups them by what they mean for each task.
///
/// Build one with [`Resolver::builder`]. It borrows its [`Dedupe`] store, so it is cheap to build
/// for each batch.
#[derive(Debug)]
pub struct Resolver<'d, D = NoDedupe> {
    ignored: Vec<UserGid>,
    max_age: Option<Duration>,
    dedupe: Option<&'d D>,
    dedupe_ttl: Duration,
    seen: Option<SeenTasks>,
}

impl Resolver<'static, NoDedupe> {
    /// A builder with nothing ignored, no age limit and no dedupe.
    pub fn builder() -> ResolverBuilder<'static, NoDedupe> {
        ResolverBuilder {
            ignored: Vec::new(),
            max_age: None,
            dedupe: None,
            dedupe_ttl: DEFAULT_DEDUPE_TTL,
            seen: None,
        }
    }
}

/// Builds a [`Resolver`].
#[derive(Debug)]
#[must_use = "a builder does nothing until it is built"]
pub struct ResolverBuilder<'d, D> {
    ignored: Vec<UserGid>,
    max_age: Option<Duration>,
    dedupe: Option<&'d D>,
    dedupe_ttl: Duration,
    seen: Option<SeenTasks>,
}

impl<'d, D> ResolverBuilder<'d, D> {
    /// Drops events this user caused: the integration's own account, so its own writes do not
    /// come back to it as work. May be called more than once.
    pub fn ignore_user(mut self, user: UserGid) -> Self {
        self.ignored.push(user);
        self
    }

    /// Drops events older than `max_age` when they are accepted. Asana usually delivers within a
    /// minute, sometimes much later; the events stream can return older ones still.
    pub fn max_age(mut self, max_age: Duration) -> Self {
        self.max_age = Some(max_age);
        self
    }

    /// Drops events `store` has already seen, by their [`EventKey`]. Asana retries a delivery it
    /// thinks failed, so the same events can arrive more than once.
    pub fn dedupe<E: Dedupe>(self, store: &E) -> ResolverBuilder<'_, E> {
        ResolverBuilder {
            ignored: self.ignored,
            max_age: self.max_age,
            dedupe: Some(store),
            dedupe_ttl: self.dedupe_ttl,
            seen: self.seen,
        }
    }

    /// How long the dedupe store remembers an event; a day by default, the longest Asana retries.
    pub fn dedupe_ttl(mut self, ttl: Duration) -> Self {
        self.dedupe_ttl = ttl;
        self
    }

    /// Shares a seen-set with a [`SectionWatcher`](super::SectionWatcher), so that a task the
    /// watcher already reported entering a section is not reported again when the webhook's event
    /// for it arrives, and the other way round.
    pub fn seen(mut self, seen: SeenTasks) -> Self {
        self.seen = Some(seen);
        self
    }

    /// The resolver.
    pub fn build(self) -> Resolver<'d, D> {
        Resolver {
            ignored: self.ignored,
            max_age: self.max_age,
            dedupe: self.dedupe,
            dedupe_ttl: self.dedupe_ttl,
            seen: self.seen,
        }
    }
}

impl<D: Dedupe> Resolver<'_, D> {
    /// The events of a verified batch worth acting on, in the order they came: not caused by an
    /// ignored user, not older than the age limit as of `now`, and not seen before.
    ///
    /// Events the batch could not read ([`EventBatch::unreadable`]) are not included; log them.
    ///
    /// # Errors
    ///
    /// The dedupe store's [`StoreError`]. Events checked before the failure are already recorded
    /// as seen, so a redelivery will skip them: on this error, keep the batch and process it
    /// without the dedupe step rather than waiting for Asana to send it again.
    pub async fn accept(
        &self,
        batch: &EventBatch,
        now: OffsetDateTime,
    ) -> Result<Vec<Event>, StoreError> {
        self.accept_events(&batch.events, now).await
    }

    /// As [`accept`](Self::accept), for events from somewhere other than one batch (a queue of
    /// stored events, say).
    ///
    /// # Errors
    ///
    /// As [`accept`](Self::accept).
    pub async fn accept_events(
        &self,
        events: &[Event],
        now: OffsetDateTime,
    ) -> Result<Vec<Event>, StoreError> {
        let mut kept = Vec::with_capacity(events.len());
        for event in events {
            if event
                .actor()
                .is_some_and(|actor| self.ignored.contains(actor))
            {
                continue;
            }
            if let Some(max_age) = self.max_age
                && event.is_older_than(max_age, now)
            {
                continue;
            }
            if let Some(store) = self.dedupe
                && store.observe(&event.key(), self.dedupe_ttl).await? == Sighting::Repeat
            {
                continue;
            }
            kept.push(event.clone());
        }
        Ok(kept)
    }
}

impl<D> Resolver<'_, D> {
    /// Groups events into what they mean, the newest event winning for each task, ordered by when
    /// each group's newest event happened.
    ///
    /// - A task's section: of all the task's section events, only the newest is kept, so a move
    ///   from one section to another (or through several) becomes one
    ///   [`SectionEntered`](Coalesced::SectionEntered) for where it ended up. At the same instant
    ///   an entry beats a departure, because a move sends both.
    /// - A task's membership of each project: the newest of added and removed.
    /// - A task's changed fields: one [`TaskChanged`](Coalesced::TaskChanged) with every field.
    /// - A deleted task: only [`Deleted`](Coalesced::Deleted), unless something happened to it
    ///   after the deletion.
    /// - A story added: once per story.
    /// - Anything else: [`Other`](Coalesced::Other), unchanged.
    ///
    /// With a [`SeenTasks`] set, a section entry already in it is dropped (and a new one is
    /// recorded), and a section departure removes the task from it.
    pub fn coalesce(&self, events: impl IntoIterator<Item = Event>) -> Vec<Coalesced> {
        let mut slots: HashMap<Slot, Entry> = HashMap::new();
        for (index, event) in events.into_iter().enumerate() {
            let (slot, departure, value) = classify(event, index);
            let rank = Rank {
                at: value.at(),
                arrival: !departure,
                index,
            };
            match slots.get_mut(&slot) {
                Some(existing) => existing.merge(rank, value),
                None => {
                    slots.insert(slot, Entry { rank, value });
                }
            }
        }

        let deletions: HashMap<TaskGid, Rank> = slots
            .iter()
            .filter_map(|(slot, entry)| match slot {
                Slot::Deleted(ResourceGid::Task(task)) => Some((task.clone(), entry.rank)),
                _ => None,
            })
            .collect();
        let mut entries: Vec<Entry> = slots
            .into_iter()
            .filter(|(slot, entry)| {
                slot.task()
                    .and_then(|task| deletions.get(task))
                    .is_none_or(|deleted| entry.rank > *deleted)
            })
            .map(|(_, entry)| entry)
            .collect();
        entries.sort_by_key(|entry| (entry.rank.at, entry.rank.index));

        entries
            .into_iter()
            .map(|entry| entry.value)
            .filter(|value| self.note_seen(value))
            .collect()
    }

    /// Records a section entry or departure in the seen-set; false when the entry was seen before.
    fn note_seen(&self, value: &Coalesced) -> bool {
        let Some(seen) = &self.seen else {
            return true;
        };
        match value {
            Coalesced::SectionEntered { task, section, .. } => seen.first_sighting(task, section),
            Coalesced::SectionLeft { task, section, .. } => {
                seen.forget(task, section);
                true
            }
            _ => true,
        }
    }
}

/// What a group of events means for the receiver, from [`Resolver::coalesce`].
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Coalesced {
    /// The task's newest section event put it in `section`.
    SectionEntered {
        /// The task.
        task: TaskGid,
        /// The section it entered. The event does not say which project the section is in.
        section: SectionGid,
        /// When.
        at: OffsetDateTime,
        /// Who moved it; `None` for the system.
        by: Option<UserGid>,
    },
    /// The task's newest section event took it out of `section`, without entering another.
    SectionLeft {
        /// The task.
        task: TaskGid,
        /// The section it left.
        section: SectionGid,
        /// When.
        at: OffsetDateTime,
        /// Who moved it; `None` for the system.
        by: Option<UserGid>,
    },
    /// Fields of the task changed.
    TaskChanged {
        /// The task.
        task: TaskGid,
        /// The fields that changed (`assignee`, `due_on`, `custom_fields`, …), each once, in the
        /// order they first changed.
        fields: Vec<String>,
        /// The custom fields whose values changed, when the events named them.
        custom_fields: Vec<CustomFieldGid>,
        /// When the newest change happened.
        at: OffsetDateTime,
        /// Who made the newest change; `None` for the system.
        by: Option<UserGid>,
    },
    /// The task was added to a project (its newest event for that project).
    TaskAdded {
        /// The task.
        task: TaskGid,
        /// The project.
        project: ProjectGid,
        /// When.
        at: OffsetDateTime,
        /// Who added it; `None` for the system.
        by: Option<UserGid>,
    },
    /// The task was removed from a project (its newest event for that project).
    TaskRemoved {
        /// The task.
        task: TaskGid,
        /// The project.
        project: ProjectGid,
        /// When.
        at: OffsetDateTime,
        /// Who removed it; `None` for the system.
        by: Option<UserGid>,
    },
    /// A story (a comment or an activity entry) was added. Asana consolidates stories, so it may
    /// no longer exist when read.
    StoryAdded {
        /// The story.
        story: StoryGid,
        /// The task it is on, when the event named it.
        task: Option<TaskGid>,
        /// When.
        at: OffsetDateTime,
        /// Who added it; `None` for the system.
        by: Option<UserGid>,
    },
    /// A resource was deleted.
    Deleted {
        /// The resource.
        resource: ResourceGid,
        /// When.
        at: OffsetDateTime,
        /// Who deleted it; `None` for the system.
        by: Option<UserGid>,
    },
    /// Any other event, unchanged.
    Other(Box<Event>),
}

impl Coalesced {
    /// The task this is about, when it is about one.
    #[must_use]
    pub fn task(&self) -> Option<&TaskGid> {
        match self {
            Self::SectionEntered { task, .. }
            | Self::SectionLeft { task, .. }
            | Self::TaskChanged { task, .. }
            | Self::TaskAdded { task, .. }
            | Self::TaskRemoved { task, .. } => Some(task),
            Self::StoryAdded { task, .. } => task.as_ref(),
            Self::Deleted {
                resource: ResourceGid::Task(task),
                ..
            } => Some(task),
            Self::Deleted { .. } => None,
            Self::Other(event) => match &event.resource.gid {
                ResourceGid::Task(task) => Some(task),
                _ => None,
            },
        }
    }

    /// When the newest event in the group happened.
    #[must_use]
    pub fn at(&self) -> OffsetDateTime {
        match self {
            Self::SectionEntered { at, .. }
            | Self::SectionLeft { at, .. }
            | Self::TaskChanged { at, .. }
            | Self::TaskAdded { at, .. }
            | Self::TaskRemoved { at, .. }
            | Self::StoryAdded { at, .. }
            | Self::Deleted { at, .. } => *at,
            Self::Other(event) => event.created_at,
        }
    }
}

/// What a group is keyed by.
#[derive(Clone, PartialEq, Eq, Hash)]
enum Slot {
    Section(TaskGid),
    Project(TaskGid, ProjectGid),
    Changed(TaskGid),
    Story(StoryGid),
    Deleted(ResourceGid),
    Other(usize),
}

impl Slot {
    /// The task a task-scoped group belongs to; a deletion's own slot is not one.
    fn task(&self) -> Option<&TaskGid> {
        match self {
            Self::Section(task) | Self::Project(task, _) | Self::Changed(task) => Some(task),
            _ => None,
        }
    }
}

/// Which of two events is newer: the later time, then an arrival over a departure, then the later
/// position in the input.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Rank {
    at: OffsetDateTime,
    arrival: bool,
    index: usize,
}

struct Entry {
    rank: Rank,
    value: Coalesced,
}

impl Entry {
    /// Takes in a newer event for the same slot: changed fields accumulate, anything else is
    /// replaced by the newer value.
    fn merge(&mut self, rank: Rank, value: Coalesced) {
        let newer = rank > self.rank;
        match (&mut self.value, value) {
            (
                Coalesced::TaskChanged {
                    fields,
                    custom_fields,
                    at,
                    by,
                    ..
                },
                Coalesced::TaskChanged {
                    fields: new_fields,
                    custom_fields: new_custom_fields,
                    at: new_at,
                    by: new_by,
                    ..
                },
            ) => {
                for field in new_fields {
                    if !fields.contains(&field) {
                        fields.push(field);
                    }
                }
                for custom_field in new_custom_fields {
                    if !custom_fields.contains(&custom_field) {
                        custom_fields.push(custom_field);
                    }
                }
                if newer {
                    *at = new_at;
                    *by = new_by;
                    self.rank = rank;
                }
            }
            (current, value) => {
                if newer {
                    *current = value;
                    self.rank = rank;
                }
            }
        }
    }
}

/// The slot an event belongs to, whether it is a departure, and what it means.
fn classify(event: Event, index: usize) -> (Slot, bool, Coalesced) {
    let at = event.created_at;
    let by = event.actor().cloned();
    if let Some((task, section)) = event.section_entered() {
        let slot = Slot::Section(task.clone());
        return (
            slot,
            false,
            Coalesced::SectionEntered {
                task,
                section,
                at,
                by,
            },
        );
    }
    if let Some((task, section)) = event.section_left() {
        let slot = Slot::Section(task.clone());
        return (
            slot,
            true,
            Coalesced::SectionLeft {
                task,
                section,
                at,
                by,
            },
        );
    }
    if let Some((task, project)) = event.task_added_to_project() {
        let slot = Slot::Project(task.clone(), project.clone());
        return (
            slot,
            false,
            Coalesced::TaskAdded {
                task,
                project,
                at,
                by,
            },
        );
    }
    if let Some((task, project)) = event.task_removed_from_project() {
        let slot = Slot::Project(task.clone(), project.clone());
        return (
            slot,
            true,
            Coalesced::TaskRemoved {
                task,
                project,
                at,
                by,
            },
        );
    }
    if let Some((task, field)) = event.task_changed() {
        let fields = vec![field.to_owned()];
        let custom_fields = event
            .custom_field_changed()
            .map(|(_, custom_field)| vec![custom_field])
            .unwrap_or_default();
        let slot = Slot::Changed(task.clone());
        let value = Coalesced::TaskChanged {
            task,
            fields,
            custom_fields,
            at,
            by,
        };
        return (slot, false, value);
    }
    if let Some((story, task)) = event.story_added() {
        let slot = Slot::Story(story.clone());
        return (
            slot,
            false,
            Coalesced::StoryAdded {
                story,
                task,
                at,
                by,
            },
        );
    }
    if event.action == EventAction::Deleted {
        let resource = event.resource.gid.clone();
        let slot = Slot::Deleted(resource.clone());
        return (slot, false, Coalesced::Deleted { resource, at, by });
    }
    (Slot::Other(index), false, Coalesced::Other(Box::new(event)))
}

/// Reads a task as it is now, with `fields`.
///
/// Events are compact and can be minutes old, and the task may have changed again since, so read
/// it before acting. Include `memberships.section` in `fields` to use [`confirm_in_section`].
///
/// ```no_run
/// use asana_client::openapi::fields;
/// use asana_client::webhooks::{confirm_in_section, reread};
/// # async fn run(
/// #     client: &asana_client::Client,
/// #     task: asana_client::openapi::ids::TaskGid,
/// #     section: asana_client::openapi::ids::SectionGid,
/// # ) -> Result<(), asana_client::Error> {
/// let now = reread(client, &task, fields![Task: name, memberships.section]).await?;
/// if confirm_in_section(&now, &section) {
///     // still there: act on it
/// }
/// # Ok(())
/// # }
/// ```
///
/// # Errors
///
/// Any [`Error`]; [`Error::NotFound`] when the task has been deleted since.
pub async fn reread(
    client: &Client,
    task: &TaskGid,
    fields: Fields<TaskField>,
) -> Result<Task, Error> {
    client
        .send(GetTask::builder(task.clone()).opt_fields(fields).build())
        .await
}

/// Whether `task` is in `section` now, by its `memberships`. Read the task with
/// `memberships.section` (see [`reread`]); without it this is always false.
#[must_use]
pub fn confirm_in_section(task: &Task, section: &SectionGid) -> bool {
    task.memberships.iter().flatten().any(|membership| {
        membership
            .section
            .as_ref()
            .and_then(|current| current.gid.as_ref())
            == Some(section)
    })
}
