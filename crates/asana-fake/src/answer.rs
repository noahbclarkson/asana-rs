//! What a handler is given and what it answers: the parsed [`Call`], the [`Answer`] in Asana's
//! envelope, and the [`Refusal`] in Asana's error shape.

use http::StatusCode;
use serde_json::{Map, Value, json};
use time::OffsetDateTime;
use time::format_description::FormatItem;
use time::macros::format_description;

use crate::select::Selection;

/// One request, parsed: what the handlers read.
#[derive(Clone, Debug)]
pub(crate) struct Call {
    /// The values of the route's `{…}` segments, in order, percent-decoded.
    pub(crate) params: Vec<String>,
    /// The decoded query pairs.
    pub(crate) query: Vec<(String, String)>,
    /// The JSON body, when there was one.
    pub(crate) body: Option<Value>,
    /// The parts of a `multipart/form-data` body.
    pub(crate) form: Vec<crate::multipart::FormPart>,
    /// The fields asked for with `opt_fields`.
    pub(crate) selection: Selection,
    /// The path as it was asked, for `next_page.path`.
    pub(crate) path: String,
}

impl Call {
    /// The route's `index`th path parameter.
    pub(crate) fn param(&self, index: usize) -> &str {
        self.params.get(index).map_or("", String::as_str)
    }

    /// A query parameter's value.
    pub(crate) fn query(&self, name: &str) -> Option<&str> {
        self.query
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    /// The body's `data` object; empty when there is none.
    pub(crate) fn data(&self) -> &Map<String, Value> {
        static EMPTY: std::sync::LazyLock<Map<String, Value>> = std::sync::LazyLock::new(Map::new);
        self.body
            .as_ref()
            .and_then(|body| body.get("data"))
            .and_then(Value::as_object)
            .unwrap_or(&EMPTY)
    }

    /// A string member of `data`, or a refusal naming it when it is present but not a string.
    pub(crate) fn text(&self, name: &str) -> Result<Option<String>, Refusal> {
        match self.data().get(name) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(text)) => Ok(Some(text.clone())),
            Some(_) => Err(Refusal::bad_request(format!("{name}: Not a string"))),
        }
    }

    /// A required string member of `data`.
    pub(crate) fn required(&self, name: &str) -> Result<String, Refusal> {
        self.text(name)?
            .ok_or_else(|| Refusal::bad_request(format!("{name}: Missing input")))
    }

    /// A boolean member of `data`.
    pub(crate) fn flag(&self, name: &str) -> Result<Option<bool>, Refusal> {
        match self.data().get(name) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Bool(flag)) => Ok(Some(*flag)),
            Some(_) => Err(Refusal::bad_request(format!("{name}: Not a boolean"))),
        }
    }

    /// Whether `data` holds `name` at all, `null` included.
    pub(crate) fn has(&self, name: &str) -> bool {
        self.data().contains_key(name)
    }

    /// A list of GIDs in `data`: an array of strings, or one comma-separated string.
    pub(crate) fn gids(&self, name: &str) -> Result<Option<Vec<String>>, Refusal> {
        match self.data().get(name) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(text)) => Ok(Some(
                text.split(',')
                    .map(str::trim)
                    .filter(|gid| !gid.is_empty())
                    .map(str::to_owned)
                    .collect(),
            )),
            Some(Value::Array(items)) => items
                .iter()
                .map(|item| match item {
                    Value::String(gid) => Ok(gid.clone()),
                    Value::Object(object) => object
                        .get("gid")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                        .ok_or_else(|| Refusal::bad_request(format!("{name}: Not a GID"))),
                    _ => Err(Refusal::bad_request(format!("{name}: Not a GID"))),
                })
                .collect::<Result<Vec<_>, _>>()
                .map(Some),
            Some(_) => Err(Refusal::bad_request(format!("{name}: Not a list of GIDs"))),
        }
    }
}

/// A successful answer.
#[derive(Clone, Debug)]
pub(crate) struct Answer {
    pub(crate) status: StatusCode,
    pub(crate) body: Value,
    pub(crate) headers: Vec<(&'static str, String)>,
}

impl Answer {
    /// `200` with `{"data": data}`.
    pub(crate) fn data(data: Value) -> Self {
        Self::with_status(StatusCode::OK, json!({ "data": data }))
    }

    /// `201` with `{"data": data}`.
    pub(crate) fn created(data: Value) -> Self {
        Self::with_status(StatusCode::CREATED, json!({ "data": data }))
    }

    /// `200` with `{"data": {}}`, Asana's answer to a change that returns nothing.
    pub(crate) fn empty() -> Self {
        Self::data(json!({}))
    }

    /// Any status with a body as given.
    pub(crate) fn with_status(status: StatusCode, body: Value) -> Self {
        Self {
            status,
            body,
            headers: Vec::new(),
        }
    }
}

/// An answer in Asana's error shape: `{"errors": [{"message", "help"}]}`.
#[derive(Clone, Debug)]
pub(crate) struct Refusal {
    pub(crate) status: StatusCode,
    pub(crate) message: String,
    /// Members beside `errors` (the events stream's `sync`).
    pub(crate) extra: Map<String, Value>,
    pub(crate) headers: Vec<(&'static str, String)>,
}

/// Where Asana's error answers point for help.
const HELP: &str = "For more information on API status codes and how to handle them, read the docs on errors: https://developers.asana.com/docs/errors";

impl Refusal {
    /// A refusal with `status` and `message`.
    pub(crate) fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            extra: Map::new(),
            headers: Vec::new(),
        }
    }

    /// `400`.
    pub(crate) fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }

    /// `403`.
    pub(crate) fn forbidden(message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, message)
    }

    /// `404` for an object that does not exist (or `400` when the text is not a GID at all).
    pub(crate) fn unknown(param: &str, gid: &str) -> Self {
        if gid.is_empty() || !gid.bytes().all(|byte| byte.is_ascii_digit()) {
            return Self::bad_request(format!("{param}: Not a valid GID: {gid}"));
        }
        Self::not_found(param, gid)
    }

    /// `404` naming the parameter and the value that matched nothing.
    pub(crate) fn not_found(param: &str, value: &str) -> Self {
        Self::new(
            StatusCode::NOT_FOUND,
            format!("{param}: Unknown object: {value}"),
        )
    }

    /// The JSON body.
    pub(crate) fn body(&self) -> Value {
        let mut error = Map::new();
        error.insert("message".into(), Value::String(self.message.clone()));
        error.insert("help".into(), Value::String(HELP.to_owned()));
        if self.status == StatusCode::INTERNAL_SERVER_ERROR {
            error.insert(
                "phrase".into(),
                Value::String("6 sad squid snuggle softly".to_owned()),
            );
        }
        let mut body = Map::new();
        body.insert("errors".into(), Value::Array(vec![Value::Object(error)]));
        body.extend(self.extra.clone());
        Value::Object(body)
    }

    /// As an [`Answer`].
    pub(crate) fn into_answer(self) -> Answer {
        Answer {
            status: self.status,
            body: self.body(),
            headers: self.headers,
        }
    }
}

/// What every handler returns.
pub(crate) type Reply = Result<Answer, Refusal>;

/// How Asana writes a time: UTC, milliseconds, `Z`.
const STAMP: &[FormatItem<'static>] =
    format_description!("[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:3]Z");

/// Writes `time` as Asana does, for example `2026-01-05T09:30:00.000Z`.
pub(crate) fn stamp(time: OffsetDateTime) -> String {
    time.to_offset(time::UtcOffset::UTC)
        .format(STAMP)
        .unwrap_or_default()
}

/// [`stamp`] as JSON, `null` for none.
pub(crate) fn stamp_value(time: Option<OffsetDateTime>) -> Value {
    time.map_or(Value::Null, |time| Value::String(stamp(time)))
}
