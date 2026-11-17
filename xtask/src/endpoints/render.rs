//! Renders one [`Operation`] as a descriptor: the struct, its `Endpoint` and `Paginated` impls, and
//! the enums of its query parameters.

use proc_macro2::{Ident, Span, TokenStream};
use quote::{format_ident, quote};

use super::docs;
use super::operation::{Body, Operation, Param, ParamKind};
use crate::GenResult;

/// The Rust types an operation's schemas became, from the models half.
pub struct Types {
    /// `Endpoint::Output`.
    pub output: TokenStream,
    /// `Paginated::Item`, for a paged list.
    pub item: Option<TokenStream>,
    /// The type of the request's `data`, for a JSON body.
    pub body: Option<TokenStream>,
    /// Whether the result is the whole body (`Envelope::Whole`).
    pub whole: bool,
    /// Whether sending twice has the effect of sending once.
    pub idempotent: bool,
    /// The `Scope` variant, when the operation names a scope.
    pub scope: Option<Ident>,
}

/// The descriptor's items.
pub fn descriptor(operation: &Operation<'_>, types: &Types) -> GenResult<TokenStream> {
    let name = format_ident!("{}", operation.type_name);
    let doc = docs::attributes(&docs::operation(
        operation.summary.as_deref(),
        operation.description.as_deref(),
        operation.deprecated,
        operation.method.as_str(),
        &operation.route,
    ));

    let path_fields = operation.path_params.iter().map(path_field);
    let query_fields = operation
        .query_params
        .iter()
        .map(query_field)
        .collect::<Vec<_>>();
    let body_field = match (&operation.body, &types.body) {
        (
            Body::Json {
                required,
                description,
                ..
            },
            Some(ty),
        ) => {
            let doc = docs::attributes(&docs::or(*description, "The request's `data`."));
            if *required {
                quote! { #doc pub data: #ty, }
            } else {
                quote! { #doc pub data: Option<#ty>, }
            }
        }
        (Body::None, None) => quote! {},
        (Body::Multipart(_), _) => {
            return Err("a multipart operation is rendered by the multipart module".to_owned());
        }
        _ => return Err("a JSON body without a type".to_owned()),
    };

    let output = &types.output;
    let operation_id = &operation.id;
    let method = format_ident!("{}", operation.method.variant());
    let route = &operation.route;
    let scope = match &types.scope {
        Some(variant) => quote! { Some(crate::scopes::Scope::#variant) },
        None => quote! { None },
    };
    let idempotent = types.idempotent;
    let envelope = types.whole.then(|| {
        quote! { const ENVELOPE: crate::endpoint::Envelope = crate::endpoint::Envelope::Whole; }
    });
    let path = path_fn(operation);
    let query = query_fn(&operation.query_params);
    let body = match (&operation.body, &types.body) {
        (Body::Json { required: true, .. }, Some(_)) => quote! {
            fn body(&self) -> Result<crate::endpoint::RequestBody, serde_json::Error> {
                super::json_data(&self.data)
            }
        },
        (
            Body::Json {
                required: false, ..
            },
            Some(_),
        ) => quote! {
            fn body(&self) -> Result<crate::endpoint::RequestBody, serde_json::Error> {
                match &self.data {
                    Some(data) => super::json_data(data),
                    None => Ok(crate::endpoint::RequestBody::Empty),
                }
            }
        },
        _ => quote! {},
    };
    let paginated = match &types.item {
        Some(item) if operation.paginated() => Some(paginated(
            &name,
            item,
            operation
                .query_params
                .iter()
                .any(|param| param.kind == ParamKind::Limit),
        )),
        _ => None,
    };
    let enums = operation.query_params.iter().filter_map(query_enum);

    Ok(quote! {
        #doc
        #[derive(Clone, Debug, bon::Builder)]
        pub struct #name {
            #( #path_fields )*
            #( #query_fields )*
            #body_field
        }

        impl crate::endpoint::Endpoint for #name {
            type Output = #output;

            const OPERATION_ID: &'static str = #operation_id;
            const METHOD: crate::endpoint::Method = crate::endpoint::Method::#method;
            const ROUTE: &'static str = #route;
            const SCOPE: Option<crate::scopes::Scope> = #scope;
            const IDEMPOTENT: bool = #idempotent;
            #envelope

            #path
            #query
            #body
        }

        #paginated
        #( #enums )*
    })
}

/// A field's identifier, raw when it is a keyword.
pub fn ident(field: &str) -> Ident {
    match field.strip_prefix("r#") {
        Some(raw) => Ident::new_raw(raw, Span::call_site()),
        None => Ident::new(field, Span::call_site()),
    }
}

fn path_field(param: &Param) -> TokenStream {
    let field = ident(&param.field);
    let doc = docs::attributes(&docs::or(
        param.description.as_deref(),
        &format!("The `{}` path parameter.", param.name),
    ));
    let ty = value_type(&param.kind);
    let into = (!matches!(param.kind, ParamKind::Gid(_))).then(|| quote! { , into });
    quote! {
        #doc
        #[builder(start_fn #into)]
        pub #field: #ty,
    }
}

pub fn query_field(param: &Param) -> TokenStream {
    let field = ident(&param.field);
    let ty = value_type(&param.kind);
    let fallback = match &param.kind {
        ParamKind::Limit => "How many objects a page holds.".to_owned(),
        ParamKind::Offset => "Where the page starts: an offset from the previous page.".to_owned(),
        _ => format!("The `{}` query parameter.", param.name),
    };
    let mut lines = match &param.kind {
        ParamKind::OptFields { .. } => {
            vec!["The fields to return. Empty asks for Asana's compact record.".to_owned()]
        }
        _ => docs::or(param.description.as_deref(), &fallback),
    };
    if matches!(param.kind, ParamKind::Date { nullable: true }) {
        lines.push(String::new());
        lines.push("`Some(None)` sends `null`: no date.".to_owned());
    }
    let doc = docs::attributes(&lines);
    let into = matches!(param.kind, ParamKind::Text | ParamKind::UserRef)
        .then(|| quote! { #[builder(into)] });
    if let ParamKind::OptFields { .. } = param.kind {
        quote! { #doc #[builder(default)] pub #field: #ty, }
    } else if param.required {
        quote! { #doc #into pub #field: #ty, }
    } else {
        quote! { #doc #into pub #field: Option<#ty>, }
    }
}

/// The Rust type of a parameter's value.
fn value_type(kind: &ParamKind) -> TokenStream {
    match kind {
        ParamKind::Gid(name) => {
            let name = format_ident!("{name}");
            quote! { crate::ids::#name }
        }
        ParamKind::UserRef => quote! { crate::ids::UserRef },
        ParamKind::Text => quote! { String },
        ParamKind::Bool => quote! { bool },
        ParamKind::Integer => quote! { u32 },
        ParamKind::Limit => quote! { crate::endpoint::PageLimit },
        ParamKind::Offset => quote! { crate::endpoint::Offset },
        ParamKind::Date { nullable: false } => quote! { time::Date },
        ParamKind::Date { nullable: true } => quote! { Option<time::Date> },
        ParamKind::DateTime => quote! { time::OffsetDateTime },
        ParamKind::Enum { type_name, .. } => {
            let name = format_ident!("{type_name}");
            quote! { #name }
        }
        ParamKind::List => quote! { Vec<String> },
        ParamKind::OptFields { family, .. } => {
            let field = format_ident!("{}", crate::naming::field_enum_name(family));
            quote! { crate::fields::Fields<crate::fields::#field> }
        }
    }
}

/// `path()`: the route with each parameter formatted in. GIDs are digits, so they go in as they
/// are; anything else is percent-encoded.
fn path_fn(operation: &Operation<'_>) -> TokenStream {
    if operation.path_params.is_empty() {
        let route = &operation.route;
        return quote! {
            fn path(&self) -> String {
                #route.to_owned()
            }
        };
    }
    let mut template = operation.route.clone();
    for param in &operation.path_params {
        template = template.replace(&format!("{{{}}}", param.name), "{}");
    }
    let arguments = operation.path_params.iter().map(|param| {
        let field = ident(&param.field);
        match param.kind {
            ParamKind::Gid(_) => quote! { self.#field },
            ParamKind::UserRef => quote! { super::path_segment(&self.#field.to_string()) },
            _ => quote! { super::path_segment(&self.#field) },
        }
    });
    quote! {
        fn path(&self) -> String {
            format!(#template, #( #arguments ),*)
        }
    }
}

/// `query()`: the required parameters in specification order, then the optional ones that are
/// set, in specification order. The optional ones go through the `query` helper, so each is one
/// line however many an operation has.
pub fn query_fn(params: &[Param]) -> TokenStream {
    if params.is_empty() {
        return quote! {};
    }
    let is_required =
        |param: &&Param| param.required && !matches!(param.kind, ParamKind::OptFields { .. });
    let required: Vec<TokenStream> = params
        .iter()
        .filter(is_required)
        .map(|param| {
            let name = &param.name;
            let field = ident(&param.field);
            let value = render_value(&param.kind, &quote! { self.#field }, false);
            quote! { (#name.into(), #value) }
        })
        .collect();
    let optional: Vec<TokenStream> = params
        .iter()
        .filter(|param| !is_required(param))
        .map(|param| {
            let name = &param.name;
            let field = ident(&param.field);
            let value = match param.kind {
                ParamKind::OptFields { .. } => quote! { self.#field.to_query_value() },
                ParamKind::Text => quote! { self.#field.clone() },
                ParamKind::Gid(_) | ParamKind::UserRef | ParamKind::Bool | ParamKind::Integer => {
                    quote! { self.#field.as_ref().map(ToString::to_string) }
                }
                ParamKind::Date { nullable: false } => quote! { self.#field.map(super::date) },
                ParamKind::Date { nullable: true } => {
                    quote! { self.#field.map(super::nullable_date) }
                }
                ParamKind::DateTime => quote! { self.#field.map(super::date_time) },
                _ => {
                    let rendered = render_value(&param.kind, &quote! { value }, true);
                    quote! { self.#field.as_ref().map(|value| #rendered) }
                }
            };
            quote! { (#name, #value) }
        })
        .collect();
    let required = if required.is_empty() {
        quote! { Vec::new() }
    } else {
        quote! { vec![ #( #required ),* ] }
    };
    let body = if optional.is_empty() {
        required
    } else {
        quote! { super::query(#required, [ #( #optional ),* ]) }
    };
    quote! {
        fn query(&self) -> Vec<crate::endpoint::QueryPair> {
            #body
        }
    }
}

/// The expression that renders a parameter's value as query text. `by_ref` says `value` is a
/// reference (the closure argument of `Option::as_ref().map`) rather than a field place.
fn render_value(kind: &ParamKind, value: &TokenStream, by_ref: bool) -> TokenStream {
    let copied = if by_ref {
        quote! { *#value }
    } else {
        quote! { #value }
    };
    match kind {
        ParamKind::Gid(_) | ParamKind::UserRef | ParamKind::Bool | ParamKind::Integer => {
            quote! { #value.to_string() }
        }
        ParamKind::Text => quote! { #value.clone() },
        ParamKind::Limit => quote! { #value.get().to_string() },
        ParamKind::Offset => quote! { #value.as_str().to_owned() },
        ParamKind::Date { nullable: false } => quote! { super::date(#copied) },
        ParamKind::Date { nullable: true } => quote! { super::nullable_date(#copied) },
        ParamKind::DateTime => quote! { super::date_time(#copied) },
        ParamKind::Enum { .. } => quote! { #value.as_str().to_owned() },
        ParamKind::List => quote! { #value.join(",") },
        ParamKind::OptFields { .. } => quote! { String::new() },
    }
}

fn paginated(name: &Ident, item: &TokenStream, has_limit: bool) -> TokenStream {
    let set_limit = if has_limit {
        quote! {
            fn set_limit(&mut self, limit: crate::endpoint::PageLimit) {
                self.limit = Some(limit);
            }

            fn limit(&self) -> Option<crate::endpoint::PageLimit> {
                self.limit
            }
        }
    } else {
        quote! {
            /// Does nothing: this operation takes no page size, and Asana decides it.
            fn set_limit(&mut self, _limit: crate::endpoint::PageLimit) {}
        }
    };
    quote! {
        impl crate::endpoint::Paginated for #name {
            type Item = #item;

            fn set_offset(&mut self, offset: Option<crate::endpoint::Offset>) {
                self.offset = offset;
            }

            #set_limit
        }
    }
}

/// A query parameter's closed set of values. It is only ever sent, so it has no unknown variant.
fn query_enum(param: &Param) -> Option<TokenStream> {
    let ParamKind::Enum { type_name, values } = &param.kind else {
        return None;
    };
    let name = format_ident!("{type_name}");
    let doc = docs::attributes(&[format!(
        "The values of the `{}` query parameter.",
        param.name
    )]);
    let variants: Vec<Ident> = values.iter().map(|value| variant(value)).collect();
    let variant_docs = values.iter().map(|value| format!(" `{value}`"));
    Some(quote! {
        #doc
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        #[non_exhaustive]
        pub enum #name {
            #( #[doc = #variant_docs] #variants, )*
        }

        impl #name {
            /// The value as Asana expects it.
            pub const fn as_str(self) -> &'static str {
                match self {
                    #( Self::#variants => #values, )*
                }
            }
        }
    })
}

/// The variant name of an enum value or a field path: `memberships.section.name` gives
/// `MembershipsSectionName`.
pub fn variant(value: &str) -> Ident {
    use heck::ToUpperCamelCase;
    let name = value.to_upper_camel_case();
    if name.starts_with(|first: char| first.is_ascii_digit()) {
        format_ident!("V{name}")
    } else {
        format_ident!("{name}")
    }
}
