//! A scripted transport, a client built on it, and small descriptors for the shapes the sample
//! endpoints cannot express (a creating POST, an idempotent POST, a whole-body answer, an upload).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use asana_client::openapi::endpoint::{
    Endpoint, Envelope, Method, Part, PartValue, QueryPair, RequestBody,
};
use asana_client::openapi::scopes::Scope;
use asana_client::{Auth, Client, Limits, RetryPolicy, Transport, TransportError};
use bytes::Bytes;
use futures_util::future::BoxFuture;
use serde::Deserialize;
use tokio::time::Instant;

pub(crate) const BASE: &str = "https://asana.example.com/api/1.0";
pub(crate) const TOKEN: &str = "0/acme-test-token-not-real";
pub(crate) const TASK: &str = "1201234567890123";

/// One request the transport saw, and when.
pub(crate) struct Seen {
    pub(crate) at: Instant,
    pub(crate) request: http::Request<Bytes>,
}

type Answer = Result<http::Response<Bytes>, TransportError>;
type Responder = dyn Fn(&http::Request<Bytes>, usize) -> Answer + Send + Sync;

/// Answers each request with a closure of the request and its index, and records it.
pub(crate) struct Scripted {
    responder: Box<Responder>,
    seen: Mutex<Vec<Seen>>,
}

impl Scripted {
    pub(crate) fn new(
        responder: impl Fn(&http::Request<Bytes>, usize) -> Answer + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            responder: Box::new(responder),
            seen: Mutex::new(Vec::new()),
        })
    }

    /// Answers with each reply in turn, then with the last one forever.
    pub(crate) fn replies(replies: Vec<Answer>) -> Arc<Self> {
        let replies = Mutex::new(VecDeque::from(replies));
        let last = Mutex::new(None::<http::Response<Bytes>>);
        Self::new(move |_, _| {
            let next = replies.lock().unwrap().pop_front();
            match next {
                Some(Ok(response)) => {
                    *last.lock().unwrap() = Some(response.clone());
                    Ok(response)
                }
                Some(Err(error)) => Err(error),
                None => Ok(last.lock().unwrap().clone().expect("a scripted reply")),
            }
        })
    }

    pub(crate) fn count(&self) -> usize {
        self.seen.lock().unwrap().len()
    }

    pub(crate) fn times(&self) -> Vec<Instant> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .map(|seen| seen.at)
            .collect()
    }

    pub(crate) fn request(&self, index: usize) -> http::Request<Bytes> {
        self.seen.lock().unwrap()[index].request.clone()
    }
}

impl Transport for Scripted {
    fn send(
        &self,
        request: http::Request<Bytes>,
    ) -> BoxFuture<'_, Result<http::Response<Bytes>, TransportError>> {
        let index = {
            let mut seen = self.seen.lock().unwrap();
            seen.push(Seen {
                at: Instant::now(),
                request: request.clone(),
            });
            seen.len() - 1
        };
        let answer = (self.responder)(&request, index);
        Box::pin(async move { answer })
    }
}

pub(crate) fn reply(status: u16, body: &str) -> http::Response<Bytes> {
    http::Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Bytes::copy_from_slice(body.as_bytes()))
        .unwrap()
}

pub(crate) fn reply_with(status: u16, header: (&str, &str), body: &str) -> http::Response<Bytes> {
    let mut response = reply(status, body);
    response.headers_mut().insert(
        http::HeaderName::from_bytes(header.0.as_bytes()).unwrap(),
        http::HeaderValue::from_str(header.1).unwrap(),
    );
    response
}

pub(crate) fn task_body() -> &'static str {
    r#"{"data": {"gid": "1201234567890123", "resource_type": "task", "name": "Acme launch"}}"#
}

/// A client on `transport` with a personal access token and the default policies.
pub(crate) fn client(transport: Arc<Scripted>) -> Client {
    client_with(transport, RetryPolicy::default(), Limits::default())
}

pub(crate) fn client_with(transport: Arc<Scripted>, retry: RetryPolicy, limits: Limits) -> Client {
    Client::builder()
        .auth(Auth::personal_access_token(TOKEN))
        .transport(transport)
        .base_url(BASE)
        .retry(retry)
        .limits(limits)
        .build()
        .unwrap()
}

pub(crate) fn get_task() -> asana_client::openapi::endpoints::GetTask {
    asana_client::openapi::endpoints::GetTask::builder(TASK.parse().unwrap()).build()
}

/// A made-up object some test descriptors return.
#[derive(Debug, Default, Deserialize, PartialEq)]
pub(crate) struct Thing {
    pub(crate) gid: Option<String>,
    pub(crate) name: Option<String>,
}

/// An empty answer, as the generator emits for operations that return `{"data": {}}`.
#[derive(Debug, Default, Deserialize, PartialEq)]
pub(crate) struct Empty {}

/// `POST /things`: creates something, so it is never sent twice on its own.
#[derive(Clone, Debug)]
pub(crate) struct CreateThing {
    pub(crate) name: String,
}

impl Endpoint for CreateThing {
    type Output = Thing;
    const OPERATION_ID: &'static str = "createThing";
    const METHOD: Method = Method::Post;
    const ROUTE: &'static str = "/things";
    const SCOPE: Option<Scope> = None;
    const IDEMPOTENT: bool = false;

    fn path(&self) -> String {
        "/things".into()
    }

    fn body(&self) -> Result<RequestBody, serde_json::Error> {
        Ok(RequestBody::Json(
            serde_json::json!({ "data": { "name": self.name } }),
        ))
    }
}

/// `POST /tasks/{task_gid}/addTag`: a POST that is safe to repeat.
#[derive(Clone, Debug)]
pub(crate) struct AddTag;

impl Endpoint for AddTag {
    type Output = Empty;
    const OPERATION_ID: &'static str = "addTagForTask";
    const METHOD: Method = Method::Post;
    const ROUTE: &'static str = "/tasks/{task_gid}/addTag";
    const SCOPE: Option<Scope> = None;
    const IDEMPOTENT: bool = true;

    fn path(&self) -> String {
        format!("/tasks/{TASK}/addTag")
    }
}

/// `DELETE /things/{gid}`, returning `()`.
#[derive(Clone, Debug)]
pub(crate) struct DeleteThing;

impl Endpoint for DeleteThing {
    type Output = ();
    const OPERATION_ID: &'static str = "deleteThing";
    const METHOD: Method = Method::Delete;
    const ROUTE: &'static str = "/things/{gid}";
    const SCOPE: Option<Scope> = None;
    const IDEMPOTENT: bool = true;

    fn path(&self) -> String {
        "/things/1".into()
    }
}

/// `GET /events`, whose answer is the whole body.
#[derive(Clone, Debug)]
pub(crate) struct GetEvents {
    pub(crate) query: Vec<QueryPair>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct EventsPage {
    pub(crate) data: Vec<serde_json::Value>,
    pub(crate) sync: String,
    pub(crate) has_more: bool,
}

impl Endpoint for GetEvents {
    type Output = EventsPage;
    const OPERATION_ID: &'static str = "getEvents";
    const METHOD: Method = Method::Get;
    const ROUTE: &'static str = "/events";
    const SCOPE: Option<Scope> = None;
    const IDEMPOTENT: bool = true;
    const ENVELOPE: Envelope = Envelope::Whole;

    fn path(&self) -> String {
        "/events".into()
    }

    fn query(&self) -> Vec<QueryPair> {
        self.query.clone()
    }
}

/// `POST /attachments`, a multipart upload.
#[derive(Clone, Debug)]
pub(crate) struct Upload;

impl Endpoint for Upload {
    type Output = Thing;
    const OPERATION_ID: &'static str = "createAttachmentForObject";
    const METHOD: Method = Method::Post;
    const ROUTE: &'static str = "/attachments";
    const SCOPE: Option<Scope> = None;
    const IDEMPOTENT: bool = false;

    fn path(&self) -> String {
        "/attachments".into()
    }

    fn body(&self) -> Result<RequestBody, serde_json::Error> {
        Ok(RequestBody::Multipart(vec![
            Part::new("parent", PartValue::Text(TASK.into())),
            Part::new(
                "file",
                PartValue::File {
                    file_name: "acme-plan.pdf".into(),
                    content_type: "application/pdf".into(),
                    bytes: Bytes::from_static(b"%PDF-1.7 made up"),
                },
            ),
        ]))
    }
}

/// `GET /workspaces/{workspace_gid}/tasks/search`, which has its own rate limit.
#[derive(Clone, Debug)]
pub(crate) struct SearchTasks;

impl Endpoint for SearchTasks {
    type Output = Vec<Thing>;
    const OPERATION_ID: &'static str = "searchTasksForWorkspace";
    const METHOD: Method = Method::Get;
    const ROUTE: &'static str = "/workspaces/{workspace_gid}/tasks/search";
    const SCOPE: Option<Scope> = None;
    const IDEMPOTENT: bool = true;

    fn path(&self) -> String {
        "/workspaces/1/tasks/search".into()
    }
}
