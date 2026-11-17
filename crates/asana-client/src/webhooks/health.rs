//! How a webhook's deliveries are going, and when Asana will delete it for failing.

use std::fmt;

use asana_openapi::fields::{Fields, WebhookField};
use asana_openapi::ids::WebhookGid;
use asana_openapi::models::Webhook;
use time::OffsetDateTime;

/// The fields of a webhook's full record, health included.
pub(super) fn full_record() -> Fields<WebhookField> {
    Fields::from([
        WebhookField::Active,
        WebhookField::CreatedAt,
        WebhookField::Resource,
        WebhookField::ResourceName,
        WebhookField::Target,
        WebhookField::Filters,
        WebhookField::LastSuccessAt,
        WebhookField::LastFailureAt,
        WebhookField::LastFailureContent,
        WebhookField::DeliveryRetryCount,
        WebhookField::NextAttemptAfter,
        WebhookField::FailureDeletionTimestamp,
    ])
}

/// How a webhook's deliveries are going, from [`Webhooks::health`](super::Webhooks::health).
///
/// Asana deletes a webhook after 24 hours without a successful delivery (heartbeats count), and
/// says when in `failure_deletion_timestamp`. [`is_failing`](Self::is_failing) and
/// [`will_be_deleted_at`](Self::will_be_deleted_at) answer the two questions a scheduled check
/// asks.
///
/// The receiver's last error response is kept out of `Display` and `Debug` (it is whatever the
/// receiver answered, and may hold anything); read it with
/// [`last_failure_content`](Self::last_failure_content).
#[derive(Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct WebhookHealth {
    /// The webhook.
    pub webhook: Option<WebhookGid>,
    /// Whether the webhook sends events. An inactive one is not deleted, but delivers nothing.
    pub active: bool,
    /// When a delivery last succeeded.
    pub last_success_at: Option<OffsetDateTime>,
    /// When a delivery last failed.
    pub last_failure_at: Option<OffsetDateTime>,
    /// How many times the current delivery has been retried; back to 0 after a success.
    pub delivery_retry_count: u32,
    /// When Asana will next try to deliver.
    pub next_attempt_after: Option<OffsetDateTime>,
    /// When Asana will delete the webhook unless a delivery succeeds first.
    pub failure_deletion_timestamp: Option<OffsetDateTime>,
    last_failure_content: Option<String>,
}

impl WebhookHealth {
    /// Reads the health fields of a webhook record (one from a listing, say). Fields the record
    /// lacks read as "never" and zero.
    #[must_use]
    pub fn from_webhook(webhook: &Webhook) -> Self {
        let retries = webhook.delivery_retry_count.unwrap_or(0);
        Self {
            webhook: webhook.gid.clone(),
            active: webhook.active.unwrap_or(false),
            last_success_at: webhook.last_success_at,
            last_failure_at: webhook.last_failure_at,
            delivery_retry_count: u32::try_from(retries.max(0)).unwrap_or(u32::MAX),
            next_attempt_after: webhook.next_attempt_after,
            failure_deletion_timestamp: webhook.failure_deletion_timestamp,
            last_failure_content: webhook
                .last_failure_content
                .clone()
                .filter(|content| !content.is_empty()),
        }
    }

    /// What the receiver answered on the last failed delivery, as Asana recorded it. It is the
    /// receiver's own response, so treat it as untrusted text.
    #[must_use]
    pub fn last_failure_content(&self) -> Option<&str> {
        self.last_failure_content.as_deref()
    }

    /// Whether deliveries are failing now: a delivery is being retried, Asana has set a deletion
    /// time, or the last failure is more recent than the last success.
    #[must_use]
    pub fn is_failing(&self) -> bool {
        let failed_last = match (self.last_failure_at, self.last_success_at) {
            (Some(failure), Some(success)) => failure > success,
            (Some(_), None) => true,
            (None, _) => false,
        };
        self.delivery_retry_count > 0 || self.failure_deletion_timestamp.is_some() || failed_last
    }

    /// When Asana will delete the webhook if no delivery succeeds before then; `None` while it is
    /// healthy.
    #[must_use]
    pub fn will_be_deleted_at(&self) -> Option<OffsetDateTime> {
        self.failure_deletion_timestamp
    }
}

impl fmt::Debug for WebhookHealth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WebhookHealth")
            .field("webhook", &self.webhook)
            .field("active", &self.active)
            .field("last_success_at", &self.last_success_at)
            .field("last_failure_at", &self.last_failure_at)
            .field("delivery_retry_count", &self.delivery_retry_count)
            .field("next_attempt_after", &self.next_attempt_after)
            .field(
                "failure_deletion_timestamp",
                &self.failure_deletion_timestamp,
            )
            .field(
                "last_failure_content",
                &self
                    .last_failure_content
                    .as_ref()
                    .map(|content| content.len()),
            )
            .finish()
    }
}

impl fmt::Display for WebhookHealth {
    /// One line: active or not, and failing or healthy with the times that matter.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(if self.active { "active" } else { "inactive" })?;
        if !self.is_failing() {
            f.write_str(", healthy")?;
            if let Some(success) = self.last_success_at {
                write!(f, ", last delivered {success}")?;
            }
            return Ok(());
        }
        write!(f, ", failing ({} retries)", self.delivery_retry_count)?;
        if let Some(failure) = self.last_failure_at {
            write!(f, ", last failed {failure}")?;
        }
        if let Some(next) = self.next_attempt_after {
            write!(f, ", next attempt after {next}")?;
        }
        if let Some(deletion) = self.failure_deletion_timestamp {
            write!(f, ", to be deleted at {deletion}")?;
        }
        Ok(())
    }
}
