//! What the fake was asked, kept so a test can assert a call was or was not made.

use asana_openapi::endpoint::Endpoint;
use serde_json::Value;

/// One request as it crossed the transport.
///
/// A batch is recorded once, as the `createBatchRequest` it was; its actions are in its `body`.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct RecordedRequest {
    /// The HTTP method.
    pub method: http::Method,
    /// The path below `/api/1.0`, for example `/tasks/1200000000000001`.
    pub path: String,
    /// The query pairs, decoded, in the order sent.
    pub query: Vec<(String, String)>,
    /// The JSON body, when the request had one (an upload's form is not kept here).
    pub body: Option<Value>,
    /// The operation's `operationId` in Asana's specification, when the path names one
    /// (served or not), for example `addTaskForSection`.
    pub operation: Option<&'static str>,
}

impl RecordedRequest {
    /// Whether this was a call of the operation `E` describes.
    pub fn is<E: Endpoint>(&self) -> bool {
        self.operation == Some(E::OPERATION_ID)
    }

    /// A query parameter's value.
    pub fn query(&self, name: &str) -> Option<&str> {
        self.query
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    /// The body's `data` member.
    pub fn data(&self) -> Option<&Value> {
        self.body.as_ref()?.get("data")
    }
}
