//! Applies an [OpenAPI Overlay 1.0](https://spec.openapis.org/overlay/v1.0.0.html) document to the
//! specification before anything reads it.
//!
//! Only the subset the overlays in `spec/overlays/` need is supported: a `target` is a path from the
//! root, `$` followed by `.name`, `['name']` or `[index]` steps (no wildcards or filters), and an
//! action either `remove`s the node it names or merges its `update` into it (objects merge key by
//! key, recursively; anything else is replaced). A target that names nothing is an error, so an
//! overlay that upstream has made stale is noticed instead of silently doing nothing.

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
    merge(node, update);
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
    fn a_target_that_names_nothing_is_an_error() {
        let mut document = json!({"s": {}});
        let overlay = json!({"actions": [{"target": "$.s.missing", "update": {"x": 1}}]});
        assert!(apply(&mut document, &overlay).is_err());
        let overlay = json!({"actions": [{"target": "$.s.missing", "remove": true}]});
        assert!(apply(&mut document, &overlay).is_err());
    }
}
