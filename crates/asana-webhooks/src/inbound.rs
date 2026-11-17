//! Telling a handshake from a delivery, and refusing anything that is neither.
//!
//! Asana calls a webhook's target in exactly two ways. The handshake carries `X-Hook-Secret` and no
//! signature; every later call (event batches and heartbeats) carries `X-Hook-Signature` and no
//! secret. [`classify`] looks only at those two headers and the body's length, and refuses a
//! request with both, with neither, with a signature that is not 64 hex digits, or with a body
//! over the cap, before anything is parsed.

use crate::events::Limits;
use crate::headers::{HOOK_SECRET, HOOK_SIGNATURE, HeaderLookup};
use crate::signing::{Signature, WebhookSecret};

/// A request to a webhook target, classified by its headers.
#[derive(Debug)]
#[non_exhaustive]
pub enum Inbound<'a> {
    /// Asana is creating a webhook: answer with [`handshake_reply`](crate::handshake_reply) at once,
    /// after saving the secret where the creator will find it.
    Handshake {
        /// The new webhook's secret.
        secret: WebhookSecret,
    },
    /// A signed delivery (a batch of events, or an empty heartbeat): [`verify`](crate::verify) it
    /// with the route's stored secret before reading it.
    Delivery {
        /// The signature from `X-Hook-Signature`.
        signature: Signature,
        /// The raw body, unparsed.
        body: &'a [u8],
    },
}

/// Why a request is neither a handshake nor a delivery.
///
/// `Display` never repeats a header value or any of the body.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum InboundError {
    /// The request carried both `X-Hook-Secret` and `X-Hook-Signature`; Asana never sends both.
    #[error("the request carries both X-Hook-Secret and X-Hook-Signature")]
    BothHeaders,
    /// The request carried neither header: it is not from Asana.
    #[error("the request carries neither X-Hook-Secret nor X-Hook-Signature")]
    NeitherHeader,
    /// `X-Hook-Secret` was present but empty.
    #[error("the X-Hook-Secret header is empty")]
    EmptySecret,
    /// `X-Hook-Signature` was not 64 hexadecimal digits.
    #[error("the X-Hook-Signature header is not 64 hexadecimal digits")]
    MalformedSignature,
    /// The body was longer than the receiver accepts.
    #[error("the body is {len} bytes, over the limit of {limit}")]
    BodyTooLarge {
        /// The body's length in bytes.
        len: usize,
        /// The limit it broke ([`Limits::max_body_bytes`]).
        limit: usize,
    },
}

impl InboundError {
    /// The status a receiver should answer: `413` for an oversized body, `400` otherwise.
    #[must_use]
    pub fn status_hint(&self) -> u16 {
        match self {
            Self::BodyTooLarge { .. } => 413,
            Self::BothHeaders
            | Self::NeitherHeader
            | Self::EmptySecret
            | Self::MalformedSignature => 400,
        }
    }
}

/// Classifies a request with the default [`Limits`].
///
/// # Errors
///
/// See [`classify_with`].
pub fn classify<'a>(
    headers: &(impl HeaderLookup + ?Sized),
    body: &'a [u8],
) -> Result<Inbound<'a>, InboundError> {
    classify_with(headers, body, &Limits::default())
}

/// Classifies a request as a handshake or a delivery, refusing a body over `limits.max_body_bytes`.
///
/// A framework adapter should also stop reading the body at that size rather than buffer an
/// unbounded one only for this to refuse it.
///
/// # Errors
///
/// [`InboundError`] for both headers, neither, an empty secret, a malformed signature, or an
/// oversized body.
pub fn classify_with<'a>(
    headers: &(impl HeaderLookup + ?Sized),
    body: &'a [u8],
    limits: &Limits,
) -> Result<Inbound<'a>, InboundError> {
    if body.len() > limits.max_body_bytes {
        return Err(InboundError::BodyTooLarge {
            len: body.len(),
            limit: limits.max_body_bytes,
        });
    }
    match (headers.get(HOOK_SECRET), headers.get(HOOK_SIGNATURE)) {
        (Some(_), Some(_)) => Err(InboundError::BothHeaders),
        (None, None) => Err(InboundError::NeitherHeader),
        (Some(secret), None) => Ok(Inbound::Handshake {
            secret: WebhookSecret::from_header(secret)?,
        }),
        (None, Some(signature)) => Ok(Inbound::Delivery {
            signature: Signature::from_hex(signature)?,
            body,
        }),
    }
}
