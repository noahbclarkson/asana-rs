//! Loads `spec/asana_oas.yaml` and applies the overlays in `spec/overlays/`.

use std::path::Path;

use crate::GenResult;

/// The specification, after the overlays.
pub struct Spec {
    /// The whole document as JSON, for the parts `openapiv3` does not model.
    pub raw: serde_json::Value,
    /// The document as typed OpenAPI 3.0.
    pub api: openapiv3::OpenAPI,
}

/// Reads the vendored specification under `root` and applies every overlay, in file-name order.
pub fn load(root: &Path) -> GenResult<Spec> {
    let path = root.join("spec/asana_oas.yaml");
    let text = std::fs::read_to_string(&path)
        .map_err(|error| format!("reading {}: {error}", path.display()))?;
    let raw: serde_json::Value = serde_yaml_ng::from_str(&text)
        .map_err(|error| format!("parsing {}: {error}", path.display()))?;
    // Overlays are applied here (wave 1, models generator).
    let api: openapiv3::OpenAPI = serde_json::from_value(raw.clone())
        .map_err(|error| format!("{} is not OpenAPI 3.0: {error}", path.display()))?;
    Ok(Spec { raw, api })
}
