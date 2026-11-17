//! Reading OpenAPI 3.0 schemas as raw JSON: references, `allOf` chains and annotations.
//!
//! The generator works on `serde_json::Value` rather than `openapiv3`'s types because Asana's
//! schemas lean on shapes those types make awkward: `allOf: [{$ref}, {nullable: true}]` to mark a
//! reference nullable, `readOnly` inside an `allOf` part, and `additionalProperties: true`.

use serde_json::{Map, Value};

use crate::GenResult;

/// The prefix of a reference to a component schema.
const SCHEMA_REF: &str = "#/components/schemas/";

/// The component schemas of the document.
pub fn components(raw: &Value) -> GenResult<&Map<String, Value>> {
    raw.pointer("/components/schemas")
        .and_then(Value::as_object)
        .ok_or_else(|| "the specification has no components.schemas".to_owned())
}

/// The component a `$ref` names, if `schema` is a reference.
pub fn ref_name(schema: &Value) -> Option<&str> {
    schema
        .get("$ref")
        .and_then(Value::as_str)
        .and_then(|reference| reference.strip_prefix(SCHEMA_REF))
}

/// Looks a component schema up by name.
pub fn component<'a>(raw: &'a Value, name: &str) -> GenResult<&'a Value> {
    components(raw)?
        .get(name)
        .ok_or_else(|| format!("a reference names the missing schema `{name}`"))
}

/// The parts of an `allOf`, or nothing.
pub fn all_of(schema: &Value) -> &[Value] {
    schema
        .get("allOf")
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

/// The members of a `oneOf` or `anyOf`, or nothing.
pub fn one_of(schema: &Value) -> &[Value] {
    schema
        .get("oneOf")
        .or_else(|| schema.get("anyOf"))
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

/// Whether an `allOf` part only annotates the others (`nullable`, `readOnly`, a description, a bare
/// `type: object`) rather than adding properties of its own.
fn is_annotation(part: &Value) -> bool {
    ref_name(part).is_none()
        && part.get("properties").is_none()
        && part.get("allOf").is_none()
        && part.get("additionalProperties").is_none()
        && one_of(part).is_empty()
        && part
            .get("type")
            .and_then(Value::as_str)
            .is_none_or(|kind| kind == "object")
}

/// When `schema` is one reference wrapped in annotations (`allOf: [{$ref}, {nullable: true}]`),
/// the component it names.
pub fn wrapped_ref(schema: &Value) -> Option<&str> {
    if let Some(name) = ref_name(schema) {
        return Some(name);
    }
    let parts = all_of(schema);
    let mut refs = parts.iter().filter_map(ref_name);
    let first = refs.next()?;
    if refs.next().is_some() {
        return None;
    }
    parts
        .iter()
        .filter(|part| ref_name(part).is_none())
        .all(is_annotation)
        .then_some(first)
}

/// Whether `key` is `true` on the schema or on any `allOf` part that only annotates it.
fn flag(schema: &Value, key: &str) -> bool {
    schema.get(key).and_then(Value::as_bool) == Some(true)
        || all_of(schema)
            .iter()
            .filter(|part| is_annotation(part))
            .any(|part| part.get(key).and_then(Value::as_bool) == Some(true))
}

/// Whether the property is read-only: never sent in a request.
pub fn read_only(schema: &Value) -> bool {
    flag(schema, "readOnly")
}

/// Whether the property may be `null`.
pub fn nullable(schema: &Value) -> bool {
    flag(schema, "nullable")
}

/// The property's description, from the schema itself or the annotations around a reference.
pub fn description(schema: &Value) -> Option<&str> {
    schema
        .get("description")
        .and_then(Value::as_str)
        .or_else(|| {
            all_of(schema)
                .iter()
                .find_map(|part| part.get("description").and_then(Value::as_str))
        })
        .filter(|text| !text.trim().is_empty())
}

/// The properties of a schema with its `allOf` chain resolved: inherited properties first, a later
/// definition of the same name replacing an earlier one in place.
///
/// Within one schema the properties are taken in name order, sorted here rather than left to
/// `serde_json`'s map, whose order depends on whether some crate in the build switches on its
/// `preserve_order` feature. The generated field order is the same either way.
pub fn flatten<'a>(raw: &'a Value, schema: &'a Value) -> GenResult<Vec<(&'a str, &'a Value)>> {
    let mut properties = Vec::new();
    collect(raw, schema, &mut properties, 0)?;
    Ok(properties)
}

fn collect<'a>(
    raw: &'a Value,
    schema: &'a Value,
    into: &mut Vec<(&'a str, &'a Value)>,
    depth: usize,
) -> GenResult<()> {
    if depth > 32 {
        return Err("an allOf chain is cyclic".to_owned());
    }
    if let Some(name) = ref_name(schema) {
        return collect(raw, component(raw, name)?, into, depth + 1);
    }
    for part in all_of(schema) {
        collect(raw, part, into, depth + 1)?;
    }
    if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
        let mut properties: Vec<_> = properties.iter().collect();
        properties.sort_by_key(|(name, _)| name.as_str());
        for (name, property) in properties {
            match into.iter_mut().find(|(existing, _)| existing == name) {
                Some(slot) => slot.1 = property,
                None => into.push((name, property)),
            }
        }
    }
    Ok(())
}

/// The first description along a schema's `allOf` chain, the schema's own first.
pub fn chain_description<'a>(raw: &'a Value, schema: &'a Value) -> Option<&'a str> {
    if let Some(text) = schema
        .get("description")
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
    {
        return Some(text);
    }
    if let Some(name) = ref_name(schema) {
        return component(raw, name)
            .ok()
            .and_then(|target| chain_description(raw, target));
    }
    all_of(schema)
        .iter()
        .find_map(|part| chain_description(raw, part))
}

/// The example or single enum value of a string property, the way Asana marks `resource_type`.
pub fn literal(schema: &Value) -> Option<&str> {
    if let Some([only]) = schema
        .get("enum")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
    {
        return only.as_str();
    }
    schema.get("example").and_then(Value::as_str)
}

/// The `type` keyword.
pub fn kind(schema: &Value) -> Option<&str> {
    schema.get("type").and_then(Value::as_str)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_reference_wrapped_in_nullable_is_still_the_reference() {
        let schema = json!({"allOf": [{"$ref": "#/components/schemas/UserCompact"}, {"nullable": true, "readOnly": true}]});
        assert_eq!(wrapped_ref(&schema), Some("UserCompact"));
        assert!(nullable(&schema));
        assert!(read_only(&schema));
        let merged = json!({"allOf": [{"$ref": "#/components/schemas/A"}, {"type": "object", "properties": {"x": {}}}]});
        assert_eq!(wrapped_ref(&merged), None);
    }

    #[test]
    fn flattening_keeps_the_first_position_and_the_last_definition() {
        let raw = json!({"components": {"schemas": {
            "ACompact": {"properties": {"gid": {"type": "string"}, "name": {"type": "string"}}},
            "AResponse": {"allOf": [
                {"$ref": "#/components/schemas/ACompact"},
                {"properties": {"name": {"type": "integer"}, "extra": {}}}
            ]}
        }}});
        let schema = component(&raw, "AResponse").unwrap();
        let properties = flatten(&raw, schema).unwrap();
        let names: Vec<_> = properties.iter().map(|(name, _)| *name).collect();
        assert_eq!(names, ["gid", "name", "extra"]);
        assert_eq!(properties[1].1["type"], "integer");
    }

    #[test]
    fn each_schemas_properties_come_in_name_order_whatever_the_map_keeps() {
        // Written out of order. Without `preserve_order` the map sorts them already; with it, it
        // would keep this order, and `flatten` must still give name order.
        let raw = json!({"components": {"schemas": {
            "ACompact": {"properties": {"zeta": {}, "gid": {}, "alpha": {}}},
            "AResponse": {"allOf": [
                {"$ref": "#/components/schemas/ACompact"},
                {"properties": {"omega": {}, "beta": {}}}
            ]}
        }}});
        let schema = component(&raw, "AResponse").unwrap();
        let names: Vec<_> = flatten(&raw, schema)
            .unwrap()
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        assert_eq!(names, ["alpha", "gid", "zeta", "beta", "omega"]);
    }
}
