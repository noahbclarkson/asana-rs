//! The names both halves of the generator agree on.
//!
//! Asana's specification names a resource's shapes as a family: `TaskCompact` is a subset of
//! `TaskBase`, which is a subset of `TaskResponse`. Because `opt_fields` can widen a compact
//! result or narrow a full one, no shape is ever guaranteed, so the generator emits ONE struct per
//! family (`Task`) with every field optional. Request shapes (`TaskRequest`) stay types of their
//! own. The models half defines the types; the endpoints half refers to them. Both ask this module
//! what a schema is called, so they cannot disagree.

#![allow(dead_code, reason = "used by the two generator halves as they land")]

/// What a component schema becomes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SchemaRole {
    /// One of the read shapes of a resource family: all of them become the struct `family`.
    Read {
        /// The Rust type, for example `Task`.
        family: String,
    },
    /// A request body shape: a type of its own, serialised and never deserialised.
    Write {
        /// The Rust type, for example `TaskRequest`.
        type_name: String,
    },
    /// Anything else: a type of its own.
    Other {
        /// The Rust type.
        type_name: String,
    },
}

impl SchemaRole {
    /// The Rust type name this schema is generated as, in `asana_openapi::models`.
    pub fn type_name(&self) -> &str {
        match self {
            Self::Read { family } => family,
            Self::Write { type_name } | Self::Other { type_name } => type_name,
        }
    }
}

/// Schemas whose generated name is not what the suffix rule gives. Keep it sorted; say why.
const OVERRIDES: &[(&str, &str)] = &[
    // `Error` would shadow the prelude's meaning in every module that imports models.
    ("Error", "ApiError"),
    ("ErrorResponse", "ApiErrorBody"),
];

/// Read shapes whose family the suffix rule gets wrong. Keep it sorted; say why.
const FAMILY_OVERRIDES: &[(&str, &str)] = &[
    // Has no suffix, but is the family `EnumOptionBase` names.
    ("EnumOption", "EnumOption"),
    // Both are `ProjectMembership` shapes; the suffix rule would leave `Compact` or `Normal` on.
    ("PortfolioMembershipCompactResponse", "PortfolioMembership"),
    ("ProjectMembershipCompactResponse", "ProjectMembership"),
    ("ProjectMembershipNormalResponse", "ProjectMembership"),
    // `UserBase` plus `email` and `photo`: still a user, not a `UserBase`.
    ("UserBaseResponse", "User"),
];

/// Request shapes the suffix rule would read as resource families, because they end in `Base`.
const WRITE_OVERRIDES: &[&str] = &["GoalRequestBase", "TaskRequestBase"];

/// What the component schema called `schema` (for example `TaskCompact`) becomes.
pub fn role_of(schema: &str) -> SchemaRole {
    if let Some((_, renamed)) = OVERRIDES.iter().find(|(from, _)| *from == schema) {
        return SchemaRole::Other {
            type_name: (*renamed).to_owned(),
        };
    }
    if let Some((_, family)) = FAMILY_OVERRIDES.iter().find(|(from, _)| *from == schema) {
        return SchemaRole::Read {
            family: (*family).to_owned(),
        };
    }
    if WRITE_OVERRIDES.contains(&schema) {
        return SchemaRole::Write {
            type_name: schema.to_owned(),
        };
    }
    for suffix in ["Compact", "Base", "Response"] {
        if let Some(family) = schema.strip_suffix(suffix)
            && !family.is_empty()
        {
            return SchemaRole::Read {
                family: family.to_owned(),
            };
        }
    }
    if schema.ends_with("Request") {
        return SchemaRole::Write {
            type_name: schema.to_owned(),
        };
    }
    SchemaRole::Other {
        type_name: schema.to_owned(),
    }
}

/// The typed GID for a resource family (`Task` gives `TaskGid`). Whether such a type exists is the
/// models half's decision: it emits one for every family whose schema has a `gid` property, and
/// publishes that set through [`crate::models::gid_families`].
pub fn gid_type_name(family: &str) -> String {
    format!("{family}Gid")
}

/// The field-selection enum for a resource family (`Task` gives `TaskField`).
pub fn field_enum_name(family: &str) -> String {
    format!("{family}Field")
}

/// The module a family's generated code lives in: the snake-cased family, so `TaskTemplate` is in
/// `models/task_template.rs`. Small families may share a file; the models half decides and the
/// endpoints half never needs to know, because everything is re-exported from `models`.
pub fn module_name(family: &str) -> String {
    use heck::ToSnakeCase;
    family.to_snake_case()
}
