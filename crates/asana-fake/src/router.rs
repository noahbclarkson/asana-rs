//! Which handler answers a request.
//!
//! The table is built from the generated descriptors' own `METHOD`, `ROUTE` and `OPERATION_ID`
//! constants (`route::<GetTask>(..)`), so it cannot drift from the generated code. A request for
//! an operation the specification has but the fake does not serve is matched against the
//! generated index of every operation and answered `501` naming it.

use std::sync::LazyLock;

use asana_openapi::endpoint::{Endpoint, Method};
use asana_openapi::endpoints::OPERATIONS;

use crate::answer::{Call, Refusal, Reply};
use crate::state::State;

/// A handler that reads and changes the state under its lock.
pub(crate) type Handler = fn(&mut State, &Call) -> Reply;

/// What answers a route.
#[derive(Clone, Copy)]
pub(crate) enum Action {
    /// An ordinary operation.
    Sync(Handler),
    /// `createWebhook`, which calls the receiver while the request is open.
    CreateWebhook,
}

/// One served operation.
pub(crate) struct Route {
    method: Method,
    segments: Vec<Segment>,
    pub(crate) operation: &'static str,
    pub(crate) action: Action,
    /// Whether the specification lists an `opt_fields` path for the operation's resource.
    opt_fields: Option<fn(&str) -> bool>,
}

impl Route {
    /// Refuses, as Asana does with `400`, an `opt_fields` path the specification does not list
    /// for the operation's resource (a misspelt `with_raw` path, say).
    pub(crate) fn check_fields(&self, query: &[(String, String)]) -> Result<(), Refusal> {
        let requested = query
            .iter()
            .filter(|(key, _)| key == "opt_fields")
            .flat_map(|(_, value)| value.split(','));
        self.check_paths(requested)
    }

    /// As [`check_fields`](Self::check_fields), for paths given one by one (a batch action's
    /// `options.fields`).
    pub(crate) fn check_paths<'a>(
        &self,
        paths: impl IntoIterator<Item = &'a str>,
    ) -> Result<(), Refusal> {
        let Some(lists) = self.opt_fields else {
            return Ok(());
        };
        match paths
            .into_iter()
            .map(str::trim)
            .find(|path| !path.is_empty() && !lists(path))
        {
            Some(path) => Err(Refusal::bad_request(format!(
                "opt_fields: `{path}` is not a field of this resource"
            ))),
            None => Ok(()),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Segment {
    Literal(&'static str),
    Param,
}

/// The route of descriptor `E`, answered by `action`.
pub(crate) fn route<E: Endpoint>(action: Action) -> Route {
    Route {
        method: E::METHOD,
        segments: segments(E::ROUTE),
        operation: E::OPERATION_ID,
        action,
        opt_fields: E::OPT_FIELDS,
    }
}

/// A served operation answered by a plain handler.
pub(crate) fn serve<E: Endpoint>(handler: Handler) -> Route {
    route::<E>(Action::Sync(handler))
}

fn segments(template: &'static str) -> Vec<Segment> {
    template
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(|segment| {
            if segment.starts_with('{') {
                Segment::Param
            } else {
                Segment::Literal(segment)
            }
        })
        .collect()
}

/// What a request matched.
pub(crate) enum Matched<'a> {
    /// A served operation, with its path parameters.
    Served(&'a Route, Vec<String>),
    /// An operation in the specification that the fake does not serve.
    Unserved(&'static str),
    /// Nothing in the specification.
    Nothing,
}

impl Matched<'_> {
    /// The operation's ID, when the request named one.
    pub(crate) fn operation(&self) -> Option<&'static str> {
        match self {
            Self::Served(route, _) => Some(route.operation),
            Self::Unserved(operation) => Some(operation),
            Self::Nothing => None,
        }
    }
}

/// Every served operation.
static ROUTES: LazyLock<Vec<Route>> = LazyLock::new(crate::ops::routes);

/// Finds what answers `method` on `path` (the path below `/api/1.0`, without its query).
pub(crate) fn find(method: &str, path: &str) -> Matched<'static> {
    let parts: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    let mut best: Option<(usize, &Route, Vec<String>)> = None;
    for route in ROUTES.iter() {
        if !route.method.as_str().eq_ignore_ascii_case(method) {
            continue;
        }
        if let Some((literals, params)) = matches(&route.segments, &parts)
            && best
                .as_ref()
                .is_none_or(|(best_literals, _, _)| literals > *best_literals)
        {
            best = Some((literals, route, params));
        }
    }
    if let Some((_, route, params)) = best {
        return Matched::Served(route, params);
    }
    let mut unserved: Option<(usize, &'static str)> = None;
    for operation in OPERATIONS {
        if !operation.method.as_str().eq_ignore_ascii_case(method) {
            continue;
        }
        if let Some((literals, _)) = matches(&segments(operation.route), &parts)
            && unserved.is_none_or(|(best, _)| literals > best)
        {
            unserved = Some((literals, operation.operation_id));
        }
    }
    unserved.map_or(Matched::Nothing, |(_, operation)| {
        Matched::Unserved(operation)
    })
}

/// Whether `parts` fit `segments`: how many literal segments matched, and the parameters.
fn matches(segments: &[Segment], parts: &[&str]) -> Option<(usize, Vec<String>)> {
    if segments.len() != parts.len() {
        return None;
    }
    let mut literals = 0;
    let mut params = Vec::new();
    for (segment, part) in segments.iter().zip(parts) {
        match segment {
            Segment::Literal(literal) if literal == part => literals += 1,
            Segment::Literal(_) => return None,
            Segment::Param => params.push(decode(part)),
        }
    }
    Some((literals, params))
}

/// Percent-decodes one path segment; a malformed escape is kept as it is.
pub(crate) fn decode(segment: &str) -> String {
    let bytes = segment.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && let (Some(high), Some(low)) = (
                bytes.get(index + 1).and_then(|byte| hex(*byte)),
                bytes.get(index + 2).and_then(|byte| hex(*byte)),
            )
        {
            out.push(high * 16 + low);
            index += 3;
            continue;
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_served_route_is_found_as_itself() {
        for route in ROUTES.iter() {
            let path: Vec<&str> = route
                .segments
                .iter()
                .map(|segment| match segment {
                    Segment::Literal(literal) => *literal,
                    Segment::Param => "1200000000000001",
                })
                .collect();
            let path = format!("/{}", path.join("/"));
            match find(route.method.as_str(), &path) {
                Matched::Served(found, _) => assert_eq!(found.operation, route.operation, "{path}"),
                _ => panic!("{} on {path} is not found", route.operation),
            }
        }
    }

    #[test]
    fn an_operation_the_fake_does_not_serve_is_named() {
        assert_eq!(
            find("GET", "/goals/1201234567890123").operation(),
            Some("getGoal")
        );
        assert!(matches!(
            find("GET", "/goals/1201234567890123"),
            Matched::Unserved("getGoal")
        ));
        assert!(matches!(
            find("GET", "/no/such/thing/here"),
            Matched::Nothing
        ));
    }

    #[test]
    fn a_literal_segment_wins_over_a_parameter() {
        let Matched::Served(route, params) = find("POST", "/sections/1200000000000001/addTask")
        else {
            panic!("addTask is served");
        };
        assert_eq!(route.operation, "addTaskForSection");
        assert_eq!(params, ["1200000000000001"]);
    }

    #[test]
    fn path_segments_are_percent_decoded() {
        assert_eq!(decode("ada%40example%2Ecom"), "ada@example.com");
        assert_eq!(decode("100%"), "100%");
    }
}
