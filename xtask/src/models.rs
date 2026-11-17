//! The models half: schemas to structs, the typed GID list and the serde helpers.

use crate::GenResult;
use crate::emit::GeneratedFile;
use crate::spec::Spec;

/// Generates `models/**`, `ids.rs` and the top-level `mod.rs` of the generated directory.
pub fn generate(_spec: &Spec) -> GenResult<Vec<GeneratedFile>> {
    Err("the models generator is not written yet".to_owned())
}

/// A Rust type for a schema, and anything that must be emitted for it to exist.
#[allow(dead_code, reason = "used by the endpoints half once both land")]
pub struct TypeRef {
    /// The type as written from inside `asana_openapi::generated`, for example
    /// `crate::models::Task`, `Vec<crate::models::Story>` or `crate::ids::TaskGid`.
    pub ty: proc_macro2::TokenStream,
    /// Item definitions the caller must emit beside its own code, when the schema was an inline
    /// object with no name of its own. Empty for a `$ref` to a component schema.
    pub items: Vec<proc_macro2::TokenStream>,
}

/// Whether a type is read from a response or written in a request.
#[allow(dead_code, reason = "used by the endpoints half once both land")]
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Deserialised from a response.
    Read,
    /// Serialised into a request.
    Write,
}

/// The Rust type for a schema as it appears in an operation (a response's `data`, a request
/// body's `data`, a query parameter). `name_hint` names an inline object's type, in PascalCase,
/// for example `GetTaskCountsOfProjectData`.
#[allow(dead_code, reason = "used by the endpoints half once both land")]
pub fn rust_type(
    _spec: &Spec,
    _schema: &openapiv3::ReferenceOr<openapiv3::Schema>,
    _name_hint: &str,
    _direction: Direction,
) -> GenResult<TypeRef> {
    Err("the models generator is not written yet".to_owned())
}

/// The resource families that have a typed GID, for the endpoints half.
#[allow(dead_code, reason = "used by the endpoints half once both land")]
pub fn gid_families(_spec: &Spec) -> GenResult<Vec<String>> {
    Err("the models generator is not written yet".to_owned())
}
