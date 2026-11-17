//! A registered OAuth app and the calls it makes to Asana's OAuth endpoints.
//!
//! These calls go to `https://app.asana.com/-/…`, outside the API's base URL, and authenticate with
//! the app's client secret rather than a bearer token, so they do not go through
//! [`Client`](crate::Client). They do go through a [`Transport`], so the caller's TLS, proxy and
//! redirect policy applies and tests need no network: pass [`Client::transport`] (or the same
//! transport the client was built with) to [`OAuthApp::builder`]. Each call is sent once; none is
//! retried, because an authorisation code works only once.
//!
//! [`Client::transport`]: crate::Client::transport

use std::fmt;
use std::sync::Arc;

use bytes::Bytes;
use http::header::{ACCEPT, CONTENT_TYPE, USER_AGENT};
use http::{HeaderValue, StatusCode, Uri};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use serde::de::DeserializeOwned;

use super::error::{OAuthBuildError, OAuthError, OAuthErrorCode};
use super::pkce::{PkceChallenge, PkceVerifier, State};
use super::scopes::ScopeSet;
use super::tokens::{
    AuthorizationCode, RawTokenInfo, RawTokenResponse, RefreshToken, TokenInfo, TokenResponse,
};
use crate::client::DEFAULT_USER_AGENT;
use crate::error::DecodeError;
use crate::transport::{ReqwestTransport, Transport};

/// Where Asana's OAuth endpoints are, unless the app is told otherwise.
pub const DEFAULT_ORIGIN: &str = "https://app.asana.com";
/// The out-of-band redirect URI Asana's guide names for native and command-line apps.
pub const OUT_OF_BAND_REDIRECT_URI: &str = "urn:ietf:wg:oauth:2.0:oob";

const AUTHORIZE_PATH: &str = "/-/oauth_authorize";
const TOKEN_PATH: &str = "/-/oauth_token";
const REVOKE_PATH: &str = "/-/oauth_revoke";
const INTROSPECT_PATH: &str = "/-/token_info";

/// An OAuth app registered in Asana's developer console. Cheap to clone.
///
/// ```no_run
/// # async fn run(code: asana_client::oauth::AuthorizationCode, verifier: asana_client::oauth::PkceVerifier)
/// # -> Result<(), Box<dyn std::error::Error>> {
/// use asana_client::oauth::{OAuthApp, PkcePair, ScopeSet, State};
/// use asana_client::openapi::endpoints::GetTask;
///
/// let app = OAuthApp::builder()
///     .client_id("1201234567890123")
///     .client_secret("made-up-secret")
///     .redirect_uri("https://acme.example.com/asana/callback")
///     .build()?;
///
/// // 1. Send the person to Asana; keep `state` and `pkce.verifier()` with their session.
/// let (state, pkce) = (State::generate(), PkcePair::generate());
/// let url = app.authorize_url(&asana_client::scopes_for![GetTask], &state, pkce.challenge());
///
/// // 2. On the redirect back, check it with `read_redirect`, then exchange the code.
/// let tokens = app.exchange(&code, &verifier).await?;
/// # Ok(()) }
/// ```
#[derive(Clone)]
pub struct OAuthApp {
    inner: Arc<Inner>,
}

struct Inner {
    client_id: String,
    client_secret: SecretString,
    redirect_uri: String,
    origin: String,
    token_uri: Uri,
    revoke_uri: Uri,
    introspect_uri: Uri,
    transport: Arc<dyn Transport>,
    user_agent: HeaderValue,
}

#[bon::bon]
impl OAuthApp {
    /// Starts building an app. `client_id`, `client_secret` and `redirect_uri` are required.
    #[builder(builder_type(doc {
        /// Builds an [`OAuthApp`]. `client_id`, `client_secret` and `redirect_uri` are required.
    }))]
    pub fn new(
        /// The app's client id, from the developer console.
        #[builder(into)]
        client_id: String,
        /// The app's client secret, from the developer console.
        #[builder(into)]
        client_secret: SecretString,
        /// The redirect URL registered for the app, exactly as registered: an `https` URL, an
        /// `http` URL on `localhost`, `127.0.0.1` or `[::1]`, or [`OUT_OF_BAND_REDIRECT_URI`].
        #[builder(into)]
        redirect_uri: String,
        /// What carries the token calls. Default: [`ReqwestTransport::with_defaults`].
        #[builder(with = |transport: impl Transport| Arc::new(transport) as Arc<dyn Transport>)]
        transport: Option<Arc<dyn Transport>>,
        /// Where Asana's OAuth endpoints are. Default [`DEFAULT_ORIGIN`]; set it to point the app
        /// at a fake.
        #[builder(into)]
        origin: Option<String>,
        /// Names the calling application in the `User-Agent`, as for the client.
        #[builder(into)]
        user_agent: Option<String>,
    ) -> Result<Self, OAuthBuildError> {
        if client_id.trim().is_empty() {
            return Err(OAuthBuildError::EmptyClientId);
        }
        validate_redirect_uri(&redirect_uri)?;
        let origin = validate_origin(origin.as_deref().unwrap_or(DEFAULT_ORIGIN))?;
        let endpoint = |path: &str| {
            Uri::try_from(format!("{origin}{path}")).map_err(|_| OAuthBuildError::InvalidOrigin)
        };
        let user_agent = match user_agent {
            Some(name) => format!("{name} {DEFAULT_USER_AGENT}"),
            None => DEFAULT_USER_AGENT.to_owned(),
        };
        let user_agent =
            HeaderValue::try_from(user_agent).map_err(|_| OAuthBuildError::InvalidUserAgent)?;
        let transport = match transport {
            Some(transport) => transport,
            None => {
                Arc::new(ReqwestTransport::with_defaults().map_err(OAuthBuildError::Transport)?)
            }
        };
        Ok(Self {
            inner: Arc::new(Inner {
                token_uri: endpoint(TOKEN_PATH)?,
                revoke_uri: endpoint(REVOKE_PATH)?,
                introspect_uri: endpoint(INTROSPECT_PATH)?,
                client_id,
                client_secret,
                redirect_uri,
                origin,
                transport,
                user_agent,
            }),
        })
    }
}

impl OAuthApp {
    /// The app's client id.
    pub fn client_id(&self) -> &str {
        &self.inner.client_id
    }

    /// The redirect URI, as given.
    pub fn redirect_uri(&self) -> &str {
        &self.inner.redirect_uri
    }

    /// Where Asana's OAuth endpoints are, without a trailing `/`.
    pub fn origin(&self) -> &str {
        &self.inner.origin
    }

    /// The URL of Asana's consent page to send a person to.
    ///
    /// It asks for an authorisation `code`, carries `state` and the PKCE `S256` challenge, and
    /// lists `scopes` space-separated. An empty `scopes` leaves the `scope` parameter out, which
    /// Asana's guide says means `default` (Full permissions) and works only for an app registered
    /// for Full permissions; an app registered with granular scopes must name them, and may name a
    /// subset. Asking again replaces the earlier grant's scopes rather than adding to them.
    pub fn authorize_url(&self, scopes: &ScopeSet, state: &State, pkce: &PkceChallenge) -> String {
        let mut url = format!("{}{AUTHORIZE_PATH}", self.inner.origin);
        let mut params = vec![
            ("client_id", self.inner.client_id.as_str()),
            ("redirect_uri", self.inner.redirect_uri.as_str()),
            ("response_type", "code"),
            ("state", state.as_str()),
            ("code_challenge_method", PkceChallenge::METHOD),
            ("code_challenge", pkce.as_str()),
        ];
        let scope = scopes.to_string();
        if !scope.is_empty() {
            params.push(("scope", &scope));
        }
        for (index, (name, value)) in params.into_iter().enumerate() {
            url.push(if index == 0 { '?' } else { '&' });
            url.push_str(name);
            url.push('=');
            percent_encode(value, &mut url);
        }
        url
    }

    /// Exchanges the code from Asana's redirect, with the PKCE verifier its challenge came from,
    /// for an access token, a refresh token and the user who authorised the app.
    ///
    /// # Errors
    ///
    /// [`OAuthError::InvalidGrant`] when the code is wrong, used or expired, or the verifier does
    /// not match; another [`OAuthError`] when Asana refuses or cannot be reached.
    pub async fn exchange(
        &self,
        code: &AuthorizationCode,
        verifier: &PkceVerifier,
    ) -> Result<TokenResponse, OAuthError> {
        let body = self.form(&[
            ("grant_type", "authorization_code"),
            ("redirect_uri", &self.inner.redirect_uri),
            ("code", code.secret().expose_secret()),
            ("code_verifier", verifier.secret().expose_secret()),
        ]);
        let raw: RawTokenResponse = self.post(&self.inner.token_uri, body, "exchange").await?;
        Ok(raw.into())
    }

    /// Exchanges a refresh token for a new access token.
    ///
    /// # Errors
    ///
    /// [`OAuthError::InvalidGrant`] when the refresh token is dead (revoked, or the person removed
    /// the app): the person must authorise the app again. Another [`OAuthError`] when Asana
    /// refuses or cannot be reached.
    pub async fn refresh(&self, refresh_token: &RefreshToken) -> Result<TokenResponse, OAuthError> {
        let body = self.form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token.secret().expose_secret()),
        ]);
        let raw: RawTokenResponse = self.post(&self.inner.token_uri, body, "refresh").await?;
        Ok(raw.into())
    }

    /// Revokes a refresh token and every access token issued from it.
    ///
    /// Asana's revocation endpoint takes only refresh tokens. It answers `200` both when the token
    /// was revoked and when it was not found, so this succeeds for a token already gone.
    ///
    /// # Errors
    ///
    /// An [`OAuthError`] when Asana refuses the request (`400`) or cannot be reached.
    pub async fn revoke(&self, refresh_token: &RefreshToken) -> Result<(), OAuthError> {
        let body = self.form(&[("token", refresh_token.secret().expose_secret())]);
        let request = self.request(&self.inner.revoke_uri, body);
        let response = self.send(request, "revoke").await?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(error_from(response.status(), response.body()))
        }
    }

    /// Asks Asana about a token: whether it is active, when it expires and which scopes it
    /// carries. Takes an OAuth access or refresh token, a personal access token or a service
    /// account's token.
    ///
    /// Asana's guide lists only `token` in the request; this sends nothing else.
    ///
    /// # Errors
    ///
    /// An [`OAuthError`] when Asana refuses or cannot be reached.
    pub async fn introspect(&self, token: &SecretString) -> Result<TokenInfo, OAuthError> {
        let body = form(&[("token", token.expose_secret())]);
        let raw: RawTokenInfo = self
            .post(&self.inner.introspect_uri, body, "introspect")
            .await?;
        Ok(raw.into())
    }

    /// A form body carrying the app's credentials and `pairs`.
    fn form(&self, pairs: &[(&str, &str)]) -> Bytes {
        let mut all = vec![
            ("client_id", self.inner.client_id.as_str()),
            ("client_secret", self.inner.client_secret.expose_secret()),
        ];
        all.extend_from_slice(pairs);
        form(&all)
    }

    fn request(&self, uri: &Uri, body: Bytes) -> http::Request<Bytes> {
        let mut request = http::Request::new(body);
        *request.method_mut() = http::Method::POST;
        *request.uri_mut() = uri.clone();
        let headers = request.headers_mut();
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/x-www-form-urlencoded"),
        );
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        headers.insert(USER_AGENT, self.inner.user_agent.clone());
        request
    }

    async fn send(
        &self,
        request: http::Request<Bytes>,
        call: &'static str,
    ) -> Result<http::Response<Bytes>, OAuthError> {
        let answer = self.inner.transport.send(request).await;
        match &answer {
            Ok(response) => {
                tracing::debug!(call, status = response.status().as_u16(), "asana.oauth")
            }
            Err(error) => tracing::debug!(call, kind = %error.kind(), "asana.oauth"),
        }
        answer.map_err(|source| OAuthError::Transport { source })
    }

    async fn post<T: DeserializeOwned>(
        &self,
        uri: &Uri,
        body: Bytes,
        call: &'static str,
    ) -> Result<T, OAuthError> {
        let response = self.send(self.request(uri, body), call).await?;
        let status = response.status();
        if !status.is_success() {
            return Err(error_from(status, response.body()));
        }
        serde_json::from_slice(response.body()).map_err(|error| OAuthError::Decode {
            source: DecodeError::from_json(&error),
        })
    }
}

impl fmt::Debug for OAuthApp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OAuthApp")
            .field("client_id", &self.inner.client_id)
            .field("client_secret", &"[REDACTED]")
            .field("redirect_uri", &self.inner.redirect_uri)
            .field("origin", &self.inner.origin)
            .finish_non_exhaustive()
    }
}

/// `application/x-www-form-urlencoded` pairs.
fn form(pairs: &[(&str, &str)]) -> Bytes {
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    for (name, value) in pairs {
        serializer.append_pair(name, value);
    }
    Bytes::from(serializer.finish())
}

/// An OAuth error body (RFC 6749 section 5.2) as the error it names, or the bare status.
fn error_from(status: StatusCode, body: &[u8]) -> OAuthError {
    #[derive(Deserialize)]
    struct ErrorBody {
        error: String,
        error_description: Option<String>,
    }
    match serde_json::from_slice::<ErrorBody>(body) {
        Ok(ErrorBody {
            error,
            error_description,
        }) => match OAuthErrorCode::parse(&error) {
            OAuthErrorCode::InvalidGrant => OAuthError::InvalidGrant {
                description: error_description,
            },
            code => OAuthError::Refused {
                status,
                code,
                description: error_description,
            },
        },
        Err(_) => OAuthError::Status { status },
    }
}

/// Appends `value` percent-encoded, leaving only RFC 3986's unreserved characters as they are, so a
/// space is `%20` as Asana's guide asks of the `scope` parameter.
fn percent_encode(value: &str, out: &mut String) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            out.push('%');
            out.push(char::from(HEX[usize::from(byte >> 4)]));
            out.push(char::from(HEX[usize::from(byte & 0x0F)]));
        }
    }
}

fn validate_redirect_uri(text: &str) -> Result<(), OAuthBuildError> {
    if text == OUT_OF_BAND_REDIRECT_URI {
        return Ok(());
    }
    let url = url::Url::parse(text).map_err(|_| OAuthBuildError::InvalidRedirectUri)?;
    let loopback = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    let valid = match url.scheme() {
        "https" => url.host().is_some(),
        "http" => loopback,
        _ => false,
    } && url.fragment().is_none()
        && url.username().is_empty()
        && url.password().is_none();
    if valid {
        Ok(())
    } else {
        Err(OAuthBuildError::InvalidRedirectUri)
    }
}

fn validate_origin(text: &str) -> Result<String, OAuthBuildError> {
    let url = url::Url::parse(text).map_err(|_| OAuthBuildError::InvalidOrigin)?;
    let valid = matches!(url.scheme(), "http" | "https")
        && url.host().is_some()
        && url.path() == "/"
        && url.query().is_none()
        && url.fragment().is_none()
        && url.username().is_empty()
        && url.password().is_none();
    if valid {
        Ok(url.as_str().trim_end_matches('/').to_owned())
    } else {
        Err(OAuthBuildError::InvalidOrigin)
    }
}
