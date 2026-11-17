//! Establishing a webhook: the route, the create request, and the handshake settled around it.

use std::fmt;
use std::net::IpAddr;
use std::time::Duration;

use asana_openapi::endpoints::{CreateWebhook, CreatedWebhook};
use asana_openapi::ids::WebhookGid;
use asana_openapi::models::{Webhook, WebhookFilterInput, WebhookRequest};
use asana_webhooks::{
    Filter, HandshakeOutcome, HandshakeStore, ResourceGid, RouteKey, StoreError, WebhookSecret,
    settle, validate_filters,
};
use http::StatusCode;
use secrecy::ExposeSecret;

use super::error::{Cleanup, WebhookError};
use super::health;
use crate::{Client, Error};

/// How long a route stays open for its handshake by default: comfortably longer than the create
/// request Asana holds open while it calls the receiver.
const DEFAULT_HANDSHAKE_TTL: Duration = Duration::from_secs(120);

/// How long the create request may take by default. Asana holds it open while it calls the
/// receiver, so it is longer than an ordinary request's timeout.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(90);

/// Where the webhook delivers.
pub(super) enum Target {
    /// A base URL; the route key is appended as the last path segment.
    Base(String),
    /// A full URL the caller formed, containing the route key.
    Exact { url: String, route: RouteKey },
}

impl Target {
    /// Checks the target and returns the URL to send with its route.
    fn resolve(self) -> Result<(url::Url, RouteKey), WebhookError> {
        match self {
            Self::Base(base) => {
                let parsed = checked(&base)?;
                if parsed.query().is_some() {
                    return Err(WebhookError::InvalidTarget(
                        "a base URL cannot carry a query (use `establish_at` for a target with one)",
                    ));
                }
                let route = RouteKey::generate();
                let joined = format!("{}/{route}", parsed.as_str().trim_end_matches('/'));
                let url = url::Url::parse(&joined)
                    .map_err(|_| WebhookError::InvalidTarget("it is not an absolute URL"))?;
                Ok((url, route))
            }
            Self::Exact { url, route } => {
                let parsed = checked(&url)?;
                if !parsed.as_str().contains(&route.to_string()) {
                    return Err(WebhookError::InvalidTarget(
                        "it does not contain the route key, so the receiver could not find the handshake",
                    ));
                }
                Ok((parsed, route))
            }
        }
    }
}

/// The rules every target follows: absolute `https`, a host, no credentials, no fragment.
fn checked(text: &str) -> Result<url::Url, WebhookError> {
    let url = url::Url::parse(text)
        .map_err(|_| WebhookError::InvalidTarget("it is not an absolute URL"))?;
    if url.scheme() != "https" {
        return Err(WebhookError::InvalidTarget(
            "Asana delivers only to https targets",
        ));
    }
    if url.host().is_none() {
        return Err(WebhookError::InvalidTarget("it names no host"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(WebhookError::InvalidTarget("it cannot carry credentials"));
    }
    if url.fragment().is_some() {
        return Err(WebhookError::InvalidTarget("it cannot carry a fragment"));
    }
    Ok(url)
}

/// Whether Asana will see the target as this machine, which it refuses with `403`.
fn is_local(url: &url::Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(domain)) => {
            let domain = domain.trim_end_matches('.').to_ascii_lowercase();
            domain == "localhost" || domain.ends_with(".localhost")
        }
        Some(url::Host::Ipv4(address)) => is_local_address(IpAddr::V4(address)),
        Some(url::Host::Ipv6(address)) => is_local_address(IpAddr::V6(address)),
        None => false,
    }
}

fn is_local_address(address: IpAddr) -> bool {
    address.is_loopback() || address.is_unspecified()
}

/// A filter as the generated request model holds it. Serialises to the same JSON as the
/// [`Filter`] itself: `{resource_type, resource_subtype, action, fields}`, absent keys omitted.
pub(super) fn filter_input(filter: &Filter) -> WebhookFilterInput {
    let fields = filter.field_list();
    WebhookFilterInput::builder()
        .maybe_resource_type(filter.resource_type().map(|kind| kind.as_str().to_owned()))
        .maybe_resource_subtype(filter.resource_subtype().map(str::to_owned))
        .maybe_action(
            filter
                .action_filter()
                .map(|action| action.as_str().to_owned()),
        )
        .maybe_fields((!fields.is_empty()).then(|| fields.to_vec()))
        .build()
}

/// A webhook being established, from [`Webhooks::establish`](super::Webhooks::establish).
///
/// `S` is `()` until a store is given with [`store`](Self::store); only then can it be sent.
#[must_use = "establishing a webhook does nothing until it is sent"]
pub struct Establish<'c, S> {
    client: &'c Client,
    resource: ResourceGid,
    target: Target,
    filters: Vec<Filter>,
    handshake_ttl: Duration,
    timeout: Duration,
    store: S,
}

impl<S> fmt::Debug for Establish<'_, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Establish")
            .field("resource", &self.resource)
            .field("filters", &self.filters)
            .field("handshake_ttl", &self.handshake_ttl)
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

impl<'c> Establish<'c, ()> {
    pub(super) fn new(client: &'c Client, resource: ResourceGid, target: Target) -> Self {
        Self {
            client,
            resource,
            target,
            filters: Vec::new(),
            handshake_ttl: DEFAULT_HANDSHAKE_TTL,
            timeout: DEFAULT_TIMEOUT,
            store: (),
        }
    }
}

impl<'c, S> Establish<'c, S> {
    /// Adds a filter: the webhook delivers an event that passes any of them. Without filters a
    /// webhook on a project, section or task delivers everything below it; one on a workspace,
    /// team, portfolio or goal must have at least one.
    pub fn filter(mut self, filter: Filter) -> Self {
        self.filters.push(filter);
        self
    }

    /// Adds several filters.
    pub fn filters(mut self, filters: impl IntoIterator<Item = Filter>) -> Self {
        self.filters.extend(filters);
        self
    }

    /// How long the route stays open for the handshake; two minutes by default. It must outlast
    /// the create request's [`timeout`](Self::timeout).
    pub fn handshake_ttl(mut self, ttl: Duration) -> Self {
        self.handshake_ttl = ttl;
        self
    }

    /// How long the create request may take, Asana's call to the receiver included; 90 seconds
    /// by default. Keep it shorter than the [`handshake_ttl`](Self::handshake_ttl).
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// The store the receiver captures the handshake into. It must be the one every instance
    /// that can receive the handshake writes to.
    pub fn store<T: HandshakeStore>(self, store: &T) -> Establish<'c, &T> {
        Establish {
            client: self.client,
            resource: self.resource,
            target: self.target,
            filters: self.filters,
            handshake_ttl: self.handshake_ttl,
            timeout: self.timeout,
            store,
        }
    }
}

impl<T: HandshakeStore> Establish<'_, &T> {
    /// Creates the webhook and settles its handshake.
    ///
    /// In order: checks the filters and the target, opens the route in the store, sends
    /// `POST /webhooks` (during which Asana calls the receiver, which captures the secret), takes
    /// the captured secret from the store, and compares it with the one in Asana's answer in
    /// constant time. Whenever that fails after Asana created the webhook, the webhook is deleted
    /// again and the error's [`Cleanup`] says whether that worked.
    ///
    /// # Errors
    ///
    /// [`WebhookError`]: [`InvalidFilters`](WebhookError::InvalidFilters),
    /// [`InvalidTarget`](WebhookError::InvalidTarget) or [`Store`](WebhookError::Store) before
    /// anything is sent; [`LocalTarget`](WebhookError::LocalTarget),
    /// [`NotCaptured`](WebhookError::NotCaptured) or [`Create`](WebhookError::Create) when Asana
    /// refuses; [`Rejected`](WebhookError::Rejected),
    /// [`MissingSecret`](WebhookError::MissingSecret) or
    /// [`AfterCreate`](WebhookError::AfterCreate) after it created the webhook.
    pub async fn send(self) -> Result<Established, WebhookError> {
        validate_filters(&self.resource.kind(), &self.filters)?;
        let (target, route) = self.target.resolve()?;
        let local = is_local(&target);
        self.store
            .expect(&route, self.handshake_ttl)
            .await
            .map_err(WebhookError::Store)?;

        let filters = (!self.filters.is_empty())
            .then(|| self.filters.iter().map(filter_input).collect::<Vec<_>>());
        let request = CreateWebhook::builder()
            .data(
                WebhookRequest::builder()
                    .resource(self.resource.gid_str())
                    .target(target.as_str())
                    .maybe_filters(filters)
                    .build(),
            )
            .opt_fields(health::full_record())
            .build();
        let created = self.client.send_with_timeout(request, self.timeout).await;
        // Taking closes the route whatever happened, so a late handshake cannot land on it.
        let captured = self.store.take(&route).await;

        match created {
            Err(source) => Err(refused(source, captured.as_ref(), local)),
            Ok(created) => confirm(self.client, created, captured, route).await,
        }
    }
}

/// The error for a create Asana refused (or that got no answer).
fn refused(
    source: Error,
    captured: Result<&Option<WebhookSecret>, &StoreError>,
    local: bool,
) -> WebhookError {
    let handshake_seen = matches!(captured, Ok(Some(_)));
    match source.status() {
        Some(StatusCode::FORBIDDEN) if local => WebhookError::LocalTarget(Box::new(source)),
        Some(StatusCode::BAD_REQUEST) if !handshake_seen => WebhookError::NotCaptured {
            refusal: Some(Box::new(source)),
            cleanup: Cleanup::NothingCreated,
        },
        _ => WebhookError::Create {
            source: Box::new(source),
            handshake_seen,
        },
    }
}

/// Settles the handshake for a webhook Asana created, deleting it on any failure.
async fn confirm(
    client: &Client,
    created: CreatedWebhook,
    captured: Result<Option<WebhookSecret>, StoreError>,
    route: RouteKey,
) -> Result<Established, WebhookError> {
    let webhook_gid = created.data.gid.clone();
    let captured = match captured {
        Ok(captured) => captured,
        Err(source) => {
            let cleanup = clean_up(client, webhook_gid).await;
            return Err(WebhookError::AfterCreate { source, cleanup });
        }
    };
    let Some(returned) = created
        .hook_secret
        .as_ref()
        .map(|secret| WebhookSecret::from_bytes(secret.expose_secret().as_bytes().to_vec()))
    else {
        let cleanup = clean_up(client, webhook_gid).await;
        return Err(WebhookError::MissingSecret { cleanup });
    };
    match settle(captured, &returned) {
        HandshakeOutcome::Established(secret) => Ok(Established {
            webhook: created.data,
            secret,
            route,
        }),
        HandshakeOutcome::NotCaptured => {
            let cleanup = clean_up(client, webhook_gid).await;
            Err(WebhookError::NotCaptured {
                refusal: None,
                cleanup,
            })
        }
        HandshakeOutcome::Rejected => {
            let cleanup = clean_up(client, webhook_gid).await;
            Err(WebhookError::Rejected { cleanup })
        }
    }
}

/// Deletes a webhook that was created but not established.
async fn clean_up(client: &Client, webhook: Option<WebhookGid>) -> Cleanup {
    let Some(webhook) = webhook else {
        tracing::warn!(
            "a webhook was created but not established, and Asana named no GID to delete"
        );
        return Cleanup::Unidentified;
    };
    match client.webhooks().delete(&webhook).await {
        Ok(()) => Cleanup::Deleted(webhook),
        Err(error) => {
            tracing::warn!(%webhook, "a webhook was created but not established, and deleting it failed");
            Cleanup::Failed {
                webhook,
                error: Box::new(error),
            }
        }
    }
}

/// A webhook whose handshake succeeded.
///
/// Keep all three: the webhook's GID to manage it, the secret (encrypted at rest) to verify every
/// delivery, and the route the receiver finds that secret by. Asana never returns the secret
/// again.
#[derive(Debug)]
#[non_exhaustive]
pub struct Established {
    /// The webhook as Asana created it.
    pub webhook: Webhook,
    /// The secret that signs its deliveries.
    pub secret: WebhookSecret,
    /// The route key in its target.
    pub route: RouteKey,
}
