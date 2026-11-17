//! The endpoints half: operations to descriptors, `opt_fields` enums and the scope table.

use crate::GenResult;
use crate::emit::GeneratedFile;
use crate::spec::Spec;

/// Generates `endpoints/**`, `fields/**` and `scopes.rs`.
pub fn generate(_spec: &Spec) -> GenResult<Vec<GeneratedFile>> {
    Err("the endpoints generator is not written yet".to_owned())
}
