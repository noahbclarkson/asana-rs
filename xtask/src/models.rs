//! The models half: schemas to structs, the typed GID list and the serde helpers.
//!
//! The work happens in three steps, each in its own module:
//!
//! 1. [`classify`] decides what every component schema is: which resource family it belongs to
//!    (`TaskCompact`, `TaskBase` and `TaskResponse` are all `Task`), which families have a typed
//!    GID, and in which directions each schema is used.
//! 2. [`build`] turns schemas into the generator's [`ir`]: one struct per family with the union of
//!    its shapes' properties, request structs without their read-only properties, string enums,
//!    unions and the inline objects they hold.
//! 3. [`render`] and [`layout`] turn that into Rust, one file per family.
//!
//! The types are written with `quote` rather than through `typify`: what `typify` would do for us
//! (`allOf` merging) is a few lines over raw JSON, while everything this crate needs on top of it
//! (one struct per family, typed GIDs, `time` and `Decimal` scalars, enums that keep unknown
//! values, unions told apart by `resource_type`, `bon` builders) would have meant rewriting its
//! output anyway.

mod build;
mod classify;
mod docs;
mod helpers;
mod ir;
mod layout;
mod render;
mod schema;
#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet};

use crate::GenResult;
use crate::emit::GeneratedFile;
use crate::spec::Spec;

/// Whether a type is read from a response or written in a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Direction {
    /// Deserialised from a response.
    Read,
    /// Serialised into a request.
    Write,
}

/// What the models half works out about a specification: built once per [`Spec`] (see
/// [`Spec::analysis`]) so that every call names types the same way.
#[derive(Debug)]
pub struct Analysis {
    classes: classify::Classes,
    items: BTreeMap<String, ir::Item>,
    memo: BTreeMap<(String, Direction), ir::Ty>,
}

impl Analysis {
    /// Classifies and builds every schema in `raw`.
    pub fn build(raw: &serde_json::Value) -> GenResult<Self> {
        let classes = classify::Classes::build(raw)?;
        let mut builder = build::Builder::new(raw, &classes)?;
        builder.build_all()?;
        let build::Builder { items, memo, .. } = builder;
        Ok(Self {
            classes,
            items,
            memo,
        })
    }
}

/// Generates `models/**`, `ids.rs` and the top-level `mod.rs` of the generated directory.
pub fn generate(spec: &Spec) -> GenResult<Vec<GeneratedFile>> {
    let analysis = spec.analysis()?;
    // The families the endpoints half emits an `opt_fields` enum for get a `HasFields` impl.
    let field_families = crate::fields::field_families(spec)?;
    layout::files(analysis, &field_families)
}

/// A Rust type for a schema, and anything that must be emitted for it to exist.
pub struct TypeRef {
    /// The type as written from inside `asana_openapi::generated`, for example
    /// `crate::models::Task`, `Vec<crate::models::Story>` or `crate::ids::TaskGid`.
    pub ty: proc_macro2::TokenStream,
    /// Item definitions the caller must emit beside its own code, when the schema was an inline
    /// object with no name of its own. Empty for a `$ref` to a component schema.
    pub items: Vec<proc_macro2::TokenStream>,
}

/// The Rust type for a schema as it appears in an operation (a response's `data`, a request
/// body's `data`, a query parameter). `name_hint` names an inline object's type, in PascalCase,
/// for example `GetTaskCountsOfProjectData`; an inline object's own inline fields are named after
/// it (`GetTaskCountsOfProjectDataSomething`), and an inline string enum is named `name_hint`.
/// Those inline types are returned in [`TypeRef::items`] and written without a path, both in
/// [`TypeRef::ty`] and inside each other, so the caller emits them beside the code that uses them;
/// everything else is a full path (`crate::models::Task`, `crate::ids::TaskGid`).
///
/// Dates and decimals come back as `time::Date` and `rust_decimal::Decimal`; a query parameter of
/// those types is the caller's to format.
pub fn rust_type(
    spec: &Spec,
    schema: &openapiv3::ReferenceOr<openapiv3::Schema>,
    name_hint: &str,
    direction: Direction,
) -> GenResult<TypeRef> {
    let analysis = spec.analysis()?;
    let json = serde_json::to_value(schema)
        .map_err(|error| format!("`{name_hint}`'s schema cannot be read as JSON: {error}"))?;
    let mut builder = build::Builder::on_top_of(
        &spec.raw,
        &analysis.classes,
        &analysis.items,
        &analysis.memo,
    )?;
    let ty = builder.operation_type(&json, name_hint, direction)?;
    let field_families = BTreeSet::new();
    let local: BTreeSet<String> = builder.items.keys().cloned().collect();
    let mut renderer = render::Renderer {
        field_families: &field_families,
        local: &local,
        used: BTreeSet::new(),
    };
    let items = builder
        .items
        .values()
        .map(|item| renderer.item(item))
        .collect::<GenResult<Vec<_>>>()?;
    if let Some(helper) = renderer
        .used
        .iter()
        .find(|helper| !analysis_uses(analysis, **helper))
    {
        return Err(format!(
            "`{name_hint}` needs the serde helper {helper:?}, which no model uses, so it is not generated"
        ));
    }
    Ok(TypeRef {
        ty: render::ty_tokens(&ty, &local),
        items,
    })
}

/// Whether the models use a serde helper, so that it exists for an operation's inline type too.
fn analysis_uses(analysis: &Analysis, helper: helpers::Helper) -> bool {
    analysis.items.values().any(|item| match &item.kind {
        ir::ItemKind::Struct { direction, fields } => fields.iter().any(|field| {
            helpers::Helper::for_field(&field.ty, *direction, field.clearable) == Some(helper)
        }),
        _ => false,
    })
}

/// The resource families that have a typed GID, for the endpoints half.
pub fn gid_families(spec: &Spec) -> GenResult<Vec<String>> {
    Ok(spec
        .analysis()?
        .classes
        .gid_families
        .keys()
        .cloned()
        .collect())
}
