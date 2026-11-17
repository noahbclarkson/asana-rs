//! Reading back what the scripted transport was sent.

use bytes::Bytes;

/// The request's query as decoded name and value pairs, in order.
pub(crate) fn query(request: &http::Request<Bytes>) -> Vec<(String, String)> {
    url::form_urlencoded::parse(request.uri().query().unwrap_or("").as_bytes())
        .map(|(name, value)| (name.into_owned(), value.into_owned()))
        .collect()
}

/// The value of one query parameter.
pub(crate) fn param(request: &http::Request<Bytes>, name: &str) -> Option<String> {
    query(request)
        .into_iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value)
}

/// The request's path, without the base URL's `/api/1.0`.
pub(crate) fn path(request: &http::Request<Bytes>) -> String {
    request
        .uri()
        .path()
        .trim_start_matches("/api/1.0")
        .to_owned()
}

/// The request's JSON body.
pub(crate) fn body(request: &http::Request<Bytes>) -> serde_json::Value {
    serde_json::from_slice(request.body()).unwrap()
}
