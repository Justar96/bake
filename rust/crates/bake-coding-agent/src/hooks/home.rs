//! The hooks files `settings.json` names, read under the owner rules of
//! `$BAKE_HOME/mcp.json` ([`crate::mcp::home`]).
//!
//! Bake 0.3 pointed each bridge's `configPath` at a `hooks.json`; bake-rs
//! takes the same paths from the `hooks` member of `settings.json`
//! ([`HookSettings`]). A path may start with `~/`; a relative one is read
//! from the Bake home. A hooks file runs commands and decides which tool
//! calls run, so, as for `mcp.json`, it must be a regular file that only
//! its owner (or root) can change, in directories only they can change,
//! and neither inside the session's working directory nor in a git work
//! tree below the user's home directory, all checked on the opened file.
//! A file that is missing, refused, unreadable, or invalid is a warning,
//! and none of its hooks run; the session still starts.

use std::io::Read;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::HookDialect;
use super::config::{HookGroups, parse_hooks};
use crate::mcp::home::{OwnerFile, open_owner_file};
use crate::settings::HookSettings;

/// The largest hooks file read, in bytes.
pub const MAX_HOOKS_FILE_BYTES: u64 = 1024 * 1024;

/// One hooks file's runnable hooks.
#[derive(Debug, Clone, PartialEq)]
pub struct HookFile {
    /// Its dialect.
    pub dialect: HookDialect,
    /// Where it was read.
    pub path: PathBuf,
    /// Its hooks.
    pub groups: HookGroups,
}

/// The hooks files `settings` names, Claude Code's first, each read for a
/// session in `workspace`; `home` is the Bake home and `user_home` the
/// user's home directory. Returns the files with hooks to run and a
/// warning for each problem. Blocking.
pub fn load_hook_files(
    settings: &HookSettings,
    home: &Path,
    workspace: &Path,
    user_home: Option<&Path>,
) -> (Vec<HookFile>, Vec<String>) {
    let mut files = Vec::new();
    let mut warnings = Vec::new();
    for (dialect, configured) in [
        (HookDialect::ClaudeCode, settings.claude_code.as_deref()),
        (HookDialect::Codex, settings.codex.as_deref()),
    ] {
        let Some(configured) = configured else {
            continue;
        };
        let path = resolve(configured, home, user_home);
        match load(dialect, &path, workspace, user_home) {
            Ok((groups, skipped)) => {
                warnings.extend(
                    skipped
                        .into_iter()
                        .map(|skipped| format!("{} hooks: {skipped}", dialect.title())),
                );
                if !groups.is_empty() {
                    files.push(HookFile {
                        dialect,
                        path,
                        groups,
                    });
                }
            }
            Err(reason) => warnings.push(format!(
                "{} hooks: {}: {reason}. None of its hooks run",
                dialect.title(),
                path.display()
            )),
        }
    }
    (files, warnings)
}

/// `~` and `~/…` name the user's home directory; a relative path is read
/// from the Bake home.
fn resolve(configured: &str, home: &Path, user_home: Option<&Path>) -> PathBuf {
    let rest = configured.strip_prefix("~/").or_else(|| {
        cfg!(windows)
            .then(|| configured.strip_prefix("~\\"))
            .flatten()
    });
    match (user_home, configured, rest) {
        (Some(user_home), "~", _) => user_home.to_path_buf(),
        (Some(user_home), _, Some(rest)) => user_home.join(rest),
        _ => home.join(configured),
    }
}

fn load(
    dialect: HookDialect,
    path: &Path,
    workspace: &Path,
    user_home: Option<&Path>,
) -> Result<(HookGroups, Vec<String>), String> {
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return Err("does not name a file".to_owned());
    };
    let name = name
        .to_str()
        .ok_or_else(|| "has a file name that is not UTF-8".to_owned())?;
    let dir = std::fs::canonicalize(parent).map_err(|error| error.to_string())?;
    let rules = OwnerFile {
        name,
        resolved: "the hooks file's directory",
        kind: "a hooks file",
        workspace_remedy: "keep it outside, or remove it from the hooks member of settings.json",
        repository_remedy: "keep it outside every repository, or remove it from the hooks member of settings.json",
        unchecked: "bake-rs cannot yet check on this platform who may change a hooks file, so it does not read it; remove it from the hooks member of settings.json until that check is supported",
    };
    let file = open_owner_file(&dir, &rules, workspace, user_home)?
        .ok_or_else(|| "not found".to_owned())?;
    let mut text = String::new();
    let read = file
        .take(MAX_HOOKS_FILE_BYTES + 1)
        .read_to_string(&mut text)
        .map_err(|error| error.to_string())?;
    if read as u64 > MAX_HOOKS_FILE_BYTES {
        return Err(format!("larger than {MAX_HOOKS_FILE_BYTES} bytes"));
    }
    let raw: Value = serde_json::from_str(crate::auth_storage::strip_bom(&text))
        .map_err(|error| format!("is not valid JSON: {error}"))?;
    let parsed = parse_hooks(&raw, dialect)?;
    Ok((parsed.groups, parsed.skipped))
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use serde_json::json;

    use super::*;
    use crate::hooks::HookEvent;
    use crate::test_support::TempDir;

    fn settings(claude_code: Option<&str>, codex: Option<&str>) -> HookSettings {
        HookSettings {
            claude_code: claude_code.map(str::to_owned),
            codex: codex.map(str::to_owned),
        }
    }

    fn write(path: &Path, value: &Value) {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        std::fs::write(path, value.to_string()).expect("write");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).expect("mode");
    }

    fn private_dir(name: &str) -> TempDir {
        let dir = TempDir::new(name);
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).expect("mode");
        dir
    }

    #[test]
    fn unset_settings_read_nothing() {
        let dir = private_dir("hooks-home-unset");
        let (files, warnings) =
            load_hook_files(&HookSettings::default(), dir.path(), Path::new("/"), None);
        assert!(files.is_empty() && warnings.is_empty());
    }

    #[test]
    fn both_dialects_load_in_order_with_their_skips() {
        let dir = private_dir("hooks-home-both");
        let home = dir.join("home");
        let user = dir.join("user");
        write(
            &home.join("claude.json"),
            &json!({ "hooks": { "PreToolUse": [{ "matcher": "bash", "hooks": [
                { "type": "command", "command": "a.sh" },
                { "type": "prompt" },
            ] }] } }),
        );
        write(
            &user.join(".codex/hooks.json"),
            &json!({ "Stop": [{ "hooks": [{ "command": "s.sh" }] }] }),
        );
        let workspace = dir.join("ws");
        std::fs::create_dir_all(&workspace).expect("ws");
        let (files, warnings) = load_hook_files(
            &settings(Some("claude.json"), Some("~/.codex/hooks.json")),
            &home,
            &workspace,
            Some(&user),
        );
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].dialect, HookDialect::ClaudeCode);
        assert_eq!(files[0].path, home.join("claude.json"));
        assert_eq!(files[0].groups.of(HookEvent::PreToolUse).len(), 1);
        assert_eq!(files[1].dialect, HookDialect::Codex);
        assert_eq!(files[1].path, user.join(".codex/hooks.json"));
        assert_eq!(files[1].groups.of(HookEvent::Stop).len(), 1);
        assert_eq!(
            warnings,
            [
                "Claude Code hooks: skipping unsupported \"prompt\" hook on PreToolUse (only command hooks run)"
            ]
        );
    }

    #[test]
    fn a_missing_invalid_or_unsafe_file_is_a_warning_and_no_hooks() {
        let dir = private_dir("hooks-home-refused");
        let home = dir.join("home");
        std::fs::create_dir_all(&home).expect("home");
        let workspace = dir.join("ws");
        let shown = |name: &str| home.join(name).display().to_string();
        let load = |name: &str| {
            load_hook_files(
                &settings(Some(name), None),
                &home,
                &workspace,
                Some(dir.path()),
            )
        };
        let (files, warnings) = load("missing.json");
        assert!(files.is_empty());
        assert_eq!(
            warnings,
            [format!(
                "Claude Code hooks: {}: not found. None of its hooks run",
                shown("missing.json")
            )]
        );
        std::fs::write(home.join("bad.json"), "{").expect("write");
        let (files, warnings) = load("bad.json");
        assert!(files.is_empty());
        assert!(
            warnings[0].contains(": is not valid JSON: "),
            "{warnings:?}"
        );
        write(
            &home.join("regex.json"),
            &json!({ "PreToolUse": [{ "matcher": "(", "hooks": [{ "command": "x" }] }] }),
        );
        let (files, warnings) = load("regex.json");
        assert!(files.is_empty());
        assert_eq!(
            warnings,
            [format!(
                "Claude Code hooks: {}: invalid claude-code regex matcher \"(\" on event \"PreToolUse\". None of its hooks run",
                shown("regex.json")
            )]
        );
        write(&home.join("open.json"), &json!({}));
        std::fs::set_permissions(
            home.join("open.json"),
            std::fs::Permissions::from_mode(0o622),
        )
        .expect("mode");
        let (files, warnings) = load("open.json");
        assert!(files.is_empty());
        assert!(
            warnings[0].contains("can be changed by other users (mode 0622)"),
            "{warnings:?}"
        );
        std::os::unix::fs::symlink(home.join("regex.json"), home.join("link.json")).expect("link");
        let (_, warnings) = load("link.json");
        assert!(warnings[0].contains("is a symbolic link"), "{warnings:?}");
    }

    #[test]
    fn a_file_the_agent_can_write_is_refused() {
        let dir = private_dir("hooks-home-workspace");
        let workspace = dir.join("ws");
        write(
            &workspace.join(".claude/settings.json"),
            &json!({ "hooks": { "Stop": [{ "hooks": [{ "command": "s.sh" }] }] } }),
        );
        let (files, warnings) = load_hook_files(
            &settings(
                Some(&workspace.join(".claude/settings.json").to_string_lossy()),
                None,
            ),
            &dir.join("home"),
            &workspace,
            None,
        );
        assert!(files.is_empty());
        assert!(
            warnings[0].contains("is inside the session's working directory")
                && warnings[0].ends_with(
                    "keep it outside, or remove it from the hooks member of settings.json. None of its hooks run"
                ),
            "{warnings:?}"
        );
        let repo = dir.join("repo");
        std::fs::create_dir_all(repo.join(".git")).expect("git");
        write(&repo.join("hooks.json"), &json!({}));
        let (files, warnings) = load_hook_files(
            &settings(None, Some(&repo.join("hooks.json").to_string_lossy())),
            &dir.join("home"),
            &workspace,
            None,
        );
        assert!(files.is_empty());
        assert!(
            warnings[0].starts_with("Codex hooks: ")
                && warnings[0].contains("is inside the git work tree"),
            "{warnings:?}"
        );
    }
}
