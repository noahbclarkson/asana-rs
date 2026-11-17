//! The fixed items of the generated `endpoints/mod.rs`: the results that are a whole response body,
//! the formatting helpers the descriptors call, and the operation index.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};

use crate::GenResult;

/// The results that carry siblings of `data`, by those siblings (sorted): the type each becomes.
const WHOLE: &[(&[&str], &str)] = &[
    // The events stream: the events, the sync token for the next call and whether more wait.
    (&["has_more", "sync"], "EventsPage"),
    // Webhook creation: the webhook and the secret that signs its deliveries.
    (&["X-Hook-Secret"], "CreatedWebhook"),
];

/// The type a whole-body result with these `siblings` of `data` becomes.
pub fn whole_type(siblings: &[String]) -> GenResult<&'static str> {
    let mut sorted: Vec<&str> = siblings.iter().map(String::as_str).collect();
    sorted.sort_unstable();
    WHOLE
        .iter()
        .find(|(known, _)| *known == sorted.as_slice())
        .map(|(_, name)| *name)
        .ok_or_else(|| {
            format!(
                "a response has the siblings {siblings:?} beside data: add a type for them to \
                 xtask/src/endpoints/support.rs"
            )
        })
}

/// The definition of the whole-body result `name`, whose `data` holds `element` values.
pub fn whole_item(name: &str, element: &TokenStream) -> GenResult<TokenStream> {
    match name {
        "EventsPage" => Ok(quote! {
            /// A page of the events stream: the events since the sync token, and the token to send
            /// next. The whole response body, because `sync` and `has_more` sit beside `data`.
            #[derive(Clone, Debug, serde::Deserialize)]
            #[non_exhaustive]
            pub struct EventsPage {
                /// The events, oldest first.
                #[serde(default)]
                pub data: Vec<#element>,
                /// The sync token to send with the next request for this resource's events.
                #[serde(default)]
                pub sync: Option<String>,
                /// Whether more events are waiting: ask again at once with `sync`.
                #[serde(default)]
                pub has_more: bool,
            }
        }),
        "CreatedWebhook" => Ok(quote! {
            /// A newly created webhook and the secret Asana signs its deliveries with. The whole
            /// response body, because `X-Hook-Secret` sits beside `data`.
            #[derive(Clone, Debug, serde::Deserialize)]
            #[non_exhaustive]
            pub struct CreatedWebhook {
                /// The webhook.
                pub data: #element,
                /// The secret that signs the webhook's deliveries, when Asana returns it here.
                #[serde(rename = "X-Hook-Secret", default, deserialize_with = "secret")]
                pub hook_secret: Option<secrecy::SecretString>,
            }

            /// Reads a secret without ever holding it in a plain `String` longer than the read.
            fn secret<'de, D: serde::Deserializer<'de>>(
                deserializer: D,
            ) -> Result<Option<secrecy::SecretString>, D::Error> {
                let text: Option<String> = serde::Deserialize::deserialize(deserializer)?;
                Ok(text.map(secrecy::SecretString::from))
            }
        }),
        other => Err(format!("no definition for the whole-body result {other}")),
    }
}

/// Which helpers the descriptors call (`super::<name>`), so only those are emitted.
#[derive(Default)]
pub struct Helpers {
    used: std::collections::BTreeSet<String>,
}

impl Helpers {
    /// Notes the helpers `tokens` calls.
    pub fn note(&mut self, tokens: &TokenStream) {
        fn walk(tokens: TokenStream, flat: &mut Vec<proc_macro2::TokenTree>) {
            for tree in tokens {
                if let proc_macro2::TokenTree::Group(group) = &tree {
                    walk(group.stream(), flat);
                }
                flat.push(tree);
            }
        }
        let mut flat = Vec::new();
        walk(tokens.clone(), &mut flat);
        for window in flat.windows(4) {
            if let [
                proc_macro2::TokenTree::Ident(first),
                proc_macro2::TokenTree::Punct(colon),
                proc_macro2::TokenTree::Punct(_),
                proc_macro2::TokenTree::Ident(name),
            ] = window
                && first == "super"
                && colon.as_char() == ':'
            {
                self.used.insert(name.to_string());
            }
        }
        if self.used.contains("nullable_date") {
            self.used.insert("date".to_owned());
        }
    }

    fn uses(&self, name: &str) -> bool {
        self.used.contains(name)
    }

    /// The helper functions.
    pub fn items(&self) -> TokenStream {
        let query = self.uses("query").then(|| {
            quote! {
                /// The query of an operation: its required parameters, then each optional one
                /// that is set.
                fn query<const N: usize>(
                    mut query: Vec<crate::endpoint::QueryPair>,
                    optional: [(&'static str, Option<String>); N],
                ) -> Vec<crate::endpoint::QueryPair> {
                    query.extend(
                        optional
                            .into_iter()
                            .filter_map(|(name, value)| Some((name.into(), value?))),
                    );
                    query
                }
            }
        });
        let path_segment = self.uses("path_segment").then(|| {
            quote! {
                /// Percent-encodes a path parameter that is not a GID, so it stays one path segment.
                /// Everything but ASCII letters, digits, `-`, `_` and `~` is encoded, dots included,
                /// so `.` and `..` cannot walk the path.
                fn path_segment(text: &str) -> String {
                    const HEX: &[u8; 16] = b"0123456789ABCDEF";
                    let mut encoded = String::with_capacity(text.len());
                    for byte in text.bytes() {
                        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'~') {
                            encoded.push(char::from(byte));
                        } else {
                            encoded.push('%');
                            encoded.push(char::from(HEX[usize::from(byte >> 4)]));
                            encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
                        }
                    }
                    encoded
                }
            }
        });
        let json_data = self.uses("json_data").then(|| {
            quote! {
                /// Wraps a request's data in Asana's `{"data": ...}` envelope.
                fn json_data<T: serde::Serialize>(
                    data: &T,
                ) -> Result<crate::endpoint::RequestBody, serde_json::Error> {
                    let mut envelope = serde_json::Map::new();
                    envelope.insert("data".to_owned(), serde_json::to_value(data)?);
                    Ok(crate::endpoint::RequestBody::Json(serde_json::Value::Object(envelope)))
                }
            }
        });
        let date = self.uses("date").then(|| {
            quote! {
                /// A date as Asana reads it in a query: `YYYY-MM-DD`.
                fn date(date: time::Date) -> String {
                    format!("{:04}-{:02}-{:02}", date.year(), u8::from(date.month()), date.day())
                }
            }
        });
        let nullable_date = self.uses("nullable_date").then(|| {
            quote! {
                /// A date filter that may also be `null`, meaning "has no date".
                fn nullable_date(value: Option<time::Date>) -> String {
                    value.map_or_else(|| "null".to_owned(), date)
                }
            }
        });
        let date_time = self.uses("date_time").then(|| {
            quote! {
                /// A timestamp as Asana reads it in a query: RFC 3339 with milliseconds, in the
                /// value's own offset (`Z` for UTC).
                fn date_time(at: time::OffsetDateTime) -> String {
                    let offset = at.offset();
                    let zone = if offset.is_utc() {
                        "Z".to_owned()
                    } else {
                        format!(
                            "{}{:02}:{:02}",
                            if offset.is_negative() { '-' } else { '+' },
                            offset.whole_hours().unsigned_abs(),
                            offset.minutes_past_hour().unsigned_abs()
                        )
                    };
                    format!(
                        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}{zone}",
                        at.year(),
                        u8::from(at.month()),
                        at.day(),
                        at.hour(),
                        at.minute(),
                        at.second(),
                        at.millisecond()
                    )
                }
            }
        });
        quote! { #query #path_segment #json_data #date #nullable_date #date_time }
    }
}

/// One row of the operation index.
pub struct IndexRow {
    /// The descriptor's path from `endpoints`, for example `tasks::GetTask`.
    pub module: String,
    pub type_name: String,
    pub tag: String,
    pub paginated: bool,
}

/// `endpoints/index.rs`: `OperationInfo` and `OPERATIONS`, read from each descriptor's constants
/// so the index cannot disagree with them.
pub fn index(rows: &[IndexRow]) -> TokenStream {
    let entries = rows.iter().map(|row| {
        let module = format_ident!("{}", row.module);
        let name = format_ident!("{}", row.type_name);
        let tag = &row.tag;
        let paginated = row.paginated;
        quote! { OperationInfo::of::<super::#module::#name>(#tag, #paginated) }
    });
    quote! {
        //! An index of every operation, for coverage tables and tests.

        use crate::endpoint::{Endpoint, Method};
        use crate::scopes::Scope;

        /// What the index records about one operation.
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        #[non_exhaustive]
        pub struct OperationInfo {
            /// The `operationId`, for example `getTask`.
            pub operation_id: &'static str,
            /// The HTTP method.
            pub method: Method,
            /// The path template, for example `/tasks/{task_gid}`.
            pub route: &'static str,
            /// The granular OAuth scope it needs, where the specification names one.
            pub scope: Option<Scope>,
            /// The specification's tag, which is also the descriptor's module.
            pub tag: &'static str,
            /// Whether it may be sent again after an unknown outcome.
            pub idempotent: bool,
            /// Whether it returns pages (implements `Paginated`).
            pub paginated: bool,
        }

        impl OperationInfo {
            const fn of<E: Endpoint>(tag: &'static str, paginated: bool) -> Self {
                Self {
                    operation_id: E::OPERATION_ID,
                    method: E::METHOD,
                    route: E::ROUTE,
                    scope: E::SCOPE,
                    tag,
                    idempotent: E::IDEMPOTENT,
                    paginated,
                }
            }
        }

        /// Every operation in Asana's specification, in specification order.
        pub const OPERATIONS: &[OperationInfo] = &[ #( #entries ),* ];
    }
}
