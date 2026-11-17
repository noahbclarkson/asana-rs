//! Applies an [OpenAPI Overlay 1.0](https://spec.openapis.org/overlay/v1.0.0.html) document to the
//! specification before anything reads it.
//!
//! Only the subset the overlays in `spec/overlays/` need is supported: a `target` is a path from the
//! root, `$` followed by `.name`, `['name']` or `[index]` steps (no wildcards or filters), and an
//! action either `remove`s the node it names or merges its `update` into it (objects merge key by
//! key, recursively; anything else is replaced).
//!
//! An overlay that upstream has made stale must fail rather than apply silently, so three things
//! are errors: a target that names nothing; an `update` that changes nothing (upstream already
//! says what the overlay says); and a node that does not match the action's `x-expect`, an object
//! of keys the target must hold with those values (`null`: must not hold the key at all). An
//! action states `x-expect` where upstream could fix or move the thing in a way that still names
//! a node, such as defining a property the overlay adds, or reordering an `allOf` an `[index]`
//! step walks.

use serde_json::Value;

use crate::GenResult;

/// Applies every action of `overlay` to `document`, in order.
pub fn apply(document: &mut Value, overlay: &Value) -> GenResult<()> {
    let actions = overlay
        .get("actions")
        .and_then(Value::as_array)
        .ok_or("an overlay needs an `actions` list")?;
    for (index, action) in actions.iter().enumerate() {
        apply_action(document, action).map_err(|error| format!("action {index}: {error}"))?;
    }
    Ok(())
}

fn apply_action(document: &mut Value, action: &Value) -> GenResult<()> {
    let target = action
        .get("target")
        .and_then(Value::as_str)
        .ok_or("an action needs a `target`")?;
    let steps = parse_target(target)?;
    if let Some(expected) = action.get("x-expect") {
        let node = resolve(document, &steps)
            .ok_or_else(|| format!("`{target}` names nothing in the specification"))?;
        check_expected(node, expected).map_err(|error| format!("`{target}`: {error}"))?;
    }
    if action.get("remove").and_then(Value::as_bool) == Some(true) {
        let (last, parents) = steps
            .split_last()
            .ok_or_else(|| format!("`{target}` cannot remove the whole document"))?;
        let parent = resolve(document, parents)
            .ok_or_else(|| format!("`{target}` names nothing in the specification"))?;
        let removed = match (parent, last) {
            (Value::Object(object), Step::Key(key)) => object.remove(key).is_some(),
            (Value::Array(items), Step::Index(index)) if *index < items.len() => {
                items.remove(*index);
                true
            }
            _ => false,
        };
        if !removed {
            return Err(format!("`{target}` names nothing in the specification"));
        }
        return Ok(());
    }
    let update = action
        .get("update")
        .ok_or_else(|| format!("the action on `{target}` has neither `update` nor `remove`"))?;
    let node = resolve(document, &steps)
        .ok_or_else(|| format!("`{target}` names nothing in the specification"))?;
    let before = node.clone();
    merge(node, update);
    if *node == before {
        return Err(format!(
            "the update to `{target}` changes nothing: upstream already says this, so the \
             overlay may be stale"
        ));
    }
    Ok(())
}

/// Checks `node` against an action's `x-expect`: each key holds the value given, or is absent
/// where the value is `null`.
fn check_expected(node: &Value, expected: &Value) -> Result<(), String> {
    let expected = expected
        .as_object()
        .ok_or("`x-expect` must be an object of keys and the values they hold")?;
    let object = node
        .as_object()
        .ok_or("the target is not an object, as `x-expect` expects")?;
    for (key, value) in expected {
        let actual = object.get(key);
        let matches = match value {
            Value::Null => actual.is_none(),
            value => actual == Some(value),
        };
        if !matches {
            return Err(format!(
                "`{key}` is not as `x-expect` says ({}): upstream may have changed what the \
                 overlay corrects",
                if value.is_null() {
                    "absent".to_owned()
                } else {
                    value.to_string()
                }
            ));
        }
    }
    Ok(())
}

/// One step of a target path.
#[derive(Debug, PartialEq, Eq)]
enum Step {
    Key(String),
    Index(usize),
}

fn parse_target(target: &str) -> GenResult<Vec<Step>> {
    let rest = target
        .strip_prefix('$')
        .ok_or_else(|| format!("`{target}` does not start at `$`"))?;
    let mut steps = Vec::new();
    let mut chars = rest.chars().peekable();
    while let Some(next) = chars.next() {
        match next {
            '.' => {
                let mut key = String::new();
                while let Some(&c) = chars.peek() {
                    if c == '.' || c == '[' {
                        break;
                    }
                    key.push(c);
                    chars.next();
                }
                if key.is_empty() || key == "*" {
                    return Err(format!("`{target}` has an empty or wildcard step"));
                }
                steps.push(Step::Key(key));
            }
            '[' => {
                let mut inner = String::new();
                for c in chars.by_ref() {
                    if c == ']' {
                        break;
                    }
                    inner.push(c);
                }
                let quoted = inner
                    .strip_prefix('\'')
                    .and_then(|rest| rest.strip_suffix('\''))
                    .or_else(|| {
                        inner
                            .strip_prefix('"')
                            .and_then(|rest| rest.strip_suffix('"'))
                    });
                match quoted {
                    Some(key) => steps.push(Step::Key(key.to_owned())),
                    None => steps.push(Step::Index(inner.parse().map_err(|_| {
                        format!(
                            "`{target}` has a step `[{inner}]` that is neither a key nor an index"
                        )
                    })?)),
                }
            }
            other => return Err(format!("`{target}` has an unexpected `{other}`")),
        }
    }
    Ok(steps)
}

fn resolve<'a>(mut node: &'a mut Value, steps: &[Step]) -> Option<&'a mut Value> {
    for step in steps {
        node = match (node, step) {
            (Value::Object(object), Step::Key(key)) => object.get_mut(key)?,
            (Value::Array(items), Step::Index(index)) => items.get_mut(*index)?,
            _ => return None,
        };
    }
    Some(node)
}

/// Merges `update` into `node` as Overlay 1.0 defines it: objects key by key, recursively;
/// anything else replaces what was there.
fn merge(node: &mut Value, update: &Value) {
    match (node, update) {
        (Value::Object(object), Value::Object(changes)) => {
            for (key, change) in changes {
                match object.get_mut(key) {
                    Some(existing) => merge(existing, change),
                    None => {
                        object.insert(key.clone(), change.clone());
                    }
                }
            }
        }
        (node, update) => *node = update.clone(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn targets_parse_names_quoted_keys_and_indexes() {
        assert_eq!(
            parse_target("$.a['b.c'][2].d").unwrap(),
            vec![
                Step::Key("a".into()),
                Step::Key("b.c".into()),
                Step::Index(2),
                Step::Key("d".into())
            ]
        );
        assert!(parse_target("a.b").is_err());
        assert!(parse_target("$.a.*").is_err());
    }

    #[test]
    fn update_merges_and_remove_deletes() {
        let mut document = json!({"s": {"p": {"type": "string", "example": "1,2"}, "q": [1, 2]}});
        let overlay = json!({"actions": [
            {"target": "$.s.p.example", "remove": true},
            {"target": "$.s.p", "update": {"type": "array", "items": {"type": "string"}}},
            {"target": "$.s.q[0]", "remove": true}
        ]});
        apply(&mut document, &overlay).unwrap();
        assert_eq!(
            document,
            json!({"s": {"p": {"type": "array", "items": {"type": "string"}}, "q": [2]}})
        );
    }

    #[test]
    fn an_update_upstream_already_made_is_an_error() {
        let mut document = json!({"s": {"p": {"type": "string", "format": "date"}}});
        let overlay = json!({"actions": [{"target": "$.s.p", "update": {"format": "date"}}]});
        let error = apply(&mut document, &overlay).unwrap_err();
        assert!(error.to_string().contains("changes nothing"), "{error}");
    }

    #[test]
    fn a_node_that_is_not_as_expected_is_an_error() {
        // Upstream now defines the property the overlay would add.
        let mut document = json!({"s": {"properties": {"memberships": {"type": "object"}}}});
        let overlay = json!({"actions": [{
            "target": "$.s.properties",
            "x-expect": {"memberships": null},
            "update": {"memberships": {"type": "array"}}
        }]});
        let error = apply(&mut document, &overlay).unwrap_err();
        assert!(error.to_string().contains("memberships"), "{error}");

        // An `allOf` reordered so that `[1]` is the reference, not the inline part.
        let mut document = json!({"p": {"allOf": [
            {"type": "object", "nullable": true},
            {"$ref": "#/components/schemas/Status"}
        ]}});
        let overlay = json!({"actions": [{
            "target": "$.p.allOf[1]",
            "x-expect": {"type": "object", "readOnly": null},
            "update": {"readOnly": true}
        }]});
        assert!(apply(&mut document, &overlay).is_err());

        // As expected, it applies.
        let mut document = json!({"p": {"allOf": [
            {"$ref": "#/components/schemas/Status"},
            {"type": "object", "nullable": true}
        ]}});
        apply(&mut document, &overlay).unwrap();
        assert_eq!(document["p"]["allOf"][1]["readOnly"], json!(true));
    }

    #[test]
    fn a_target_that_names_nothing_is_an_error() {
        let mut document = json!({"s": {}});
        let overlay = json!({"actions": [{"target": "$.s.missing", "update": {"x": 1}}]});
        assert!(apply(&mut document, &overlay).is_err());
        let overlay = json!({"actions": [{"target": "$.s.missing", "remove": true}]});
        assert!(apply(&mut document, &overlay).is_err());
    }
}
