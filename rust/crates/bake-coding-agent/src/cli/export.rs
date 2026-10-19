//! `--export <session-file> [output.html]`: write a session file as one
//! self-contained HTML page and exit.
//!
//! Adapted from Pi v1.3.0 (`71023aec1546d6d677a65669c5665c0927ce0dd0`)
//! `packages/coding-agent/src/core/export-html/index.ts` (`exportFromFile`)
//! and the `parsed.export` branch of `packages/coding-agent/src/main.ts`.
//! The page itself is [`crate::export_html`], the one `/share` uploads; as
//! in Pi's `exportFromFile`, it carries no system prompt and no tool
//! definitions, because no session is set up to supply them.
//!
//! The default output name is Pi's, with Bake's program name:
//! `bake-rs-session-<input file name without .jsonl>.html`, in the working
//! directory.
//!
//! Deviation from Pi: an empty input file is refused with Pi's
//! [`EXPORT_NOTHING_YET`](crate::agent_session::share::EXPORT_NOTHING_YET)
//! message instead of being opened. Pi's `SessionManager.open`, and this
//! port's, give an empty session file a header and write it; an export
//! never writes to the file it reads.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::agent_session::share::EXPORT_NOTHING_YET;
use crate::export_html::{self, SessionData};
use crate::session::SessionManager;
use crate::session::paths::{normalize_path, resolve_path};

/// Pi's `exportFromFile`: the session at `input` as HTML at `output`, or at
/// Pi's default name beside the working directory. Returns the path
/// written, or Pi's failure message. Blocking.
pub fn export_from_file(input: &str, output: Option<&str>) -> Result<PathBuf, String> {
    let resolved = resolve_path(Path::new(input));
    if !resolved.exists() {
        return Err(format!("File not found: {}", resolved.display()));
    }
    if std::fs::metadata(&resolved)
        .map_err(|error| format!("{}: {error}", resolved.display()))?
        .len()
        == 0
    {
        return Err(EXPORT_NOTHING_YET.to_owned());
    }
    let session = SessionManager::open(&resolved, None, None).map_err(|error| error.to_string())?;
    let data = SessionData {
        header: session
            .header()
            .map(|header| Value::Object(header.as_json().clone())),
        entries: session
            .entries()
            .iter()
            .map(|entry| Value::Object(entry.as_json().clone()))
            .collect(),
        leaf_id: session.leaf_id().map(str::to_owned),
        system_prompt: None,
        tools: None,
    };
    let html = export_html::generate_html(&data);
    let output = match output {
        Some(path) => normalize_path(path),
        None => PathBuf::from(default_output_name(&resolved)),
    };
    std::fs::write(&output, html).map_err(|error| format!("{}: {error}", output.display()))?;
    Ok(output)
}

/// Pi's default export name for `input`, with Bake's program name.
fn default_output_name(input: &Path) -> String {
    let stem = input
        .file_name()
        .map(|name| {
            let name = name.to_string_lossy();
            name.strip_suffix(".jsonl")
                .unwrap_or(name.as_ref())
                .to_owned()
        })
        .unwrap_or_default();
    format!("bake-rs-session-{stem}.html")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TempDir, write};

    /// A session file with a header and one user message.
    fn write_session(dir: &Path) -> PathBuf {
        let path = dir.join("2026-01-01T00-00-00-000Z_s1.jsonl");
        let header = serde_json::json!({
            "type": "session", "version": 3, "id": "s1",
            "timestamp": "2026-01-01T00:00:00.000Z", "cwd": dir.to_string_lossy(),
        });
        let message = serde_json::json!({
            "type": "message", "id": "a1", "parentId": null,
            "timestamp": "2026-01-01T00:00:01.000Z",
            "message": { "role": "user", "content": "hi", "timestamp": 1 },
        });
        write(&path, &format!("{header}\n{message}\n"));
        path
    }

    // Pi `test/export-html.test.ts`: the page holds the session and nothing
    // writes to the file it read.
    #[test]
    fn a_session_file_becomes_one_self_contained_page() {
        let root = TempDir::new("export");
        let input = write_session(root.path());
        let before = std::fs::read(&input).expect("read");
        let output = root.join("out.html");
        let written = export_from_file(&input.to_string_lossy(), Some(&output.to_string_lossy()))
            .expect("export");
        assert_eq!(written, output);
        let html = std::fs::read_to_string(&output).expect("page");
        assert!(html.starts_with("<!DOCTYPE html>"));
        assert!(!html.contains("__SESSION_DATA__"));
        assert_eq!(std::fs::read(&input).expect("read again"), before);
    }

    // Pi's `exportFromFile` refuses a missing file; an empty one is refused
    // here rather than given a header (see the module documentation).
    #[test]
    fn a_missing_or_empty_session_file_is_refused_without_writing() {
        let root = TempDir::new("export-refuse");
        let missing = root.join("nope.jsonl");
        assert_eq!(
            export_from_file(&missing.to_string_lossy(), None),
            Err(format!("File not found: {}", missing.display()))
        );
        let empty = root.join("empty.jsonl");
        write(&empty, "");
        assert_eq!(
            export_from_file(&empty.to_string_lossy(), None),
            Err(EXPORT_NOTHING_YET.to_owned())
        );
        assert_eq!(std::fs::read(&empty).expect("still empty").len(), 0);
    }

    #[test]
    fn the_default_name_drops_one_jsonl_suffix() {
        assert_eq!(
            default_output_name(Path::new("/a/b/2026-01-01T00-00-00-abc.jsonl")),
            "bake-rs-session-2026-01-01T00-00-00-abc.html"
        );
        assert_eq!(
            default_output_name(Path::new("session")),
            "bake-rs-session-session.html"
        );
    }
}
