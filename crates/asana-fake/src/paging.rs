//! Paged lists: `limit` (1 to 100) and an opaque `offset`, answered with `next_page`.
//!
//! Without a `limit` the whole list comes back in one answer with no `next_page`, as Asana answers
//! its older unpaginated calls. An offset the fake did not hand out, or one handed out for another
//! list (another path, or the same path with other filters), is refused with `400`, as Asana
//! refuses an offset that is not this query's.

use serde_json::{Value, json};

use crate::answer::{Answer, Call, Refusal, Reply};
use crate::state::{Id, State};

/// The prefix of every offset the fake hands out. The rest is the position and a digest of the
/// list it belongs to, so an offset reads as opaque but is checked when it comes back.
const OFFSET_PREFIX: &str = "eyJmYWtlIjoib2Zmc2V0In0.";

/// The query parameters that choose the page or its shape rather than the list, so an offset
/// stays good when they change.
const PAGE_PARAMETERS: &[&str] = &["offset", "limit", "opt_fields", "opt_pretty"];

/// A digest of the list a call reads: its path and its other query parameters, in any order.
/// FNV-1a, so it is the same in every run.
fn list_digest(call: &Call) -> u64 {
    let mut filters: Vec<(&str, &str)> = call
        .query
        .iter()
        .filter(|(key, _)| !PAGE_PARAMETERS.contains(&key.as_str()))
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    filters.sort_unstable();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut feed = |bytes: &[u8]| {
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
        // A separator no key or value can make ambiguous.
        hash ^= 0xff;
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    };
    feed(call.path.as_bytes());
    for (key, value) in filters {
        feed(key.as_bytes());
        feed(value.as_bytes());
    }
    hash
}

/// Answers one page of `items` (GIDs of records), each written as a list item.
pub(crate) fn page(state: &State, call: &Call, items: &[Id]) -> Reply {
    let (start, end, next) = window(call, items.len())?;
    let data = items
        .get(start..end)
        .unwrap_or_default()
        .iter()
        .map(|gid| call.selection.item(state, gid))
        .collect();
    Ok(paged(call, data, next))
}

/// Answers one page of values already written.
pub(crate) fn page_values(call: &Call, items: Vec<Value>) -> Reply {
    let (start, end, next) = window(call, items.len())?;
    let data = items
        .into_iter()
        .skip(start)
        .take(end.saturating_sub(start))
        .collect();
    Ok(paged(call, data, next))
}

/// Where the page starts and ends, and where the next one starts if there is one.
fn window(call: &Call, total: usize) -> Result<(usize, usize, Option<usize>), Refusal> {
    let limit = match call.query("limit") {
        None => None,
        Some(text) => match text.parse::<usize>() {
            Ok(limit @ 1..=100) => Some(limit),
            _ => {
                return Err(Refusal::bad_request(
                    "limit: The limit must be between 1 and 100",
                ));
            }
        },
    };
    let start = match call.query("offset") {
        None => 0,
        Some(offset) => offset
            .strip_prefix(OFFSET_PREFIX)
            .and_then(|rest| rest.split_once('.'))
            .filter(|(_, digest)| *digest == format!("{:016x}", list_digest(call)))
            .and_then(|(position, _)| position.parse::<usize>().ok())
            .ok_or_else(|| {
                Refusal::bad_request("offset: Your pagination token is invalid or has expired")
            })?,
    };
    let Some(limit) = limit else {
        return Ok((start.min(total), total, None));
    };
    let end = start.saturating_add(limit).min(total);
    let next = (end < total).then_some(end);
    Ok((start.min(total), end, next))
}

fn paged(call: &Call, data: Vec<Value>, next: Option<usize>) -> Answer {
    let next_page = match next {
        None => Value::Null,
        Some(position) => {
            let offset = format!("{OFFSET_PREFIX}{position}.{:016x}", list_digest(call));
            let mut query: Vec<String> = call
                .query
                .iter()
                .filter(|(key, _)| key != "offset")
                .map(|(key, value)| format!("{key}={value}"))
                .collect();
            query.push(format!("offset={offset}"));
            let path = format!("{}?{}", call.path, query.join("&"));
            json!({
                "offset": offset,
                "path": path,
                "uri": format!("https://app.asana.com/api/1.0{path}"),
            })
        }
    };
    if call.query("limit").is_none() {
        return Answer::data(Value::Array(data));
    }
    Answer::with_status(
        http::StatusCode::OK,
        json!({ "data": data, "next_page": next_page }),
    )
}
