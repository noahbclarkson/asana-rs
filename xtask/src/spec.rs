//! Loads `spec/asana_oas.yaml` and applies the overlays in `spec/overlays/`.

use std::path::Path;
use std::sync::OnceLock;

use crate::GenResult;
use crate::models::Analysis;
use crate::overlay;

/// The specification, after the overlays.
pub struct Spec {
    /// The whole document as JSON, for the parts `openapiv3` does not model.
    pub raw: serde_json::Value,
    /// The document as typed OpenAPI 3.0.
    pub api: openapiv3::OpenAPI,
    /// What the models half works out about the schemas, built once on first use so that
    /// [`crate::models::rust_type`] answers every call from the same naming decisions.
    analysis: OnceLock<Result<Analysis, String>>,
}

impl Spec {
    /// A specification from a JSON document that has already had its overlays applied. Tests build
    /// small specifications with this.
    pub fn from_raw(raw: serde_json::Value) -> GenResult<Self> {
        let api: openapiv3::OpenAPI = serde_json::from_value(raw.clone())
            .map_err(|error| format!("the specification is not OpenAPI 3.0: {error}"))?;
        Ok(Self {
            raw,
            api,
            analysis: OnceLock::new(),
        })
    }

    /// The models half's analysis of this specification.
    pub fn analysis(&self) -> GenResult<&Analysis> {
        self.analysis
            .get_or_init(|| Analysis::build(&self.raw))
            .as_ref()
            .map_err(Clone::clone)
    }
}

/// Reads the vendored specification under `root` and applies every overlay, in file-name order.
pub fn load(root: &Path) -> GenResult<Spec> {
    let path = root.join("spec/asana_oas.yaml");
    let mut raw = read_yaml(&path)?;
    let directory = root.join("spec/overlays");
    let mut overlays: Vec<_> = std::fs::read_dir(&directory)
        .map_err(|error| format!("reading {}: {error}", directory.display()))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "yaml")
        })
        .collect();
    overlays.sort();
    for overlay_path in overlays {
        let document = read_yaml(&overlay_path)?;
        overlay::apply(&mut raw, &document)
            .map_err(|error| format!("{}: {error}", overlay_path.display()))?;
    }
    Spec::from_raw(raw).map_err(|error| format!("{}: {error}", path.display()))
}

/// Reads a YAML file as JSON. YAML allows keys that are not strings, and Asana's specification
/// writes status codes as integers (`200:`), which JSON cannot hold as keys, so every key is
/// turned into its text first.
pub fn read_yaml(path: &Path) -> GenResult<serde_json::Value> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("reading {}: {error}", path.display()))?;
    yaml_from_str(&text).map_err(|error| format!("parsing {}: {error}", path.display()))
}

/// Parses YAML text into JSON, stringifying non-string keys.
pub fn yaml_from_str(text: &str) -> GenResult<serde_json::Value> {
    let yaml: serde_yaml_ng::Value =
        serde_yaml_ng::from_str(text).map_err(|error| error.to_string())?;
    yaml_to_json(yaml)
}

fn yaml_to_json(yaml: serde_yaml_ng::Value) -> GenResult<serde_json::Value> {
    use serde_json::Value as Json;
    use serde_yaml_ng::Value as Yaml;
    Ok(match yaml {
        Yaml::Null => Json::Null,
        Yaml::Bool(value) => Json::Bool(value),
        Yaml::Number(number) => {
            serde_json::from_str(&number.to_string()).map_err(|error| error.to_string())?
        }
        Yaml::String(text) => Json::String(text),
        Yaml::Sequence(items) => Json::Array(
            items
                .into_iter()
                .map(yaml_to_json)
                .collect::<GenResult<_>>()?,
        ),
        Yaml::Mapping(mapping) => {
            let mut object = serde_json::Map::new();
            for (key, value) in mapping {
                let key = match key {
                    Yaml::String(text) => text,
                    Yaml::Number(number) => number.to_string(),
                    Yaml::Bool(value) => value.to_string(),
                    other => return Err(format!("a mapping key is not a scalar: {other:?}")),
                };
                object.insert(key, yaml_to_json(value)?);
            }
            Json::Object(object)
        }
        Yaml::Tagged(tagged) => yaml_to_json(tagged.value)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integer_status_keys_load_as_text() {
        let json =
            yaml_from_str("responses:\n  200:\n    description: ok\n  '404':\n    x: 1\n").unwrap();
        assert_eq!(json["responses"]["200"]["description"], "ok");
        assert_eq!(json["responses"]["404"]["x"], 1);
    }
}
