//! The seam between the client and the network.
//!
//! The client turns every operation into a plain [`http::Request`] with its whole body in memory
//! and hands it to a [`Transport`], which answers with a plain [`http::Response`] whose whole body
//! has been read. Nothing on either side sees a type from an HTTP library, so a transport can be
//! `reqwest` ([`ReqwestTransport`]), an in-memory fake for tests, or anything else that can carry
//! a request.

use std::error::Error as StdError;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use bytes::{Bytes, BytesMut};
use futures_util::future::BoxFuture;

/// An error a transport may wrap: anything that can be sent between threads.
pub type BoxError = Box<dyn StdError + Send + Sync + 'static>;

/// Carries one request to Asana and brings back the whole response.
///
/// Implementations answer every status code with `Ok`; an error is for when no response came
/// back at all. The client decides what a status means, retries and rate-limits; a transport only
/// carries bytes, and should not follow redirects.
pub trait Transport: Send + Sync + 'static {
    /// Sends `request` and reads the response body in full.
    fn send(
        &self,
        request: http::Request<Bytes>,
    ) -> BoxFuture<'_, Result<http::Response<Bytes>, TransportError>>;
}

impl<T: Transport + ?Sized> Transport for Arc<T> {
    fn send(
        &self,
        request: http::Request<Bytes>,
    ) -> BoxFuture<'_, Result<http::Response<Bytes>, TransportError>> {
        (**self).send(request)
    }
}

impl<T: Transport + ?Sized> Transport for Box<T> {
    fn send(
        &self,
        request: http::Request<Bytes>,
    ) -> BoxFuture<'_, Result<http::Response<Bytes>, TransportError>> {
        (**self).send(request)
    }
}

/// What went wrong carrying a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum TransportErrorKind {
    /// No connection was made (the name did not resolve, the connection was refused, the TLS
    /// handshake failed). The request never reached Asana.
    Connect,
    /// The request or the response took too long. Asana may have acted on it.
    Timeout,
    /// The response body was larger than the transport accepts. Asana acted on the request.
    BodyTooLarge {
        /// The largest body the transport accepts, in bytes.
        limit: usize,
    },
    /// Any other failure after the connection was made. Asana may have acted on the request.
    Other,
}

impl fmt::Display for TransportErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connect => f.write_str("could not connect to Asana"),
            Self::Timeout => f.write_str("the request to Asana timed out"),
            Self::BodyTooLarge { limit } => {
                write!(f, "Asana's response was larger than the {limit}-byte limit")
            }
            Self::Other => f.write_str("the request to Asana failed"),
        }
    }
}

/// A request that brought back no response.
///
/// Its `Display` names only the kind of failure. A source error is kept for diagnosis; the
/// transports in this crate strip the URL from it, because a URL can carry query values.
#[derive(Debug, thiserror::Error)]
#[error("{kind}")]
pub struct TransportError {
    kind: TransportErrorKind,
    #[source]
    source: Option<BoxError>,
}

impl TransportError {
    /// An error of `kind`, with the error that caused it if there is one.
    pub fn new(kind: TransportErrorKind, source: Option<BoxError>) -> Self {
        Self { kind, source }
    }

    /// No connection was made, so the request never reached Asana.
    pub fn connect(source: impl Into<BoxError>) -> Self {
        Self::new(TransportErrorKind::Connect, Some(source.into()))
    }

    /// The request or response took too long.
    pub fn timeout(source: impl Into<BoxError>) -> Self {
        Self::new(TransportErrorKind::Timeout, Some(source.into()))
    }

    /// The response body was larger than `limit` bytes.
    pub fn body_too_large(limit: usize) -> Self {
        Self::new(TransportErrorKind::BodyTooLarge { limit }, None)
    }

    /// Any other failure after the connection was made.
    pub fn other(source: impl Into<BoxError>) -> Self {
        Self::new(TransportErrorKind::Other, Some(source.into()))
    }

    /// What went wrong.
    pub fn kind(&self) -> TransportErrorKind {
        self.kind
    }

    /// Whether Asana may have received the request, and so may have acted on it.
    ///
    /// When this is `false`, sending the request again is safe for any method.
    pub fn may_have_reached_asana(&self) -> bool {
        !matches!(self.kind, TransportErrorKind::Connect)
    }

    /// Whether sending the same request again could succeed. A body that was too large will be
    /// too large again.
    pub fn is_transient(&self) -> bool {
        !matches!(self.kind, TransportErrorKind::BodyTooLarge { .. })
    }
}

/// How long one request may take in all, set by the client as an extension on the
/// `http::Request` it hands a [`Transport`] when an operation needs longer (or shorter) than the
/// transport's own default: creating a webhook stays open while Asana calls the receiver.
///
/// A transport honours it if it can; [`ReqwestTransport`] does. One that ignores it must itself
/// allow at least 60 seconds for webhook creation to work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RequestTimeout(pub std::time::Duration);

/// A [`Transport`] over a [`reqwest::Client`].
///
/// [`ReqwestTransport::new`] wraps a client the caller built, so its TLS, proxy and redirect
/// policy are kept (give it `redirect::Policy::none()`: Asana's API does not redirect, and a
/// redirect would carry the `Authorization` header to wherever it points).
/// [`ReqwestTransport::with_defaults`] builds one: rustls, no redirects, a 10-second connect
/// timeout and a 60-second total timeout.
#[derive(Clone, Debug)]
pub struct ReqwestTransport {
    client: reqwest::Client,
    max_body: usize,
}

impl ReqwestTransport {
    /// The largest response body read by default: 16 MiB.
    pub const DEFAULT_MAX_BODY: usize = 16 * 1024 * 1024;

    /// The connect timeout of [`ReqwestTransport::with_defaults`].
    pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

    /// The total timeout of [`ReqwestTransport::with_defaults`].
    pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

    /// Wraps a client the caller configured, reading bodies of up to
    /// [`DEFAULT_MAX_BODY`](Self::DEFAULT_MAX_BODY).
    pub fn new(client: reqwest::Client) -> Self {
        Self {
            client,
            max_body: Self::DEFAULT_MAX_BODY,
        }
    }

    /// Builds a client with rustls, no redirects, the default timeouts and a user agent naming
    /// this crate.
    ///
    /// # Errors
    ///
    /// [`TransportError`] when `reqwest` cannot build the client (the TLS backend failed to
    /// initialise).
    pub fn with_defaults() -> Result<Self, TransportError> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Self::DEFAULT_CONNECT_TIMEOUT)
            .timeout(Self::DEFAULT_TIMEOUT)
            .user_agent(crate::client::DEFAULT_USER_AGENT)
            .build()
            .map_err(|error| TransportError::other(error.without_url()))?;
        Ok(Self::new(client))
    }

    /// Sets the largest response body read, in bytes. A larger body fails with
    /// [`TransportErrorKind::BodyTooLarge`].
    #[must_use]
    pub fn max_body(mut self, bytes: usize) -> Self {
        self.max_body = bytes;
        self
    }

    async fn round_trip(
        &self,
        request: http::Request<Bytes>,
    ) -> Result<http::Response<Bytes>, TransportError> {
        let timeout = request.extensions().get::<RequestTimeout>().copied();
        let mut request = reqwest::Request::try_from(request).map_err(classify)?;
        if let Some(RequestTimeout(timeout)) = timeout {
            *request.timeout_mut() = Some(timeout);
        }
        let mut response = self.client.execute(request).await.map_err(classify)?;
        if response
            .content_length()
            .is_some_and(|length| length > self.max_body as u64)
        {
            return Err(TransportError::body_too_large(self.max_body));
        }
        let mut builder = http::Response::builder()
            .status(response.status())
            .version(response.version());
        if let Some(headers) = builder.headers_mut() {
            *headers = std::mem::take(response.headers_mut());
        }
        let mut body = BytesMut::new();
        while let Some(chunk) = response.chunk().await.map_err(classify)? {
            if body.len() + chunk.len() > self.max_body {
                return Err(TransportError::body_too_large(self.max_body));
            }
            body.extend_from_slice(&chunk);
        }
        builder.body(body.freeze()).map_err(TransportError::other)
    }
}

impl Transport for ReqwestTransport {
    fn send(
        &self,
        request: http::Request<Bytes>,
    ) -> BoxFuture<'_, Result<http::Response<Bytes>, TransportError>> {
        Box::pin(self.round_trip(request))
    }
}

/// Sorts a `reqwest` error by whether the request can have reached Asana, dropping its URL.
fn classify(error: reqwest::Error) -> TransportError {
    let kind = if error.is_connect() {
        TransportErrorKind::Connect
    } else if error.is_timeout() {
        TransportErrorKind::Timeout
    } else {
        TransportErrorKind::Other
    };
    TransportError::new(kind, Some(Box::new(error.without_url())))
}
