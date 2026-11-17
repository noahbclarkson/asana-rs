//! What each component schema becomes, decided once for the whole specification: its family or
//! type name, which families have a typed GID and with what `resource_type`, and in which
//! directions (read from a response, written in a request) each schema is ever used.

use std::collections::{BTreeMap, BTreeSet};

use heck::ToSnakeCase;
use serde_json::Value;

use super::Direction;
use super::schema;
use crate::GenResult;
use crate::naming::{SchemaRole, role_of};

/// Families whose `gid` names more than one kind of object, so it stays a plain string. A member
/// is a user or a team; the others are generic references to anything.
const POLYMORPHIC: &[&str] = &[
    "AccessRequestTargetId",
    "AsanaNamedResource",
    "AsanaResource",
    "AuditLogEventActor",
    "AuditLogEventResource",
    "Member",
];

/// Resources with a GID that no schema of theirs declares, because the family is a union of
/// kinds or its schema holds only an action's result; operations still take them in paths
/// (`/memberships/{membership_gid}`, `/rule_triggers/{rule_trigger_gid}/run`).
const EXTRA_GIDS: &[(&str, &str)] = &[
    ("Membership", "membership"),
    ("RuleTrigger", "rule_trigger"),
];

/// The GID types the rest of the workspace relies on by name; generation fails if one goes.
const REQUIRED_GIDS: &[&str] = &[
    "Attachment",
    "CustomField",
    "EnumOption",
    "Goal",
    "Portfolio",
    "Project",
    "Section",
    "Story",
    "Tag",
    "Task",
    "Team",
    "User",
    "Webhook",
    "Workspace",
];

/// The decisions about every component schema.
#[derive(Debug)]
pub struct Classes {
    /// Each read family and its schemas, the compact shape first.
    pub families: BTreeMap<String, Vec<String>>,
    /// Families that are a union (`anyOf`) of other families rather than an object.
    pub unions: BTreeSet<String>,
    /// For each family with a typed GID, Asana's `resource_type` for it.
    pub gid_families: BTreeMap<String, String>,
    /// Families that use another family's GID type, and that family.
    pub gid_aliases: BTreeMap<String, String>,
    /// For each `resource_type` that names exactly one family, that family.
    pub family_of_resource_type: BTreeMap<String, String>,
    /// Every component schema and the directions it is used in.
    pub reached: BTreeSet<(String, Direction)>,
}

impl Classes {
    /// Classifies every component schema of `raw`.
    pub fn build(raw: &Value) -> GenResult<Self> {
        let components = schema::components(raw)?;
        let mut families: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut unions = BTreeSet::new();
        for (name, body) in components {
            if let SchemaRole::Read { family } = role_of(name) {
                if !schema::one_of(body).is_empty() {
                    unions.insert(family.clone());
                }
                families.entry(family).or_default().push(name.clone());
            }
        }
        for members in families.values_mut() {
            members.sort_by_key(|name| shape_rank(name));
        }
        let mut gid_families = BTreeMap::new();
        for (family, members) in &families {
            if unions.contains(family) || POLYMORPHIC.contains(&family.as_str()) {
                continue;
            }
            if let Some(resource_type) = family_gid(raw, family, members)? {
                gid_families.insert(family.clone(), resource_type);
            }
        }
        for (name, body) in components {
            if let SchemaRole::Other { type_name } = role_of(name)
                && !POLYMORPHIC.contains(&type_name.as_str())
                && schema::one_of(body).is_empty()
                && let Some(resource_type) =
                    family_gid(raw, &type_name, std::slice::from_ref(name))?
            {
                gid_families.insert(type_name, resource_type);
            }
        }
        for (family, resource_type) in EXTRA_GIDS {
            if !families.contains_key(*family) {
                return Err(format!(
                    "`{family}` is no longer a family; revisit EXTRA_GIDS"
                ));
            }
            gid_families.insert((*family).to_owned(), (*resource_type).to_owned());
        }
        // A family whose `resource_type` is another family's name shares that family's GID: a
        // placeholder's rate is a rate, and a goal membership is a membership.
        let mut gid_aliases = BTreeMap::new();
        for (family, resource_type) in &gid_families {
            if let Some(canonical) = gid_families
                .iter()
                .find(|(other, literal)| {
                    *other != family
                        && *literal == resource_type
                        && other.to_snake_case() == **literal
                })
                .map(|(other, _)| other.clone())
            {
                gid_aliases.insert(family.clone(), canonical);
            }
        }
        for alias in gid_aliases.keys() {
            gid_families.remove(alias);
        }
        for required in REQUIRED_GIDS {
            if !gid_families.contains_key(*required) {
                return Err(format!(
                    "the family `{required}` no longer has a GID; other crates rely on `{required}Gid`"
                ));
            }
        }
        let mut seen: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for (family, resource_type) in &gid_families {
            seen.entry(resource_type).or_default().push(family);
        }
        let family_of_resource_type = seen
            .into_iter()
            .filter(|(_, families)| families.len() == 1)
            .map(|(resource_type, families)| (resource_type.to_owned(), families[0].to_owned()))
            .collect();
        let reached = reach(raw)?;
        Ok(Self {
            families,
            unions,
            gid_families,
            gid_aliases,
            family_of_resource_type,
            reached,
        })
    }

    /// The family whose GID type a `gid` of `family` has, if it has one.
    pub fn gid_type(&self, family: &str) -> Option<String> {
        if self.gid_families.contains_key(family) {
            return Some(family.to_owned());
        }
        self.gid_aliases.get(family).cloned()
    }

    /// Whether a component schema is ever read from a response.
    pub fn is_read(&self, schema: &str) -> bool {
        self.reached.contains(&(schema.to_owned(), Direction::Read))
    }
}

/// Orders a family's shapes from the smallest: compact, base, then the full response.
fn shape_rank(name: &str) -> (u8, String) {
    let rank = if name.contains("Compact") {
        0
    } else if name.ends_with("Base") {
        1
    } else {
        2
    };
    (rank, name.to_owned())
}

/// The `resource_type` of a family with its own `gid`, or `None` when it has none. Asana's
/// examples are copied between schemas, so an example of `task` or `user` on a family that is
/// neither is not trusted, and the snake-cased family name is used instead.
fn family_gid(raw: &Value, family: &str, members: &[String]) -> GenResult<Option<String>> {
    let mut has_gid = false;
    let mut example = None;
    for member in members {
        let body = schema::component(raw, member)?;
        for (name, property) in schema::flatten(raw, body)? {
            match name {
                "gid" => has_gid = true,
                "resource_type" if example.is_none() => example = schema::literal(property),
                _ => {}
            }
        }
    }
    if !has_gid {
        return Ok(None);
    }
    let fallback = family.to_snake_case();
    Ok(Some(match example {
        Some(literal @ ("task" | "user")) if literal != fallback => fallback,
        Some(literal) => literal.to_owned(),
        None => fallback,
    }))
}

/// Every component schema reachable from the read families and from the operations' responses
/// (read), JSON request bodies and parameters (written), with the directions it is reached in.
/// A written schema's read-only properties are not followed: they are never sent.
fn reach(raw: &Value) -> GenResult<BTreeSet<(String, Direction)>> {
    let mut reached = BTreeSet::new();
    for (name, _) in schema::components(raw)? {
        if matches!(role_of(name), SchemaRole::Read { .. }) {
            visit_component(raw, name, Direction::Read, &mut reached)?;
        }
    }
    let paths = raw
        .get("paths")
        .and_then(Value::as_object)
        .ok_or("the specification has no paths")?;
    for operations in paths.values() {
        let Some(operations) = operations.as_object() else {
            continue;
        };
        for (method, operation) in operations {
            if method == "parameters" {
                for parameter in operation.as_array().into_iter().flatten() {
                    visit_parameter(raw, parameter, &mut reached)?;
                }
                continue;
            }
            for parameter in operation
                .get("parameters")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                visit_parameter(raw, parameter, &mut reached)?;
            }
            if let Some(body) = operation.pointer("/requestBody/content/application~1json/schema") {
                visit(raw, body, Direction::Write, &mut reached)?;
            }
            for response in operation
                .get("responses")
                .and_then(Value::as_object)
                .into_iter()
                .flat_map(|responses| responses.values())
            {
                let response = resolve_named(raw, response, "responses")?;
                if let Some(body) = response.pointer("/content/application~1json/schema") {
                    visit(raw, body, Direction::Read, &mut reached)?;
                }
            }
        }
    }
    Ok(reached)
}

fn visit_parameter(
    raw: &Value,
    parameter: &Value,
    reached: &mut BTreeSet<(String, Direction)>,
) -> GenResult<()> {
    let parameter = resolve_named(raw, parameter, "parameters")?;
    if let Some(body) = parameter.get("schema") {
        visit(raw, body, Direction::Write, reached)?;
    }
    Ok(())
}

/// Follows a `$ref` to `#/components/{section}/...`.
fn resolve_named<'a>(raw: &'a Value, value: &'a Value, section: &str) -> GenResult<&'a Value> {
    let Some(reference) = value.get("$ref").and_then(Value::as_str) else {
        return Ok(value);
    };
    let prefix = format!("#/components/{section}/");
    let name = reference
        .strip_prefix(&prefix)
        .ok_or_else(|| format!("`{reference}` is not a reference to a {section} entry"))?;
    raw.get("components")
        .and_then(|components| components.get(section))
        .and_then(|entries| entries.get(name))
        .ok_or_else(|| format!("`{reference}` names nothing"))
}

fn visit_component(
    raw: &Value,
    name: &str,
    direction: Direction,
    reached: &mut BTreeSet<(String, Direction)>,
) -> GenResult<()> {
    if !reached.insert((name.to_owned(), direction)) {
        return Ok(());
    }
    let body = schema::component(raw, name)?;
    visit_definition(raw, body, direction, reached)
}

/// Walks a schema's own definition: a reference or an `allOf` part here is inherited properties,
/// not a use of the schema it names.
fn visit_definition(
    raw: &Value,
    body: &Value,
    direction: Direction,
    reached: &mut BTreeSet<(String, Direction)>,
) -> GenResult<()> {
    if let Some(name) = schema::ref_name(body) {
        return visit_definition(raw, schema::component(raw, name)?, direction, reached);
    }
    for part in schema::all_of(body) {
        visit_definition(raw, part, direction, reached)?;
    }
    visit_parts(raw, body, direction, reached)
}

/// Walks a schema where it is used: a (wrapped) reference is a use of the schema it names.
fn visit(
    raw: &Value,
    body: &Value,
    direction: Direction,
    reached: &mut BTreeSet<(String, Direction)>,
) -> GenResult<()> {
    if let Some(name) = schema::wrapped_ref(body) {
        return visit_component(raw, name, direction, reached);
    }
    for part in schema::all_of(body) {
        visit_definition(raw, part, direction, reached)?;
    }
    visit_parts(raw, body, direction, reached)
}

fn visit_parts(
    raw: &Value,
    body: &Value,
    direction: Direction,
    reached: &mut BTreeSet<(String, Direction)>,
) -> GenResult<()> {
    for part in schema::one_of(body) {
        visit(raw, part, direction, reached)?;
    }
    if let Some(items) = body.get("items") {
        visit(raw, items, direction, reached)?;
    }
    if let Some(additional) = body.get("additionalProperties")
        && additional.is_object()
    {
        visit(raw, additional, direction, reached)?;
    }
    if let Some(properties) = body.get("properties").and_then(Value::as_object) {
        for property in properties.values() {
            if direction == Direction::Write && schema::read_only(property) {
                continue;
            }
            visit(raw, property, direction, reached)?;
        }
    }
    Ok(())
}
