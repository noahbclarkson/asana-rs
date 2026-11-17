//! The actix-web side: the resource, reading the raw body under a cap of its own, and turning the
//! [`Reply`] into a response. Every status rule is [`Receiver::receive`]'s.

use actix_web::error::PayloadError;
use actix_web::http::StatusCode;
use actix_web::http::header::{HeaderName, HeaderValue};
use actix_web::web::{self, PayloadConfig};
use actix_web::{HttpRequest, HttpResponse, Resource};
use asana_webhooks::{FnHeaders, HandshakeStore, Receiver, Reply, SecretLookup, Sink};

/// The name of the path parameter that holds the route key.
const ROUTE_KEY: &str = "route_key";

/// An actix [`Resource`] answering `POST {path}/{route_key}` with [`handle`], carrying `receiver`
/// and a body cap of its own.
///
/// `path` is the base the webhooks' targets were made from (for example `/webhooks/asana` for
/// targets `https://example.com/webhooks/asana/{route_key}`); a trailing slash is ignored. The
/// receiver (as [`web::Data`]) and a [`PayloadConfig`] of
/// [`Limits::max_body_bytes`](asana_webhooks::Limits) are set on the resource itself, so they reach
/// only this handler, and actix finds them before any app-wide config. Other methods on the path
/// are answered `405` by actix.
pub fn resource<H, L, S>(receiver: &Receiver<H, L, S>, path: &str) -> Resource
where
    H: HandshakeStore + 'static,
    L: SecretLookup + 'static,
    S: Sink + 'static,
{
    let pattern = format!("{}/{{{ROUTE_KEY}}}", path.trim_end_matches('/'));
    web::resource(pattern)
        .app_data(PayloadConfig::new(receiver.current_limits().max_body_bytes))
        .app_data(web::Data::new(receiver.clone()))
        .route(web::post().to(handle::<H, L, S>))
}

/// The same [`resource`], as a function for [`App::configure`](actix_web::App::configure) or
/// [`Scope::configure`](actix_web::Scope::configure).
pub fn configure<H, L, S>(
    receiver: &Receiver<H, L, S>,
    path: &str,
) -> impl FnOnce(&mut web::ServiceConfig) + use<H, L, S>
where
    H: HandshakeStore + 'static,
    L: SecretLookup + 'static,
    S: Sink + 'static,
{
    let resource = resource(receiver, path);
    move |config| {
        config.service(resource);
    }
}

/// The actix handler for a webhook target: mount it on a path whose last segment is
/// `{route_key}`, with the [`Receiver`] as [`web::Data`] and a [`PayloadConfig`] beside it.
/// [`resource`] does all three.
///
/// A body longer than the [`PayloadConfig`]'s limit is answered `413`, read no further than the
/// limit, and a body that could not be read `400`; everything else is
/// [`Receiver::receive`]'s answer, inside its `asana_webhook` tracing span.
pub async fn handle<H, L, S>(
    request: HttpRequest,
    body: Result<web::Bytes, actix_web::Error>,
    receiver: web::Data<Receiver<H, L, S>>,
) -> HttpResponse
where
    H: HandshakeStore + 'static,
    L: SecretLookup + 'static,
    S: Sink + 'static,
{
    let body = match body {
        Ok(body) => body,
        Err(error) => return HttpResponse::new(unreadable(&error)),
    };
    let route_key = request.match_info().get(ROUTE_KEY).unwrap_or_default();
    let headers = request.headers();
    // actix's header map matches names case-insensitively, as `HeaderLookup` requires.
    let lookup =
        FnHeaders::new(|name: &str| headers.get(name).and_then(|value| value.to_str().ok()));
    let reply = receiver.receive(route_key, &lookup, &body).await;
    into_response(&reply)
}

/// The status for a body the extractor could not read: `413` over the cap, `400` otherwise.
fn unreadable(error: &actix_web::Error) -> StatusCode {
    match error.as_error::<PayloadError>() {
        Some(PayloadError::Overflow) => StatusCode::PAYLOAD_TOO_LARGE,
        _ => StatusCode::BAD_REQUEST,
    }
}

fn into_response(reply: &Reply) -> HttpResponse {
    let status = StatusCode::from_u16(reply.status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let Some(echo) = reply.echo() else {
        return HttpResponse::new(status);
    };
    let name = HeaderName::from_static("x-hook-secret");
    match HeaderValue::from_bytes(echo.header_value()) {
        Ok(mut value) => {
            // Keeps the secret out of any debug output of the response's headers.
            value.set_sensitive(true);
            HttpResponse::build(status)
                .insert_header((name, value))
                .finish()
        }
        // Asana's secret arrived as a header value, so it is always a valid one; refuse rather
        // than answer a handshake without its echo.
        Err(_) => HttpResponse::new(StatusCode::BAD_REQUEST),
    }
}
