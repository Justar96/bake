//! Development-only spelling of the paths TypeScript's JSONL backend gives a
//! Session's artifacts, from `encodeSegment`, `projectKey`, `projectDir`,
//! `sessionDir`, and `logPath` in
//! `packages/session/session-persistence-jsonl/src/format.ts` and
//! `sessionFormatLogFilename` in `packages/session/session-format/src/filename.ts`.

use std::path::{Path, PathBuf};

use crate::{CURRENT_SESSION_FORMAT_VERSION, MAX_SAFE_INTEGER};

/// The project directory of a Session without a `cwd`.
pub(crate) const NO_CWD_DIR: &str = "_no-cwd";

/// The current generation's plain log name, `sessionFormatLogFilename(3)`.
pub(crate) const CURRENT_LOG_FILENAME: &str = "session.v3.jsonl";

const _: () = assert!(CURRENT_SESSION_FORMAT_VERSION == 3);

/// Encode one string as one safe path segment, as TypeScript's
/// `encodeSegment` does over UTF-16 code units: ASCII letters, digits, `.`,
/// `_`, and `-` stay literal, every other unit becomes `~XXXX`, and the whole
/// segments `.` and `..` are escaped. TypeScript throws for an empty string,
/// which this returns unchanged.
pub fn encode_segment(raw: &str) -> String {
    match raw {
        "." => return "~002E".into(),
        ".." => return "~002E~002E".into(),
        _ => {}
    }
    let mut out = String::with_capacity(raw.len());
    for unit in raw.encode_utf16() {
        push_unit(&mut out, unit);
    }
    out
}

/// The project directory name for a `cwd`, as TypeScript's `projectKey`
/// builds it: runs of `/`, `\`, and `:` become one `-`, other units are kept
/// or escaped as in [`encode_segment`], leading `-` are removed, an empty
/// result is `root`, and the key keeps its first 251 units between `--` and
/// `--`. Escapes are ASCII, so the cut can split one. TypeScript throws for
/// an empty `cwd`, which this spells `--root--`.
pub fn project_key(cwd: &str) -> String {
    let mut readable = String::with_capacity(cwd.len());
    let mut separator_run = false;
    for unit in cwd.encode_utf16() {
        if matches!(unit, 0x2F | 0x5C | 0x3A) {
            if !separator_run {
                readable.push('-');
            }
            separator_run = true;
        } else {
            push_unit(&mut readable, unit);
            separator_run = false;
        }
    }
    let slug = readable.trim_start_matches('-');
    let slug = if slug.is_empty() { "root" } else { slug };
    format!("--{}--", &slug[..slug.len().min(251)])
}

fn push_unit(out: &mut String, unit: u16) {
    match u8::try_from(unit) {
        Ok(byte) if byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-') => {
            out.push(char::from(byte));
        }
        _ => out.push_str(&format!("~{unit:04X}")),
    }
}

/// TypeScript's `logPath(root, cwd, id, 'none')`: the current generation's
/// plain log of Session `id`, beneath the project directory its `cwd` names
/// or `_no-cwd`. `None` where TypeScript throws: an empty `id` or `cwd`.
/// The parts are joined with [`Path::join`], which does not normalize `root`
/// as Node's `join` does.
pub fn session_log_path(root: &Path, cwd: Option<&str>, id: &str) -> Option<PathBuf> {
    if id.is_empty() || cwd == Some("") {
        return None;
    }
    Some(log_path(root, cwd, &encode_segment(id)))
}

/// [`session_log_path`] for an already encoded, non-empty id segment.
pub(crate) fn log_path(root: &Path, cwd: Option<&str>, encoded_id: &str) -> PathBuf {
    let project = cwd.map_or_else(|| NO_CWD_DIR.to_owned(), project_key);
    root.join(project)
        .join(encoded_id)
        .join(CURRENT_LOG_FILENAME)
}

/// TypeScript's `parseSessionFormatLogFilename`: the generation a canonical
/// plain log name carries, `session.jsonl` as 0 and `session.vN.jsonl` as N
/// for a decimal N without a leading zero, or `None` for any other name,
/// including one whose N is not a safe integer.
pub(crate) fn canonical_generation(name: &str) -> Option<u64> {
    let rest = name.strip_prefix("session")?;
    if rest == ".jsonl" {
        return Some(0);
    }
    let digits = rest.strip_prefix(".v")?.strip_suffix(".jsonl")?;
    if !digits.starts_with(|first: char| matches!(first, '1'..='9'))
        || !digits.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    // Overflowing u64 is far beyond 2^53 − 1, where `Number` is not safe.
    digits
        .parse::<u64>()
        .ok()
        .filter(|version| *version <= MAX_SAFE_INTEGER)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_generation_names_match_the_typescript_pattern() {
        for (name, version) in [
            ("session.jsonl", Some(0)),
            ("session.v1.jsonl", Some(1)),
            ("session.v3.jsonl", Some(3)),
            ("session.v10.jsonl", Some(10)),
            (
                "session.v9007199254740991.jsonl",
                Some(9_007_199_254_740_991),
            ),
            ("session.v9007199254740992.jsonl", None),
            ("session.v99999999999999999999999.jsonl", None),
            ("session.v0.jsonl", None),
            ("session.v03.jsonl", None),
            ("session.V3.jsonl", None),
            ("session.v.jsonl", None),
            ("session.v3.json", None),
            ("session.v3.jsonl.zstd", None),
            ("session.v3.jsonl.tmp", None),
            ("session.v+3.jsonl", None),
            ("Session.jsonl", None),
        ] {
            assert_eq!(canonical_generation(name), version, "{name}");
        }
    }

    #[test]
    fn session_log_path_refuses_what_typescript_cannot_encode() {
        let root = Path::new("r");
        assert_eq!(session_log_path(root, None, ""), None);
        assert_eq!(session_log_path(root, Some(""), "a"), None);
        assert_eq!(
            session_log_path(root, Some("/w"), ".."),
            Some(
                root.join("--w--")
                    .join("~002E~002E")
                    .join("session.v3.jsonl")
            )
        );
    }
}
