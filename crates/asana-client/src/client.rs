//! The client: one token, one transport, and the limits and policies every call shares.
//!
//! A [`Client`] is an `Arc` inside, so cloning it is cheap and every clone shares the same
//! concurrency limits, search bucket, rate-limit cooldown and record of announced API changes.
//! Use one client per token: Asana counts its limits per token.

use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};

use http::HeaderValue;

use crate::auth::Auth;
use crate::limits::{Governor, Limits};
use crate::retry::RetryPolicy;
use crate::transport::{ReqwestTransport, Transport, TransportError};

/// Asana's API, where the client sends requests unless told otherwise.
pub const DEFAULT_BASE_URL: &str = "https://app.asana.com/api/1.0";

/// The `User-Agent` this crate sends, after any the caller names.
pub const DEFAULT_USER_AGENT: &str = concat!("asana-rs/", env!("CARGO_PKG_VERSION"));

/// Sends Asana operations. Build one with [`Client::builder`].
///
/// ```no_run
/// # async fn run() -> Result<(), Box<dyn std::error::Error>> {
/// use asana_client::{Auth, Client};
/// use asana_client::openapi::endpoints::GetTask;
///
/// let client = Client::builder()
///     .auth(Auth::personal_access_token("0/abc"))
///     .user_agent("acme-sync/1.0")
///     .build()?;
/// let task = client.send(GetTask::builder("1201234567890123".parse()?).build()).await?;
/// # Ok(()) }
/// ```
#[derive(Clone)]
pub struct Client {
    pub(crate) inner: Arc<Inner>,
}

/// What every clone of a client shares.
pub(crate) struct Inner {
    pub(crate) auth: Auth,
    pub(crate) transport: Arc<dyn Transport>,
    pub(crate) base_url: String,
    pub(crate) user_agent: HeaderValue,
    pub(crate) retry: RetryPolicy,
    pub(crate) limits: Limits,
    pub(crate) governor: Governor,
    pub(crate) enable: Option<HeaderValue>,
    pub(crate) disable: Option<HeaderValue>,
    pub(crate) changes: Mutex<Vec<AsanaChange>>,
}

#[bon::bon]
impl Client {
    /// Starts building a client. Only [`auth`](ClientBuilder::auth) is required.
    #[builder(builder_type(doc {
        /// Builds a [`Client`]. Only `auth` is required.
    }))]
    pub fn new(
        /// The credential to send.
        auth: Auth,
        /// What carries requests. Default: [`ReqwestTransport::with_defaults`].
        #[builder(with = |transport: impl Transport| Arc::new(transport) as Arc<dyn Transport>)]
        transport: Option<Arc<dyn Transport>>,
        /// Where Asana's API is, without a trailing `/`. Default [`DEFAULT_BASE_URL`]; set it to
        /// point the client at a fake or a test server.
        #[builder(into)]
        base_url: Option<String>,
        /// Names the calling application in the `User-Agent`, for example `acme-sync/1.4`. The
        /// crate's own name and version follow it.
        #[builder(into)]
        user_agent: Option<String>,
        /// When failed requests are sent again. Default [`RetryPolicy::default`].
        #[builder(default)]
        retry: RetryPolicy,
        /// The client's concurrency and search limits. Default [`Limits::default`].
        #[builder(default)]
        limits: Limits,
        /// API changes to opt into early, sent as `Asana-Enable`.
        #[builder(default, with = |names: impl IntoIterator<Item = impl Into<String>>| {
            names.into_iter().map(Into::into).collect()
        })]
        asana_enable: Vec<String>,
        /// API changes to opt out of for now, sent as `Asana-Disable`.
        #[builder(default, with = |names: impl IntoIterator<Item = impl Into<String>>| {
            names.into_iter().map(Into::into).collect()
        })]
        asana_disable: Vec<String>,
    ) -> Result<Self, BuildError> {
        let transport = match transport {
            Some(transport) => transport,
            None => Arc::new(ReqwestTransport::with_defaults().map_err(BuildError::Transport)?),
        };
        let base_url = validate_base_url(base_url.as_deref().unwrap_or(DEFAULT_BASE_URL))?;
        let user_agent = match user_agent {
            Some(name) => format!("{name} {DEFAULT_USER_AGENT}"),
            None => DEFAULT_USER_AGENT.to_owned(),
        };
        let user_agent =
            HeaderValue::try_from(user_agent).map_err(|_| BuildError::InvalidUserAgent)?;
        Ok(Self {
            inner: Arc::new(Inner {
                auth,
                transport,
                base_url,
                user_agent,
                retry,
                limits,
                governor: Governor::new(limits),
                enable: change_header(&asana_enable)?,
                disable: change_header(&asana_disable)?,
                changes: Mutex::new(Vec::new()),
            }),
        })
    }
}

impl Client {
    /// Where the client sends requests, without a trailing `/`.
    pub fn base_url(&self) -> &str {
        &self.inner.base_url
    }

    /// The transport the client sends through, for requests outside the API's base URL (OAuth's
    /// token endpoint) that should share its network configuration.
    pub fn transport(&self) -> Arc<dyn Transport> {
        Arc::clone(&self.inner.transport)
    }

    /// The `User-Agent` the client sends.
    pub fn user_agent(&self) -> &HeaderValue {
        &self.inner.user_agent
    }

    /// The client's limits.
    pub fn limits(&self) -> Limits {
        self.inner.limits
    }

    /// Every change Asana has announced on a response so far (the `Asana-Change` header), once
    /// per change name, in the order first seen.
    pub fn observed_changes(&self) -> Vec<AsanaChange> {
        self.inner
            .changes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Records the `Asana-Change` headers of a response, warning once per new change.
    pub(crate) fn record_changes(&self, headers: &http::HeaderMap) {
        let announced = headers
            .get_all("asana-change")
            .iter()
            .filter_map(|value| value.to_str().ok())
            .flat_map(|value| value.split(','))
            .filter_map(AsanaChange::parse);
        for change in announced {
            let mut seen = self
                .inner
                .changes
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if seen.iter().any(|known| known.name == change.name) {
                continue;
            }
            tracing::warn!(
                change = %change.name,
                info = change.info.as_deref().unwrap_or(""),
                affected = change.affected,
                "Asana announced a change to its API that affects this request; \
                 opt in or out with asana_enable / asana_disable",
            );
            seen.push(change);
        }
    }
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client")
            .field("base_url", &self.inner.base_url)
            .field("limits", &self.inner.limits)
            .field("retry", &self.inner.retry)
            .finish_non_exhaustive()
    }
}

/// A change to Asana's API announced in a response's `Asana-Change` header.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct AsanaChange {
    /// The change's name, as `Asana-Enable` and `Asana-Disable` take it.
    pub name: String,
    /// Where Asana describes the change.
    pub info: Option<String>,
    /// Whether this request would behave differently once the change is in force.
    pub affected: bool,
}

impl AsanaChange {
    /// Reads one `name=…;info=…;affected=true` entry.
    fn parse(entry: &str) -> Option<Self> {
        let mut name = None;
        let mut info = None;
        let mut affected = false;
        for pair in entry.split(';') {
            let Some((key, value)) = pair.split_once('=') else {
                continue;
            };
            match key.trim() {
                "name" => name = Some(value.trim().to_owned()),
                "info" => info = Some(value.trim().to_owned()),
                "affected" => affected = value.trim().eq_ignore_ascii_case("true"),
                _ => {}
            }
        }
        Some(Self {
            name: name.filter(|name| !name.is_empty())?,
            info,
            affected,
        })
    }
}

/// Why a client could not be built.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BuildError {
    /// The base URL is not an absolute `http` or `https` URL without a query or fragment.
    #[error("the base URL must be an absolute http(s) URL without a query or fragment")]
    InvalidBaseUrl,
    /// The user agent holds characters a header cannot carry.
    #[error("the user agent cannot be sent in an HTTP header")]
    InvalidUserAgent,
    /// An `Asana-Enable` or `Asana-Disable` name is empty or holds characters other than
    /// letters, digits, `_` and `-`.
    #[error("an Asana change name must be letters, digits, `_` or `-`")]
    InvalidChangeName,
    /// The default transport could not be built.
    #[error("the default transport could not be built")]
    Transport(#[source] TransportError),
}

fn validate_base_url(text: &str) -> Result<String, BuildError> {
    let url = url::Url::parse(text).map_err(|_| BuildError::InvalidBaseUrl)?;
    let valid = matches!(url.scheme(), "http" | "https")
        && url.host().is_some()
        && url.query().is_none()
        && url.fragment().is_none()
        && url.username().is_empty()
        && url.password().is_none();
    if !valid {
        return Err(BuildError::InvalidBaseUrl);
    }
    Ok(url.as_str().trim_end_matches('/').to_owned())
}

fn change_header(names: &[String]) -> Result<Option<HeaderValue>, BuildError> {
    if names.is_empty() {
        return Ok(None);
    }
    let valid = |name: &String| {
        !name.is_empty()
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    };
    if !names.iter().all(valid) {
        return Err(BuildError::InvalidChangeName);
    }
    HeaderValue::try_from(names.join(","))
        .map(Some)
        .map_err(|_| BuildError::InvalidChangeName)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_base_url_is_absolute_http_and_loses_its_trailing_slash() {
        assert_eq!(
            validate_base_url("https://app.asana.com/api/1.0/").unwrap(),
            "https://app.asana.com/api/1.0"
        );
        assert_eq!(
            validate_base_url("http://127.0.0.1:8080").unwrap(),
            "http://127.0.0.1:8080"
        );
        for bad in [
            "app.asana.com",
            "ftp://example.com",
            "https://example.com/api?x=1",
            "https://example.com/#top",
            "https://user:pass@example.com",
        ] {
            assert!(validate_base_url(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn asana_change_entries_are_read_by_key() {
        let change = AsanaChange::parse(
            "name=new_user_task_lists;info=https://example.com/changes;affected=true",
        )
        .unwrap();
        assert_eq!(change.name, "new_user_task_lists");
        assert_eq!(change.info.as_deref(), Some("https://example.com/changes"));
        assert!(change.affected);
        assert!(AsanaChange::parse("info=https://example.com").is_none());
    }

    #[test]
    fn change_names_are_checked_before_they_become_a_header() {
        assert!(change_header(&[]).unwrap().is_none());
        assert_eq!(
            change_header(&["a_b".into(), "c-d".into()])
                .unwrap()
                .unwrap(),
            "a_b,c-d"
        );
        assert!(change_header(&["bad name".into()]).is_err());
        assert!(change_header(&["x\r\ny".into()]).is_err());
    }
}
