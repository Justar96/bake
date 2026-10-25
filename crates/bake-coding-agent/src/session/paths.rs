//! Path normalization the session manager applies to working directories,
//! session files, and session directories.
//!
//! Ported from Pi `packages/coding-agent/src/utils/paths.ts` (v1.1.0):
//! `normalizePath` expands a leading `~` and, on Windows, turns Git Bash,
//! MSYS, Cygwin, and WSL drive paths such as `/c/work` into `C:\work`;
//! `resolvePath` makes the result absolute against the current directory and
//! removes `.` and `..` lexically, as Node's `path.resolve` does. One
//! deviation: Pi also accepts `file://` URLs, which Bake treats as ordinary
//! relative paths.

use std::path::{Component, Path, PathBuf};

/// Pi's `normalizeWindowsShellPath`: `/c/x`, `/mnt/c/x`, and `/cygdrive/c/x`
/// to `C:\x`. Other paths are returned unchanged.
pub fn normalize_windows_shell_path(path: &str) -> String {
    if path.starts_with("//") || path.contains('\\') {
        return path.to_owned();
    }
    let Some(rest) = path.strip_prefix('/') else {
        return path.to_owned();
    };
    let rest = rest
        .strip_prefix("mnt/")
        .or_else(|| rest.strip_prefix("cygdrive/"))
        .unwrap_or(rest);
    let mut chars = rest.chars();
    let Some(drive) = chars.next().filter(char::is_ascii_alphabetic) else {
        return path.to_owned();
    };
    let after = chars.as_str();
    let suffix = match after.strip_prefix('/') {
        Some(suffix) => suffix,
        None if after.is_empty() => "",
        None => return path.to_owned(),
    };
    format!(
        "{}:\\{}",
        drive.to_ascii_uppercase(),
        suffix.replace('/', "\\")
    )
}

/// Pi's `normalizePath` with its defaults: Windows shell paths on Windows,
/// then `~`, `~/`, and on Windows `~\` expanded against the home directory.
pub fn normalize_path(input: &str) -> PathBuf {
    let normalized = if cfg!(windows) {
        normalize_windows_shell_path(input)
    } else {
        input.to_owned()
    };
    if let Some(home) = std::env::home_dir() {
        if normalized == "~" {
            return home;
        }
        let rest = normalized.strip_prefix("~/").or_else(|| {
            cfg!(windows)
                .then(|| normalized.strip_prefix("~\\"))
                .flatten()
        });
        if let Some(rest) = rest {
            return home.join(rest);
        }
    }
    PathBuf::from(normalized)
}

/// [`normalize_path`] for a path that may not be UTF-8: only a UTF-8 path
/// can name `~`.
pub fn normalize_path_buf(input: &Path) -> PathBuf {
    match input.to_str() {
        Some(text) => normalize_path(text),
        None => input.to_path_buf(),
    }
}

/// Pi's `resolvePath`: [`normalize_path`], then absolute against the current
/// directory with `.` and `..` removed. When the current directory cannot be
/// read, a relative path is only normalized lexically.
pub fn resolve_path(input: &Path) -> PathBuf {
    let normalized = normalize_path_buf(input);
    let joined = if normalized.is_absolute() {
        normalized
    } else {
        match std::env::current_dir() {
            Ok(base) => normalize_path_buf(&base).join(normalized),
            Err(_) => normalized,
        }
    };
    lexical_normalize(&joined)
}

/// [`resolve_path`] as a string, as Pi stores working directories.
pub fn resolve_path_string(input: &str) -> String {
    resolve_path(Path::new(input))
        .to_string_lossy()
        .into_owned()
}

/// Remove `.` and `..` components and redundant separators.
pub fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                let at_root = matches!(
                    out.components().next_back(),
                    None | Some(Component::RootDir | Component::Prefix(_))
                );
                if !at_root {
                    out.pop();
                } else if !out.has_root() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_shell_paths() {
        assert_eq!(normalize_windows_shell_path("/c/work/x"), "C:\\work\\x");
        assert_eq!(normalize_windows_shell_path("/mnt/d/x"), "D:\\x");
        assert_eq!(normalize_windows_shell_path("/cygdrive/e"), "E:\\");
        assert_eq!(normalize_windows_shell_path("/usr/bin"), "/usr/bin");
        assert_eq!(normalize_windows_shell_path("//server/x"), "//server/x");
        assert_eq!(normalize_windows_shell_path("relative"), "relative");
    }

    #[cfg(unix)]
    #[test]
    fn resolves_lexically() {
        assert_eq!(
            resolve_path(Path::new("/a/./b/../c/")),
            PathBuf::from("/a/c")
        );
        assert_eq!(resolve_path(Path::new("/../x")), PathBuf::from("/x"));
        assert!(resolve_path(Path::new("rel")).is_absolute());
    }

    #[test]
    fn expands_home() {
        if let Some(home) = std::env::home_dir() {
            assert_eq!(normalize_path("~"), home);
            assert_eq!(normalize_path("~/x"), home.join("x"));
        }
        assert_eq!(normalize_path("a~/x"), PathBuf::from("a~/x"));
    }
}
