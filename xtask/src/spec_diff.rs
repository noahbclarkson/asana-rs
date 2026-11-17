//! `cargo xtask spec-diff <old.yaml> <new.yaml>`: what changed between two versions of Asana's
//! specification, as Markdown for the body of the scheduled spec-update pull request.
//!
//! Both documents are compared as upstream published them, without the overlays: the question is
//! what Asana changed, and a stale overlay is reported separately by `cargo xtask gen`, which fails
//! when an overlay's target no longer exists.
//!
//! An operation is `METHOD /path`. A change is "documentation only" when the two versions are
//! equal once every `description`, `summary`, `example`, `examples`, `externalDocs` and `x-*`
//! extension is removed (a field *named* `description` under `properties` is kept: it is part of
//! the shape). Anything else is a change of shape, which may be breaking for the generated code.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;
use std::process::ExitCode;

use serde_json::{Map, Value};

use crate::spec;

/// The HTTP methods an OpenAPI path item can hold.
const METHODS: &[&str] = &[
    "get", "put", "post", "delete", "patch", "options", "head", "trace",
];

/// Keys that only document a node.
const DOC_KEYS: &[&str] = &[
    "description",
    "summary",
    "example",
    "examples",
    "externalDocs",
];

/// Runs the command: reads both files and prints the Markdown summary.
pub fn run(old: &str, new: &str) -> ExitCode {
    let result = spec::read_yaml(Path::new(old))
        .and_then(|old| Ok((old, spec::read_yaml(Path::new(new))?)))
        .map(|(old, new)| render(&diff(&old, &new)));
    match result {
        Ok(markdown) => {
            print!("{markdown}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("xtask spec-diff: {error}");
            ExitCode::FAILURE
        }
    }
}

/// How one entry differs between the two documents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Change {
    Added,
    Removed,
    /// Different once documentation is ignored.
    Shape,
    /// Equal once documentation is ignored.
    Docs,
}

/// Every difference, grouped: operations by `METHOD /path`, components by section and name.
#[derive(Debug, Default)]
struct Diff {
    old_version: String,
    new_version: String,
    operations: BTreeMap<String, (Change, Option<String>)>,
    components: BTreeMap<String, BTreeMap<String, Change>>,
}

impl Diff {
    fn is_empty(&self) -> bool {
        self.operations.is_empty() && self.components.is_empty()
    }
}

fn diff(old: &Value, new: &Value) -> Diff {
    let version = |document: &Value| {
        document
            .pointer("/info/version")
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_owned()
    };
    let mut result = Diff {
        old_version: version(old),
        new_version: version(new),
        ..Diff::default()
    };
    let (old_ops, new_ops) = (operations(old), operations(new));
    for (key, change) in compare(&old_ops, &new_ops) {
        let id = new_ops
            .get(&key)
            .or_else(|| old_ops.get(&key))
            .and_then(|operation| operation.get("operationId"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        result.operations.insert(key, (change, id));
    }
    let empty = Map::new();
    let sections = |document: &Value| {
        document
            .get("components")
            .and_then(Value::as_object)
            .map_or_else(BTreeSet::new, |components| {
                components.keys().cloned().collect()
            })
    };
    for section in sections(old).union(&sections(new)) {
        let entries = |document: &Value| -> BTreeMap<String, Value> {
            document
                .pointer(&format!("/components/{section}"))
                .and_then(Value::as_object)
                .unwrap_or(&empty)
                .iter()
                .map(|(name, value)| (name.clone(), value.clone()))
                .collect()
        };
        let changes = compare(&entries(old), &entries(new));
        if !changes.is_empty() {
            result.components.insert(section.clone(), changes);
        }
    }
    result
}

/// Every operation in `document`, keyed `METHOD /path`. Parameters declared on the path item
/// apply to each of its operations, so they are folded in under `pathItemParameters` to make a
/// change to them show on the operations they affect (not under an `x-` key, which would be
/// ignored as documentation).
fn operations(document: &Value) -> BTreeMap<String, Value> {
    let mut found = BTreeMap::new();
    let Some(paths) = document.get("paths").and_then(Value::as_object) else {
        return found;
    };
    for (path, item) in paths {
        let shared = item.get("parameters").cloned();
        for method in METHODS {
            if let Some(operation) = item.get(*method) {
                let mut operation = operation.clone();
                if let (Some(shared), Some(object)) = (&shared, operation.as_object_mut()) {
                    object.insert("pathItemParameters".to_owned(), shared.clone());
                }
                found.insert(format!("{} {path}", method.to_uppercase()), operation);
            }
        }
    }
    found
}

fn compare(
    old: &BTreeMap<String, Value>,
    new: &BTreeMap<String, Value>,
) -> BTreeMap<String, Change> {
    let mut changes = BTreeMap::new();
    for (key, before) in old {
        match new.get(key) {
            None => {
                changes.insert(key.clone(), Change::Removed);
            }
            Some(after) if after != before => {
                let change = if without_docs(before, false) == without_docs(after, false) {
                    Change::Docs
                } else {
                    Change::Shape
                };
                changes.insert(key.clone(), change);
            }
            Some(_) => {}
        }
    }
    for key in new.keys().filter(|key| !old.contains_key(*key)) {
        changes.insert(key.clone(), Change::Added);
    }
    changes
}

/// `value` with its documentation removed. `names` is true when the object's keys are names the
/// author chose (the children of `properties`), which are never documentation keys.
fn without_docs(value: &Value, names: bool) -> Value {
    match value {
        Value::Object(object) => Value::Object(
            object
                .iter()
                .filter(|(key, _)| {
                    names || !(DOC_KEYS.contains(&key.as_str()) || key.starts_with("x-"))
                })
                .map(|(key, child)| (key.clone(), without_docs(child, key == "properties")))
                .collect(),
        ),
        Value::Array(items) => {
            Value::Array(items.iter().map(|item| without_docs(item, false)).collect())
        }
        other => other.clone(),
    }
}

fn render(diff: &Diff) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "## What changed in Asana's specification\n");
    if diff.old_version != diff.new_version {
        let _ = writeln!(
            out,
            "`info.version`: `{}` → `{}`\n",
            diff.old_version, diff.new_version
        );
    }
    if diff.is_empty() {
        let _ = writeln!(
            out,
            "No operation or component changed (only the document's own metadata)."
        );
        return out;
    }
    let count = |change| {
        diff.operations
            .values()
            .filter(|(each, _)| *each == change)
            .count()
    };
    let _ = writeln!(
        out,
        "Operations: {} added, {} removed, {} changed in shape, {} in documentation only.\n",
        count(Change::Added),
        count(Change::Removed),
        count(Change::Shape),
        count(Change::Docs),
    );
    for (change, heading) in [
        (Change::Removed, "Removed operations (breaking)"),
        (
            Change::Shape,
            "Operations whose shape changed (check for breaking changes)",
        ),
        (Change::Added, "Added operations"),
        (Change::Docs, "Operations whose documentation changed"),
    ] {
        let lines: Vec<String> = diff
            .operations
            .iter()
            .filter(|(_, (each, _))| *each == change)
            .map(|(key, (_, id))| match id {
                Some(id) => format!("- `{key}` (`{id}`)"),
                None => format!("- `{key}`"),
            })
            .collect();
        if !lines.is_empty() {
            let _ = writeln!(out, "### {heading}\n\n{}\n", lines.join("\n"));
        }
    }
    if !diff.components.is_empty() {
        let _ = writeln!(out, "### Components\n");
        for (section, changes) in &diff.components {
            let names = |change| {
                let names: Vec<String> = changes
                    .iter()
                    .filter(|(_, each)| **each == change)
                    .map(|(name, _)| format!("`{name}`"))
                    .collect();
                names.join(", ")
            };
            let _ = writeln!(out, "- **{section}**");
            for (change, label) in [
                (Change::Removed, "removed"),
                (Change::Shape, "shape changed"),
                (Change::Added, "added"),
                (Change::Docs, "documentation only"),
            ] {
                let list = names(change);
                if !list.is_empty() {
                    let _ = writeln!(out, "  - {label}: {list}");
                }
            }
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn document(version: &str, paths: Value, schemas: Value) -> Value {
        json!({"info": {"version": version}, "paths": paths, "components": {"schemas": schemas}})
    }

    #[test]
    fn operations_are_added_removed_and_changed() {
        let old = document(
            "1.0",
            json!({
                "/tasks/{task_gid}": {
                    "get": {"operationId": "getTask", "responses": {"200": {"description": "ok"}}},
                    "delete": {"operationId": "deleteTask"}
                },
                "/tags": {"get": {"operationId": "getTags", "summary": "Old words"}}
            }),
            json!({}),
        );
        let new = document(
            "1.1",
            json!({
                "/tasks/{task_gid}": {
                    "get": {
                        "operationId": "getTask",
                        "responses": {"200": {"description": "ok"}, "404": {"description": "x"}}
                    },
                    "put": {"operationId": "updateTask"}
                },
                "/tags": {"get": {"operationId": "getTags", "summary": "New words"}}
            }),
            json!({}),
        );
        let found = diff(&old, &new);
        let change = |key: &str| found.operations.get(key).map(|(change, _)| *change);
        assert_eq!(change("DELETE /tasks/{task_gid}"), Some(Change::Removed));
        assert_eq!(change("PUT /tasks/{task_gid}"), Some(Change::Added));
        assert_eq!(change("GET /tasks/{task_gid}"), Some(Change::Shape));
        assert_eq!(change("GET /tags"), Some(Change::Docs));
        assert_eq!(found.operations.len(), 4);

        let markdown = render(&found);
        assert!(markdown.contains("`1.0` → `1.1`"));
        assert!(markdown.contains("1 added, 1 removed, 1 changed in shape, 1 in documentation"));
        assert!(markdown.contains("- `DELETE /tasks/{task_gid}` (`deleteTask`)"));
    }

    #[test]
    fn a_path_level_parameter_change_shows_on_its_operations() {
        let paths = |required: bool| {
            json!({"/tasks/{task_gid}": {
                "parameters": [{"name": "task_gid", "in": "path", "required": required}],
                "get": {"operationId": "getTask"}
            }})
        };
        let found = diff(
            &document("1", paths(true), json!({})),
            &document("1", paths(false), json!({})),
        );
        assert_eq!(
            found
                .operations
                .get("GET /tasks/{task_gid}")
                .map(|(change, _)| *change),
            Some(Change::Shape)
        );
    }

    #[test]
    fn a_field_named_description_is_shape_not_documentation() {
        let old = document(
            "1",
            json!({}),
            json!({
                "Goal": {"properties": {"description": {"type": "string"}}, "description": "A"},
                "Tag": {"properties": {"name": {"type": "string", "description": "Old"}}}
            }),
        );
        let new = document(
            "1",
            json!({}),
            json!({
                "Goal": {"properties": {"notes": {"type": "string"}}, "description": "A"},
                "Tag": {"properties": {"name": {"type": "string", "description": "New"}}},
                "Team": {"type": "object"}
            }),
        );
        let schemas = &diff(&old, &new).components["schemas"];
        assert_eq!(schemas["Goal"], Change::Shape);
        assert_eq!(schemas["Tag"], Change::Docs);
        assert_eq!(schemas["Team"], Change::Added);
    }

    #[test]
    fn identical_documents_say_so() {
        let same = document("1", json!({"/a": {"get": {}}}), json!({"A": {}}));
        let found = diff(&same, &same);
        assert!(found.is_empty());
        assert!(render(&found).contains("No operation or component changed"));
    }
}
