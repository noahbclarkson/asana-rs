//! The axum side: the route, reading the raw body with a cap, and turning the [`Reply`] into a
//! response. Every protocol rule is in [`Receiver::receive`].

use asana_webhooks::{FnHeaders, HandshakeStore, Receiver, Reply, SecretLookup, Sink};
use axum::Router;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::header::{CONTENT_LENGTH, HeaderName, HeaderValue};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use futures_util::StreamExt as _;

/// A router answering `POST {path}/{route_key}` with [`handle`], with `receiver` as its state.
///
/// `path` is the base the webhooks' targets were made from, without a trailing slash (for example
/// `/webhooks/asana` for targets `https://example.com/webhooks/asana/{route_key}`). The result fits
/// any application's router, whatever its state: `.merge` it, or `.nest` it with an empty `path`.
pub fn router<H, L, S, T>(receiver: &Receiver<H, L, S>, path: &str) -> Router<T>
where
    H: HandshakeStore + 'static,
    L: SecretLookup + 'static,
    S: Sink + 'static,
    T: Clone + Send + Sync + 'static,
{
    Router::new()
        .route(&format!("{path}/{{route_key}}"), post(handle::<H, L, S>))
        .with_state(receiver.clone())
}

/// The axum handler for a webhook target: mount it on a path whose last segment is
/// `{route_key}`, with a [`Receiver`] as state. [`router`] does both.
///
/// It refuses a body longer than the receiver's
/// [`max_body_bytes`](asana_webhooks::Limits::max_body_bytes) with `413`, from its declared
/// `Content-Length` or as soon as the bytes read pass the limit, and otherwise hands the route
/// segment, the headers and the exact bytes to [`Receiver::receive`], which decides every other
/// answer (see [`asana_webhooks::Receiver`]). A handshake's echoed `X-Hook-Secret` is marked
/// sensitive, so it stays out of any debug output of the response.
pub async fn handle<H, L, S>(
    State(receiver): State<Receiver<H, L, S>>,
    Path(route_key): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> Response
where
    H: HandshakeStore + 'static,
    L: SecretLookup + 'static,
    S: Sink + 'static,
{
    let cap = receiver.current_limits().max_body_bytes;
    if declared_length(&headers).is_some_and(|length| length > cap) {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    }
    let bytes = match read_capped(body, cap).await {
        Ok(bytes) => bytes,
        Err(status) => return status.into_response(),
    };
    let lookup =
        FnHeaders::new(|name: &str| headers.get(name).and_then(|value| value.to_str().ok()));
    into_response(&receiver.receive(&route_key, &lookup, &bytes).await)
}

/// The `Content-Length` the request declares, when it declares a readable one.
fn declared_length(headers: &HeaderMap) -> Option<usize> {
    headers
        .get(CONTENT_LENGTH)?
        .to_str()
        .ok()?
        .parse::<u64>()
        .ok()
        .map(|length| usize::try_from(length).unwrap_or(usize::MAX))
}

/// Reads the whole body, stopping with `413` as soon as it passes `cap` bytes, or `400` when it
/// cannot be read. Never through a JSON extractor: the signature is over these exact bytes.
async fn read_capped(body: Body, cap: usize) -> Result<Vec<u8>, StatusCode> {
    let mut stream = body.into_data_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| StatusCode::BAD_REQUEST)?;
        if bytes.len().saturating_add(chunk.len()) > cap {
            return Err(StatusCode::PAYLOAD_TOO_LARGE);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn into_response(reply: &Reply) -> Response {
    let status = StatusCode::from_u16(reply.status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let Some(echo) = reply.echo() else {
        return status.into_response();
    };
    let name = HeaderName::from_bytes(echo.header_name().as_bytes());
    let value = HeaderValue::from_bytes(echo.header_value());
    match (name, value) {
        (Ok(name), Ok(mut value)) => {
            value.set_sensitive(true);
            (status, [(name, value)]).into_response()
        }
        // Asana's secret arrived as a header value, so it always is one; never answer a
        // handshake without its echo.
        _ => StatusCode::BAD_REQUEST.into_response(),
    }
}
