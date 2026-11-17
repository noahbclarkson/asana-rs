//! Webhooks, the side that needs the HTTP client: establishing one across the handshake, looking
//! after it, and turning verified deliveries into work.
//!
//! The protocol itself (classifying a request, answering the handshake, checking signatures,
//! decoding events) is [`asana_webhooks`], which needs no HTTP client and no web framework. This
//! module adds what does need the client: `POST /webhooks` with the handshake around it,
//! [`health`](Webhooks::health), listing and deleting, the [`Resolver`] that turns events into
//! the few shapes a receiver acts on, and the [`SectionWatcher`] for receivers that cannot wait
//! for Asana's delivery latency.
//!
//! # Running webhooks in production
//!
//! ## Establishing one, across instances
//!
//! Asana calls the target with the new webhook's secret *while* the creating `POST /webhooks` is
//! still open, and only answers that request once the target has echoed the secret. Behind a load
//! balancer that call may reach a different instance from the one waiting on the create, so the
//! two meet in a [`HandshakeStore`](asana_webhooks::HandshakeStore) every instance can reach (a
//! database table keyed by [`RouteDigest`](asana_webhooks::RouteDigest)).
//! [`MemoryHandshakeStore`](asana_webhooks::MemoryHandshakeStore) is enough only when one process
//! both creates and receives.
//!
//! [`Webhooks::establish`] does the creator's side: it makes a fresh
//! [`RouteKey`], opens it in the store, sends the create with the target
//! `{base}/{route_key}`, then takes what the receiver captured and compares it with the secret in
//! Asana's answer in constant time. On success keep the [`Established`] webhook's GID, its
//! [`secret`](Established::secret) (encrypted: it signs every delivery) and its
//! [`route`](Established::route) (the receiver looks the secret up by it). Every way it can fail
//! after Asana created the webhook deletes the webhook again, and the error's [`Cleanup`] says
//! whether that worked.
//!
//! The create is one request that lasts as long as Asana's handshake, so it is sent with a
//! timeout of its own (90 seconds, [`Establish::timeout`]) through [`crate::RequestTimeout`].
//! A transport that ignores that extension needs a total timeout of at least a minute, or the
//! create is cut off while Asana is still calling the receiver.
//!
//! ## Answering deliveries
//!
//! - **Answer within 10 seconds.** A delivery not answered `2xx` in time is retried with back-off
//!   for up to 24 hours. Verify the signature, store or enqueue the batch, answer `200`, and do the
//!   work afterwards.
//! - **Heartbeats.** A delivery with no events arrives right after the handshake and every eight
//!   hours. Answer it `200` like any other.
//! - **`410 Gone`** deletes the webhook: answer it for a route you no longer want, never for one
//!   you merely failed to process.
//! - **Deletion after failures.** After 24 hours without a successful delivery (heartbeats count)
//!   Asana deletes the webhook. [`Webhooks::health`] shows it coming:
//!   [`WebhookHealth::is_failing`] and [`WebhookHealth::will_be_deleted_at`]. Check it on a
//!   schedule and re-establish a webhook that is gone.
//!
//! ## What arrives, and when
//!
//! - **At most once.** A delivery Asana gives up on is lost. The events stream is the catch-up:
//!   keep a sync token per resource, read it after an outage or on a slow schedule with
//!   [`Events::stream`](crate::resources::events::Events::stream) (or keep it running with
//!   [`Events::poll`](crate::resources::events::Events::poll)), and feed the events through the
//!   same [`Resolver`] with [`accept_events`](Resolver::accept_events). They are the same
//!   [`Event`](asana_webhooks::Event) type, and the stream can repeat a page after a restart, so
//!   the dedupe store drops what the webhook already delivered.
//! - **Compact and late.** Events name the resource and little else, can arrive minutes after the
//!   change, and the resource may have changed again since. Drop stale ones
//!   ([`ResolverBuilder::max_age`]), drop repeats ([`ResolverBuilder::dedupe`]), and
//!   [`reread`] the task before acting on it.
//! - **Latency.** Asana documents about a minute between a change and its delivery on average,
//!   most within ten minutes, and occasionally longer. Nothing on the receiving side shortens
//!   that. A receiver that must react within seconds pairs the webhook with a
//!   [`SectionWatcher`], polling the one section it cares about every few seconds; a shared
//!   [`SeenTasks`] keeps the two from acting twice on the same arrival.
//!
//! # From a verified batch to work
//!
//! ```
//! use std::time::Duration;
//!
//! use asana_client::webhooks::{Coalesced, Resolver};
//! use asana_webhooks::{Event, EventBatch, MemoryDedupe};
//! use time::macros::datetime;
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() -> Result<(), Box<dyn std::error::Error>> {
//! // Events the receiver verified and queued earlier: a task moved from one section to another.
//! let events: Vec<Event> = serde_json::from_str(r#"[
//!   {"user": {"gid": "1201234567890001", "resource_type": "user"},
//!    "created_at": "2026-03-02T09:15:00.000Z", "action": "removed",
//!    "resource": {"gid": "1201234567890123", "resource_type": "task"},
//!    "parent": {"gid": "1201234567890401", "resource_type": "section"}},
//!   {"user": {"gid": "1201234567890001", "resource_type": "user"},
//!    "created_at": "2026-03-02T09:15:00.000Z", "action": "added",
//!    "resource": {"gid": "1201234567890123", "resource_type": "task"},
//!    "parent": {"gid": "1201234567890402", "resource_type": "section"}}
//! ]"#)?;
//! let batch = EventBatch { events, ..EventBatch::default() };
//!
//! let dedupe = MemoryDedupe::new();
//! let resolver = Resolver::builder()
//!     .ignore_user("1201234567890999".parse()?) // the integration's own account
//!     .max_age(Duration::from_secs(3600))
//!     .dedupe(&dedupe)
//!     .build();
//!
//! let now = datetime!(2026-03-02 09:16 UTC);
//! let fresh = resolver.accept(&batch, now).await?;
//! for change in resolver.coalesce(fresh) {
//!     match change {
//!         Coalesced::SectionEntered { task, section, .. } => {
//!             assert_eq!(section.to_string(), "1201234567890402");
//!             // Re-read before acting: `reread(&client, &task, fields).await?` and
//!             // `confirm_in_section(&task_now, &section)`.
//!             let _ = task;
//!         }
//!         other => panic!("expected one section entry, got {other:?}"),
//!     }
//! }
//!
//! // The same delivery again (Asana retried it) produces nothing.
//! assert!(resolver.accept(&batch, now).await?.is_empty());
//! # Ok(())
//! # }
//! ```

mod error;
mod establish;
mod health;
mod resolver;
mod watcher;

use asana_openapi::endpoints::{DeleteWebhook, GetWebhook, GetWebhooks, UpdateWebhook};
use asana_openapi::fields::{Fields, WebhookField};
use asana_openapi::ids::{WebhookGid, WorkspaceGid};
use asana_openapi::models::{Webhook, WebhookUpdateRequest};
use asana_webhooks::{EventAction, Filter, FilterError, ResourceGid, RouteKey};

use crate::{Client, Error, Items, Pages};

pub use error::{Cleanup, WebhookError};
pub use establish::{Establish, Established};
pub use health::WebhookHealth;
pub use resolver::{Coalesced, NoDedupe, Resolver, ResolverBuilder, confirm_in_section, reread};
pub use watcher::{Arrivals, SectionWatcher, SeenTasks};

impl Client {
    /// Webhooks: establish, inspect, list and delete them.
    pub fn webhooks(&self) -> Webhooks<'_> {
        Webhooks { client: self }
    }
}

/// The webhook calls, from [`Client::webhooks`].
#[derive(Clone, Copy, Debug)]
pub struct Webhooks<'c> {
    client: &'c Client,
}

impl<'c> Webhooks<'c> {
    /// Starts establishing a webhook on `resource` that delivers to `target_base_url` plus a fresh
    /// route key: `{target_base_url}/{route_key}`.
    ///
    /// The base must be an `https` URL with no query, fragment or credentials. Add filters, give
    /// it the [`HandshakeStore`](asana_webhooks::HandshakeStore) its receiver captures into with
    /// [`store`](Establish::store), and [`send`](Establish::send) it.
    ///
    /// ```no_run
    /// use asana_client::{Auth, Client};
    /// use asana_client::openapi::ids::ProjectGid;
    /// use asana_webhooks::{EventAction, Filter, MemoryHandshakeStore};
    ///
    /// # async fn run() -> Result<(), Box<dyn std::error::Error>> {
    /// let client = Client::builder()
    ///     .auth(Auth::personal_access_token("0/made-up"))
    ///     .build()?;
    /// // Shared with the receiver; across instances, a store over a database instead.
    /// let store = MemoryHandshakeStore::new();
    /// let project: ProjectGid = "1201234567890456".parse()?;
    ///
    /// let established = client
    ///     .webhooks()
    ///     .establish(project, "https://hooks.example.com/asana")
    ///     .filter(Filter::task().action(EventAction::Added))
    ///     .filter(Filter::task().action(EventAction::Changed).fields(["completed", "due_on"]))
    ///     .store(&store)
    ///     .send()
    ///     .await?;
    ///
    /// // Keep all three: the GID to manage it, the secret (encrypted) to verify deliveries, and
    /// // the route the receiver finds the secret by.
    /// let _ = (&established.webhook.gid, established.secret.expose(), established.route.digest());
    /// # Ok(())
    /// # }
    /// ```
    pub fn establish(
        &self,
        resource: impl Into<ResourceGid>,
        target_base_url: impl Into<String>,
    ) -> Establish<'c, ()> {
        Establish::new(
            self.client,
            resource.into(),
            establish::Target::Base(target_base_url.into()),
        )
    }

    /// Starts establishing a webhook on `resource` that delivers to exactly `target_url`, which
    /// must contain `route` (in its path or query) so the receiver can find the handshake.
    ///
    /// For a target whose shape the caller controls, such as one with per-subscription state in
    /// its query; Asana delivers to the full URL, query included.
    pub fn establish_at(
        &self,
        resource: impl Into<ResourceGid>,
        target_url: impl Into<String>,
        route: RouteKey,
    ) -> Establish<'c, ()> {
        Establish::new(
            self.client,
            resource.into(),
            establish::Target::Exact {
                url: target_url.into(),
                route,
            },
        )
    }

    /// Reads a webhook's full record.
    ///
    /// The record never holds the secret: Asana returns it once, when the webhook is created.
    ///
    /// # Errors
    ///
    /// Any [`Error`]; [`Error::NotFound`] when the webhook is gone (Asana deletes failing ones).
    pub async fn get(&self, webhook: &WebhookGid) -> Result<Webhook, Error> {
        self.client
            .send(
                GetWebhook::builder(webhook.clone())
                    .opt_fields(health::full_record())
                    .build(),
            )
            .await
    }

    /// Reads how a webhook's deliveries are going: whether it is failing, and when Asana will
    /// delete it if it keeps failing.
    ///
    /// # Errors
    ///
    /// Any [`Error`]; [`Error::NotFound`] when the webhook is gone.
    pub async fn health(&self, webhook: &WebhookGid) -> Result<WebhookHealth, Error> {
        self.get(webhook)
            .await
            .map(|record| WebhookHealth::from_webhook(&record))
    }

    /// Lists the webhooks this token created in `workspace`; narrow it to one resource with
    /// [`ListWebhooks::resource`].
    pub fn list(&self, workspace: WorkspaceGid) -> ListWebhooks<'c> {
        ListWebhooks {
            client: self.client,
            endpoint: GetWebhooks::builder().workspace(workspace).build(),
        }
    }

    /// Deletes a webhook. One that is already gone (`404`) counts as deleted.
    ///
    /// # Errors
    ///
    /// Any other [`Error`].
    pub async fn delete(&self, webhook: &WebhookGid) -> Result<(), Error> {
        match self
            .client
            .send(DeleteWebhook::builder(webhook.clone()).build())
            .await
        {
            Ok(_) => Ok(()),
            Err(error) if error.is_not_found() => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// Replaces a webhook's filters.
    ///
    /// The rule that holds for every webhook (`fields` only with `action: changed`) is checked
    /// first; the rules for workspace, team, portfolio and goal webhooks need the resource's kind,
    /// so check those with [`asana_webhooks::validate_filters`] when it applies.
    ///
    /// # Errors
    ///
    /// [`WebhookError::InvalidFilters`] before sending, or [`WebhookError::Api`] with what Asana
    /// answered.
    pub async fn update_filters(
        &self,
        webhook: &WebhookGid,
        filters: impl IntoIterator<Item = Filter>,
    ) -> Result<Webhook, WebhookError> {
        let filters: Vec<Filter> = filters.into_iter().collect();
        if filters.iter().any(|filter| {
            !filter.field_list().is_empty() && filter.action_filter() != Some(&EventAction::Changed)
        }) {
            return Err(WebhookError::InvalidFilters(FilterError::FieldsNeedChanged));
        }
        let data = WebhookUpdateRequest::builder()
            .filters(filters.iter().map(establish::filter_input).collect())
            .build();
        self.client
            .send(
                UpdateWebhook::builder(webhook.clone())
                    .data(data)
                    .opt_fields(health::full_record())
                    .build(),
            )
            .await
            .map_err(|source| WebhookError::Api(Box::new(source)))
    }
}

/// A listing of webhooks, from [`Webhooks::list`].
#[derive(Debug)]
#[must_use = "a listing does nothing until it is streamed"]
pub struct ListWebhooks<'c> {
    client: &'c Client,
    endpoint: GetWebhooks,
}

impl ListWebhooks<'_> {
    /// Only the webhooks on `resource`.
    pub fn resource(mut self, resource: impl Into<ResourceGid>) -> Self {
        self.endpoint.resource = Some(resource.into().gid_str().to_owned());
        self
    }

    /// The fields each webhook comes with; by default Asana's compact record (GID, `active`,
    /// resource and target).
    pub fn fields(mut self, fields: Fields<WebhookField>) -> Self {
        self.endpoint.opt_fields = fields;
        self
    }

    /// Every matching webhook, fetched a page at a time as the stream is read.
    pub fn stream(self) -> Items<Webhook> {
        self.client.stream(self.endpoint)
    }

    /// The matching webhooks a page at a time.
    pub fn pages(self) -> Pages<Webhook> {
        self.client.pages(self.endpoint)
    }
}
