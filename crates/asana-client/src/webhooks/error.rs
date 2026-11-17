//! What can go wrong establishing or changing a webhook, and what was left behind when it did.

use std::fmt;

use asana_openapi::ids::WebhookGid;
use asana_webhooks::{FilterError, RouteKey, StoreError, WebhookSecret};

use crate::Error;

/// Why a webhook could not be established or changed.
///
/// Every variant that can follow Asana creating the webhook carries a [`Cleanup`] saying whether
/// the webhook was deleted again. `Display` never contains a secret, a route key or a response
/// body.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum WebhookError {
    /// The target URL cannot be used; nothing was sent. The reason names the rule.
    #[error("the webhook target cannot be used: {0}")]
    InvalidTarget(&'static str),
    /// Asana would refuse these filters for this kind of resource; nothing was sent.
    #[error("the webhook's filters would be refused")]
    InvalidFilters(#[source] FilterError),
    /// The handshake store would not open the route; nothing was sent.
    #[error("the handshake store would not open the webhook's route")]
    Store(#[source] StoreError),
    /// Asana refused the target because it is this machine (`localhost` or a loopback address).
    /// Asana must reach the receiver from the internet: expose it at a public HTTPS address, for
    /// example through a tunnel while developing.
    #[error(
        "Asana refuses a webhook whose target is localhost or a loopback address; expose the receiver at a public HTTPS address"
    )]
    LocalTarget(#[source] Box<Error>),
    /// Asana refused the create, or it got no answer, after the receiver captured the handshake
    /// (`handshake_seen`) or without it.
    ///
    /// Asana creates a webhook only once the handshake was answered, so when the receiver saw
    /// none, nothing was created here. An unknown outcome after the receiver answered the
    /// handshake is [`OutcomeUnknown`](Self::OutcomeUnknown) instead.
    #[error("Asana did not create the webhook ({})", if *.handshake_seen {
        "after the receiver answered its handshake"
    } else {
        "and the receiver saw no handshake"
    })]
    Create {
        /// What Asana answered, or why no answer came.
        #[source]
        source: Box<Error>,
        /// Whether the receiver captured a handshake secret on the route.
        handshake_seen: bool,
    },
    /// The receiver answered the handshake, then Asana's answer to the create was lost (a
    /// timeout, a dropped connection or a `5xx`): Asana may have created the webhook, and it is
    /// not deleted, because nothing names it.
    ///
    /// The route has been closed and the secret the receiver captured is here. Find the webhook
    /// among the resource's ([`Webhooks::list`](super::Webhooks::list)) by its target (the base
    /// URL with `route` as the last path segment, or the exact URL given to `establish_at`), then
    /// either keep it, with this `secret` and `route`, or delete it. Establishing again without
    /// looking can leave two webhooks on the resource.
    #[error(
        "Asana may have created the webhook: the receiver answered its handshake, then the answer to the create was lost"
    )]
    OutcomeUnknown {
        /// Why no answer came.
        #[source]
        source: Box<Error>,
        /// The route the receiver captured the handshake on.
        route: RouteKey,
        /// The secret the receiver captured, which is the webhook's if Asana created it.
        secret: WebhookSecret,
    },
    /// The receiver never captured the handshake. Either Asana cannot reach the target, or the
    /// instance that received the handshake does not share the [`HandshakeStore`] with this one.
    ///
    /// [`HandshakeStore`]: asana_webhooks::HandshakeStore
    #[error(
        "the receiver captured no handshake: Asana cannot reach the target, or the instance that received it does not share the handshake store; {cleanup}"
    )]
    NotCaptured {
        /// Asana's refusal, when it refused the create (a `400` naming the failed handshake).
        refusal: Option<Box<Error>>,
        /// What became of the webhook.
        cleanup: Cleanup,
    },
    /// The receiver captured a different secret from the one Asana returned: something other
    /// than Asana answered on the route.
    #[error("the secret the receiver captured differs from the one Asana returned; {cleanup}")]
    Rejected {
        /// What became of the webhook.
        cleanup: Cleanup,
    },
    /// Asana created the webhook but its answer carried no `X-Hook-Secret` to confirm the
    /// handshake against.
    #[error("Asana's answer carried no webhook secret to confirm the handshake with; {cleanup}")]
    MissingSecret {
        /// What became of the webhook.
        cleanup: Cleanup,
    },
    /// Asana created the webhook, then the handshake store failed (or the route expired) when
    /// the captured secret was taken.
    #[error("the handshake store failed after Asana created the webhook; {cleanup}")]
    AfterCreate {
        /// The store's error.
        #[source]
        source: StoreError,
        /// What became of the webhook.
        cleanup: Cleanup,
    },
    /// Asana refused a change to an existing webhook.
    #[error("Asana refused the change to the webhook")]
    Api(#[source] Box<Error>),
}

impl WebhookError {
    /// What became of a webhook Asana created before the failure, when one may have been.
    #[must_use]
    pub fn cleanup(&self) -> Option<&Cleanup> {
        match self {
            Self::NotCaptured { cleanup, .. }
            | Self::Rejected { cleanup }
            | Self::MissingSecret { cleanup }
            | Self::AfterCreate { cleanup, .. } => Some(cleanup),
            _ => None,
        }
    }
}

impl From<FilterError> for WebhookError {
    fn from(error: FilterError) -> Self {
        Self::InvalidFilters(error)
    }
}

/// What became of a webhook Asana had created when establishing it failed afterwards.
#[derive(Debug)]
#[non_exhaustive]
pub enum Cleanup {
    /// Asana created no webhook, so nothing was left behind.
    NothingCreated,
    /// The webhook Asana created was deleted again.
    Deleted(WebhookGid),
    /// Deleting the webhook failed: it still exists. Delete it yourself.
    Failed {
        /// The webhook left behind.
        webhook: WebhookGid,
        /// Why deleting it failed.
        error: Box<Error>,
    },
    /// Asana's answer named no webhook, so it could not be deleted. List the resource's webhooks
    /// to find it.
    Unidentified,
}

impl Cleanup {
    /// Whether nothing was left behind in Asana.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        matches!(self, Self::NothingCreated | Self::Deleted(_))
    }

    /// The webhook left behind, when deleting it failed.
    #[must_use]
    pub fn remaining(&self) -> Option<&WebhookGid> {
        match self {
            Self::Failed { webhook, .. } => Some(webhook),
            _ => None,
        }
    }
}

impl fmt::Display for Cleanup {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NothingCreated => f.write_str("no webhook was created"),
            Self::Deleted(_) => f.write_str("the webhook Asana created was deleted again"),
            Self::Failed { webhook, .. } => write!(
                f,
                "deleting the webhook Asana created failed, so webhook {webhook} still exists"
            ),
            Self::Unidentified => f.write_str(
                "Asana's answer named no webhook to delete, so one may remain on the resource",
            ),
        }
    }
}
