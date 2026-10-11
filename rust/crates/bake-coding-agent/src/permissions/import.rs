//! The D25 import of the default preset: `permission.defaultPreset` from a
//! 0.3 home's `settings.yaml`, read once and never written.
//!
//! The 0.3 settings document is a YAML mapping of namespace sections
//! (`packages/settings/settings-file/src/index.ts`); the permission service
//! installed its `permission` section with one field, `defaultPreset`
//! (`installSection(ctx, PERMISSION_SETTINGS_NAMESPACE, …)` in
//! `packages/interaction/permission-presets/src/index.ts`). The file is
//! read as the CLIProxyAPI import reads it: capped at
//! [`MAX_FILE_BYTES`](crate::cliproxyapi::import::MAX_FILE_BYTES) and
//! parsed by its bounded YAML reader.

use std::path::Path;

use serde_json::Value;

use super::presets::PERMISSION_SETTINGS_NAMESPACE;
use crate::cliproxyapi::import::{SETTINGS_FILENAME, read_yaml};

/// The default preset a 0.3 home saved, when it saved one. A missing file
/// or section imports nothing; an unreadable or malformed one is an error
/// whose text names the file.
pub fn import_default_preset(home: &Path) -> Result<Option<String>, String> {
    let file = home.join(SETTINGS_FILENAME);
    let document = read_yaml(&file).map_err(|error| error.to_string())?;
    let at = |path: &str, reason: &str| format!("{}: {path} {reason}", file.display());
    let root = match document {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::Object(root)) => root,
        Some(_) => return Err(at("(root)", "must be a mapping")),
    };
    let section = match root.get(PERMISSION_SETTINGS_NAMESPACE) {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::Object(section)) => section,
        Some(_) => return Err(at(PERMISSION_SETTINGS_NAMESPACE, "must be a mapping")),
    };
    match section.get("defaultPreset") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(name)) if !name.is_empty() => Ok(Some(name.clone())),
        Some(_) => Err(at(
            &format!("{PERMISSION_SETTINGS_NAMESPACE}.defaultPreset"),
            "must be a non-empty string",
        )),
    }
}
