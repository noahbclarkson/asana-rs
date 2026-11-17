//! The events API: what changed on a task, project or goal since a sync token.
//!
//! How Asana's events API works: a request without a sync token is answered `412` with a fresh
//! token and no events; events that happen after that are returned to a request that sends the
//! token, at most 100 at a time, each answer carrying the next token and `has_more`. A token that
//! has expired (Asana keeps events for a limited time) is answered `412` with a fresh token: the
//! events in between are lost, so the caller must read the resource again in full.
//!
//! The events are decoded into [`asana_webhooks::Event`], the type a webhook receiver gets, so
//! code that handles webhook deliveries handles polled events too. An event this crate cannot
//! read is handed over with its raw JSON rather than dropped.
//!
//! ```no_run
//! # async fn run(client: asana_client::Client, project: asana_client::openapi::ids::ProjectGid,
//! #     saved: Option<asana_client::SyncToken>) -> Result<(), asana_client::Error> {
//! use futures_util::StreamExt;
//! use asana_client::resources::events::EventsItem;
//!
//! let mut items = client.events().stream(&project, saved);
//! while let Some(item) = items.next().await {
//!     match item {
//!         Ok(EventsItem::Event { event, sync }) => { /* handle event, then save sync */ }
//!         Ok(EventsItem::Unreadable { sync, .. }) => { /* log it, then save sync */ }
//!         Ok(EventsItem::CaughtUp { sync }) => { /* save sync */ }
//!         Ok(_) => {}
//!         Err(asana_client::Error::SyncTokenExpired { sync }) => { /* re-read everything, save sync */ }
//!         Err(other) => return Err(other),
//!     }
//! }
//! # Ok(()) }
//! ```

use std::collections::VecDeque;
use std::time::Duration;

use asana_openapi::endpoint::{Endpoint, Envelope, Method, QueryPair};
use asana_openapi::endpoints::GetEvents;
use asana_openapi::ids::Gid;
use asana_openapi::scopes::Scope;
use asana_webhooks::{Event, EventBatch, UnreadableEvent};
use futures_util::StreamExt;
use futures_util::stream::{self, BoxStream};
use serde::Deserialize;
use serde_json::Value;

use crate::{Client, Error, Require, SyncToken};

impl Client {
    /// The events API: changes to a task, project or goal since a sync token.
    pub fn events(&self) -> Events<'_> {
        Events { client: self }
    }
}

/// The events operations, from [`Client::events`].
#[derive(Clone, Copy, Debug)]
pub struct Events<'a> {
    client: &'a Client,
}

/// One answer of the events API.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct EventsPage {
    /// The events that were read, oldest first.
    pub events: Vec<Event>,
    /// The events that could not be read, with their positions in the page.
    pub unreadable: Vec<UnreadableEvent>,
    /// The token to send next time.
    pub sync: SyncToken,
    /// Whether more events are waiting: ask again at once with `sync`.
    pub has_more: bool,
}

/// What [`Events::stream`] and [`Events::poll`] hand over.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum EventsItem {
    /// One event, and the token to save once it has been handled.
    ///
    /// Resuming from that token never misses an event but may repeat some: within a page the
    /// token is the one the page was asked for with, so a restart reads the page again. Handle
    /// events idempotently, or de-duplicate them on [`Event::key`].
    Event {
        /// The event.
        event: Box<Event>,
        /// The token to save after handling it.
        sync: SyncToken,
    },
    /// An event this crate could not read, with its raw JSON, and the token to save after it.
    Unreadable {
        /// The event as Asana sent it, and what was wrong with it.
        event: Box<UnreadableEvent>,
        /// The token to save after it.
        sync: SyncToken,
    },
    /// Every event Asana had was handed over; save this token. When the stream began without a
    /// token, its first item is this, carrying the token Asana issued: events start after it.
    CaughtUp {
        /// The token to save.
        sync: SyncToken,
    },
}

impl EventsItem {
    /// The token to save once this item has been handled.
    pub fn sync(&self) -> &SyncToken {
        match self {
            Self::Event { sync, .. } | Self::Unreadable { sync, .. } | Self::CaughtUp { sync } => {
                sync
            }
        }
    }
}

/// A stream of events, from [`Events::stream`] or [`Events::poll`].
pub type EventItems = BoxStream<'static, Result<EventsItem, Error>>;

impl Events<'_> {
    /// One request to the events API.
    ///
    /// Without a token, Asana's first-sync `412` is handled here: the answer is an empty page
    /// carrying the token Asana issued. With a token, an expired one is an error.
    ///
    /// Sends `getEvents` (`GET /events?resource=…&sync=…`); it names no granular scope, so an OAuth
    /// token needs the `default` scope (full permissions); retried like every read.
    ///
    /// # Errors
    ///
    /// [`Error::SyncTokenExpired`] when `token` has expired: events were lost, so read the
    /// resource again in full and continue from the fresh token it carries. Otherwise as
    /// [`Client::send`].
    pub async fn sync<G: Gid>(
        &self,
        resource: &G,
        token: Option<SyncToken>,
    ) -> Result<EventsPage, Error> {
        fetch(self.client, resource.as_str(), token).await
    }

    /// Every event since `token`, then [`EventsItem::CaughtUp`], then the end of the stream.
    ///
    /// Pages are read as the stream is: a page with `has_more` is followed at once by the next.
    /// The stream ends after its first error. Each page is one `getEvents` request, as
    /// [`sync`](Self::sync) describes.
    pub fn stream<G: Gid>(&self, resource: &G, token: Option<SyncToken>) -> EventItems {
        items(
            self.client.clone(),
            resource.as_str().to_owned(),
            token,
            None,
        )
    }

    /// As [`stream`](Self::stream), but on catching up it waits `every` (on tokio's clock) and
    /// asks again, for ever: a poller to stand in for a webhook. It ends only after an error,
    /// after which the caller resumes from the last token it saved.
    pub fn poll<G: Gid>(
        &self,
        resource: &G,
        token: Option<SyncToken>,
        every: Duration,
    ) -> EventItems {
        items(
            self.client.clone(),
            resource.as_str().to_owned(),
            token,
            Some(every),
        )
    }
}

/// Where a stream of events stands.
struct State {
    client: Client,
    resource: String,
    token: Option<SyncToken>,
    every: Option<Duration>,
    ready: VecDeque<EventsItem>,
    /// Whether the next request waits `every` first.
    wait_first: bool,
    finished: bool,
}

fn items(
    client: Client,
    resource: String,
    token: Option<SyncToken>,
    every: Option<Duration>,
) -> EventItems {
    let state = State {
        client,
        resource,
        token,
        every,
        ready: VecDeque::new(),
        wait_first: false,
        finished: false,
    };
    stream::unfold(state, |mut state| async move {
        loop {
            if let Some(item) = state.ready.pop_front() {
                return Some((Ok(item), state));
            }
            if state.finished {
                return None;
            }
            if state.wait_first
                && let Some(every) = state.every
            {
                tokio::time::sleep(every).await;
            }
            state.wait_first = false;
            let asked_with = state.token.clone();
            match fetch(&state.client, &state.resource, asked_with.clone()).await {
                Ok(page) => state.take_page(page, asked_with),
                Err(error) => {
                    state.finished = true;
                    return Some((Err(error), state));
                }
            }
        }
    })
    .boxed()
}

impl State {
    /// Queues a page's events, each with the token that resumes after it.
    fn take_page(&mut self, page: EventsPage, asked_with: Option<SyncToken>) {
        let EventsPage {
            events,
            unreadable,
            sync,
            has_more,
        } = page;
        let count = events.len() + unreadable.len();
        let mut events = events.into_iter();
        let mut unreadable = unreadable.into_iter().peekable();
        for index in 0..count {
            // Within the page, resuming means asking for the page again; after its last event,
            // resuming means asking with the page's own token.
            let resume = match (&asked_with, index + 1 == count) {
                (Some(asked), false) => asked.clone(),
                _ => sync.clone(),
            };
            let item = match unreadable.next_if(|bad| bad.index == index) {
                Some(bad) => EventsItem::Unreadable {
                    event: Box::new(bad),
                    sync: resume,
                },
                None => match events.next() {
                    Some(event) => EventsItem::Event {
                        event: Box::new(event),
                        sync: resume,
                    },
                    None => break,
                },
            };
            self.ready.push_back(item);
        }
        self.token = Some(sync.clone());
        if !has_more {
            self.ready.push_back(EventsItem::CaughtUp { sync });
            match self.every {
                Some(_) => self.wait_first = true,
                None => self.finished = true,
            }
        }
    }
}

/// One `getEvents` request, with the first sync's `412` turned into an empty page.
async fn fetch(
    client: &Client,
    resource: &str,
    token: Option<SyncToken>,
) -> Result<EventsPage, Error> {
    let first_sync = token.is_none();
    let endpoint = RawEvents(
        GetEvents::builder()
            .resource(resource)
            .maybe_sync(token.map(|token| token.as_str().to_owned()))
            .build(),
    );
    let raw = match client.send(endpoint).await {
        Ok(raw) => raw,
        Err(Error::SyncTokenExpired { sync }) if first_sync => {
            return Ok(EventsPage {
                events: Vec::new(),
                unreadable: Vec::new(),
                sync,
                has_more: false,
            });
        }
        Err(error) => return Err(error),
    };
    let sync = raw.sync.require("sync")?;
    let EventBatch {
        events, unreadable, ..
    } = EventBatch::from_values(raw.data);
    Ok(EventsPage {
        events,
        unreadable,
        sync,
        has_more: raw.has_more,
    })
}

/// The events API's answer before its events are read.
#[derive(Deserialize)]
struct RawPage {
    #[serde(default)]
    data: Vec<Value>,
    #[serde(default)]
    sync: Option<SyncToken>,
    #[serde(default)]
    has_more: bool,
}

/// The generated `getEvents`, answering with raw events so each is read on its own.
struct RawEvents(GetEvents);

impl Endpoint for RawEvents {
    type Output = RawPage;
    const OPERATION_ID: &'static str = GetEvents::OPERATION_ID;
    const METHOD: Method = GetEvents::METHOD;
    const ROUTE: &'static str = GetEvents::ROUTE;
    const SCOPE: Option<Scope> = GetEvents::SCOPE;
    const IDEMPOTENT: bool = GetEvents::IDEMPOTENT;
    const ENVELOPE: Envelope = Envelope::Whole;

    fn path(&self) -> String {
        self.0.path()
    }

    fn query(&self) -> Vec<QueryPair> {
        self.0.query()
    }
}
