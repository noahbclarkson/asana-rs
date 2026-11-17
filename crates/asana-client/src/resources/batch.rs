//! The batch API: up to ten operations in one request.
//!
//! Any descriptor can go in a batch. [`Batch::push`] turns it into an action (method, relative
//! path, `data` and `options`) and hands back a [`Ticket`] typed by the descriptor;
//! [`BatchResults::take`] redeems the ticket for that action's own result, decoded as
//! [`Client::send`] would decode it, or its own [`Error`].
//!
//! ```no_run
//! # async fn run(client: asana_client::Client, a: asana_client::openapi::ids::TaskGid,
//! #     b: asana_client::openapi::ids::TaskGid) -> Result<(), asana_client::Error> {
//! use asana_client::openapi::endpoints::GetTask;
//!
//! let mut batch = client.batch();
//! let first = batch.push(GetTask::builder(a).build())?;
//! let second = batch.push(GetTask::builder(b).build())?;
//! let mut results = batch.send().await?;
//! let first = results.take(first)?;
//! let second = results.take(second); // one action failing leaves the others readable
//! # let _ = (first, second); Ok(()) }
//! ```
//!
//! Asana runs the actions in parallel, in no promised order, and answers in the order they were
//! pushed. Each action counts against the rate limits as a request of its own; if any would be
//! limited the whole batch answers `429`, which the client waits out and retries like any call.
//! An upload cannot be batched, nor can a batch.

use std::marker::PhantomData;
use std::sync::atomic::{AtomicU64, Ordering};

use asana_openapi::endpoint::{Endpoint, Method, RequestBody};
use asana_openapi::scopes::Scope;
use http::StatusCode;
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::retry::safe_to_repeat;
use crate::{Client, Error, decode_response};

/// The most actions Asana accepts in one batch.
pub const MAX_ACTIONS: usize = 10;

/// The operation every batch is sent as.
const OPERATION: &str = "createBatchRequest";

/// Numbers batches, so a ticket cannot be redeemed against another batch's results.
static NEXT_BATCH: AtomicU64 = AtomicU64::new(0);

impl Client {
    /// A new, empty batch of up to ten operations.
    pub fn batch(&self) -> Batch<'_> {
        Batch {
            client: self,
            id: NEXT_BATCH.fetch_add(1, Ordering::Relaxed),
            actions: Vec::new(),
            repeatable: true,
        }
    }
}

/// Up to ten operations to send together, from [`Client::batch`].
#[must_use = "a batch does nothing until it is sent"]
#[derive(Debug)]
pub struct Batch<'a> {
    client: &'a Client,
    id: u64,
    actions: Vec<Value>,
    /// Whether every action is safe to send twice, so the batch is too.
    repeatable: bool,
}

/// A claim on one action's result, typed by its descriptor. Redeem it with
/// [`BatchResults::take`].
#[must_use = "a ticket is the only way to read its action's result"]
pub struct Ticket<E> {
    batch: u64,
    index: usize,
    endpoint: PhantomData<fn() -> E>,
}

impl<E> Ticket<E> {
    /// The action's position in the batch, from 0.
    pub fn index(&self) -> usize {
        self.index
    }
}

impl<E> std::fmt::Debug for Ticket<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ticket")
            .field("index", &self.index)
            .finish_non_exhaustive()
    }
}

impl Batch<'_> {
    /// Adds an operation and returns the ticket for its result.
    ///
    /// The descriptor's query becomes the action's `data` (for a `GET`) or is merged into its
    /// body's `data`; `opt_fields`, `limit` and `offset` become its `options`.
    ///
    /// # Errors
    ///
    /// [`Error::BatchFull`] when the batch already holds ten actions; [`Error::InvalidRequest`]
    /// for an upload, whose multipart body a batch cannot carry; [`Error::Encode`] when the
    /// body cannot be written as JSON.
    pub fn push<E: Endpoint>(&mut self, endpoint: E) -> Result<Ticket<E>, Error> {
        if self.actions.len() >= MAX_ACTIONS {
            return Err(Error::BatchFull { limit: MAX_ACTIONS });
        }
        self.actions.push(action(&endpoint)?);
        self.repeatable &= safe_to_repeat(E::METHOD, E::IDEMPOTENT);
        Ok(Ticket {
            batch: self.id,
            index: self.actions.len() - 1,
            endpoint: PhantomData,
        })
    }

    /// How many actions the batch holds.
    pub fn len(&self) -> usize {
        self.actions.len()
    }

    /// Whether the batch holds no actions.
    pub fn is_empty(&self) -> bool {
        self.actions.is_empty()
    }

    /// Sends the batch and returns every action's answer, to be read with tickets.
    ///
    /// Sends `createBatchRequest` (`POST /batch`); no granular scope (each action needs its own).
    /// A `429` is waited out and retried like any call. After a server failure or a lost
    /// connection the batch is sent again only when every action in it is safe to repeat. An
    /// empty batch sends nothing.
    ///
    /// # Errors
    ///
    /// An [`Error`] for the batch as a whole. One action failing is not one: its error comes
    /// from [`BatchResults::take`].
    pub async fn send(self) -> Result<BatchResults, Error> {
        if self.actions.is_empty() {
            return Ok(BatchResults {
                batch: self.id,
                answers: Vec::new(),
            });
        }
        let answers = if self.repeatable {
            self.client
                .send(BatchCall::<true> {
                    actions: self.actions,
                })
                .await?
        } else {
            self.client
                .send(BatchCall::<false> {
                    actions: self.actions,
                })
                .await?
        };
        Ok(BatchResults {
            batch: self.id,
            answers: answers.into_iter().map(Some).collect(),
        })
    }
}

/// Every action's answer, in the order the actions were pushed.
#[derive(Debug)]
pub struct BatchResults {
    batch: u64,
    answers: Vec<Option<ActionAnswer>>,
}

impl BatchResults {
    /// The result of the action `ticket` was issued for, decoded to its descriptor's output, or
    /// the error its status and Asana's error body map to (as [`Client::send`] maps them).
    ///
    /// # Errors
    ///
    /// The action's own [`Error`]; [`Error::InvalidRequest`] for a ticket from another batch;
    /// [`Error::MissingField`] when Asana's answer had no entry for the action.
    pub fn take<E: Endpoint>(&mut self, ticket: Ticket<E>) -> Result<E::Output, Error> {
        if ticket.batch != self.batch {
            return Err(Error::InvalidRequest {
                operation: OPERATION,
                reason: "the ticket belongs to another batch",
            });
        }
        let answer = self
            .answers
            .get_mut(ticket.index)
            .and_then(Option::take)
            .ok_or(Error::MissingField("batch result"))?;
        let status = StatusCode::from_u16(answer.status_code)
            .map_err(|_| Error::MissingField("status_code"))?;
        let body = serde_json::to_vec(&answer.body).map_err(|source| Error::Encode {
            operation: E::OPERATION_ID,
            source,
        })?;
        decode_response::<E>(status, &body)
    }

    /// The HTTP status each action answered with, in order.
    pub fn statuses(&self) -> impl Iterator<Item = Option<u16>> + '_ {
        self.answers
            .iter()
            .map(|answer| answer.as_ref().map(|answer| answer.status_code))
    }

    /// How many answers Asana returned.
    pub fn len(&self) -> usize {
        self.answers.len()
    }

    /// Whether Asana returned no answers.
    pub fn is_empty(&self) -> bool {
        self.answers.is_empty()
    }
}

/// One action's answer as the batch response carries it.
#[derive(Debug, Deserialize)]
struct ActionAnswer {
    status_code: u16,
    #[serde(default)]
    body: Value,
}

/// `POST /batch`, with whether a retry is safe fixed by its type, since the client reads it from
/// a constant.
struct BatchCall<const REPEATABLE: bool> {
    actions: Vec<Value>,
}

impl<const REPEATABLE: bool> Endpoint for BatchCall<REPEATABLE> {
    type Output = Vec<ActionAnswer>;
    const OPERATION_ID: &'static str = OPERATION;
    const METHOD: Method = Method::Post;
    const ROUTE: &'static str = "/batch";
    const SCOPE: Option<Scope> = None;
    const IDEMPOTENT: bool = REPEATABLE;

    fn path(&self) -> String {
        "/batch".to_owned()
    }

    fn body(&self) -> Result<RequestBody, serde_json::Error> {
        Ok(RequestBody::Json(serde_json::json!({
            "data": { "actions": self.actions }
        })))
    }
}

/// One descriptor as a batch action.
fn action<E: Endpoint>(endpoint: &E) -> Result<Value, Error> {
    let refused = |reason| Error::InvalidRequest {
        operation: E::OPERATION_ID,
        reason,
    };
    let mut data = match endpoint.body().map_err(|source| Error::Encode {
        operation: E::OPERATION_ID,
        source,
    })? {
        RequestBody::Empty => Map::new(),
        RequestBody::Json(Value::Object(mut body)) => match body.remove("data") {
            Some(Value::Object(data)) => data,
            None if body.is_empty() => Map::new(),
            _ => {
                return Err(refused(
                    "its body's `data` is not an object, as a batch needs",
                ));
            }
        },
        RequestBody::Json(_) => return Err(refused("its body is not a JSON object")),
        RequestBody::Multipart(_) => return Err(refused("an upload cannot be sent in a batch")),
        _ => return Err(refused("its body is of a kind a batch cannot carry")),
    };
    let mut options = Map::new();
    for (name, value) in endpoint.query() {
        place(&mut data, &mut options, &name, value);
    }
    let mut action = Map::new();
    action.insert(
        "method".into(),
        Value::String(E::METHOD.as_str().to_ascii_lowercase()),
    );
    action.insert("relative_path".into(), Value::String(endpoint.path()));
    if !data.is_empty() {
        action.insert("data".into(), Value::Object(data));
    }
    if !options.is_empty() {
        action.insert("options".into(), Value::Object(options));
    }
    Ok(Value::Object(action))
}

/// Puts one query parameter where a batch action takes it.
fn place(
    data: &mut Map<String, Value>,
    options: &mut Map<String, Value>,
    name: &str,
    value: String,
) {
    match name {
        "opt_fields" => {
            let fields = value
                .split(',')
                .filter(|field| !field.is_empty())
                .map(|field| Value::String(field.to_owned()))
                .collect();
            options.insert("fields".into(), Value::Array(fields));
        }
        "limit" => {
            let limit = value
                .parse::<u64>()
                .map_or(Value::String(value), Value::from);
            options.insert("limit".into(), limit);
        }
        // An offset is opaque, so it stays the string Asana gave.
        "offset" => {
            options.insert("offset".into(), Value::String(value));
        }
        "opt_pretty" => {}
        _ => {
            data.entry(name.to_owned()).or_insert(Value::String(value));
        }
    }
}
