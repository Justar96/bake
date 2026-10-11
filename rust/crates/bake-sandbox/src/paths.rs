//! Canonical paths and containment, from `canonicalPath` in
//! `packages/sandbox/sandbox/src/roots.ts` and `isPathUnder` in
//! `packages/fs/fs-sandbox/src/containment.ts`.

use std::path::{Component, Path, PathBuf};

/// The path with every symbolic link resolved by the filesystem, as spawn
/// and the enforcement layers see it. When the path or a prefix does not
/// exist, the longest existing prefix is resolved and the rest appended
/// lexically, so a file about to be created gets the identity it will have.
/// The TypeScript `canonicalPath` returned a missing path unchanged; the
/// prefix resolution keeps a new file under a linked workspace inside it.
pub fn canonical_path(path: &Path) -> PathBuf {
    if let Ok(resolved) = std::fs::canonicalize(path) {
        return strip_verbatim(resolved);
    }
    let mut missing: Vec<std::ffi::OsString> = Vec::new();
    let mut prefix = path.to_path_buf();
    loop {
        let Some(name) = prefix.file_name().map(|name| name.to_os_string()) else {
            return path.to_path_buf();
        };
        missing.push(name);
        if !prefix.pop() {
            return path.to_path_buf();
        }
        if let Ok(resolved) = std::fs::canonicalize(&prefix) {
            let mut full = strip_verbatim(resolved);
            for name in missing.iter().rev() {
                full.push(name);
            }
            return full;
        }
    }
}

/// Removes Windows' `\\?\` verbatim prefix from a local drive path, which
/// `std::fs::canonicalize` adds and the rest of the process does not use.
fn strip_verbatim(path: PathBuf) -> PathBuf {
    #[cfg(windows)]
    {
        if let Some(text) = path.to_str()
            && let Some(rest) = text.strip_prefix(r"\\?\")
            && !rest.starts_with("UNC\\")
        {
            return PathBuf::from(rest);
        }
    }
    path
}

/// Whether `path` is `root` or lies beneath it, compared component by
/// component after lexically removing `.` and resolving `..`. Callers pass
/// canonical paths (see [`canonical_path`]); this function does no I/O.
pub fn is_path_under(path: &Path, root: &Path) -> bool {
    let path = lexical(path);
    let root = lexical(root);
    path.starts_with(&root)
}

fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn containment_is_by_component() {
        assert!(is_path_under(Path::new("/w/a/b"), Path::new("/w")));
        assert!(is_path_under(Path::new("/w"), Path::new("/w")));
        assert!(!is_path_under(Path::new("/workspace"), Path::new("/w")));
        assert!(!is_path_under(Path::new("/w/../etc"), Path::new("/w")));
        assert!(is_path_under(Path::new("/w/./a"), Path::new("/w")));
    }

    #[test]
    fn a_missing_tail_keeps_the_resolved_prefix() {
        let base = canonical_path(&std::env::temp_dir());
        let missing = std::env::temp_dir().join("bake-sandbox-missing-dir/child.txt");
        assert_eq!(
            canonical_path(&missing),
            base.join("bake-sandbox-missing-dir").join("child.txt")
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_link_resolves_to_its_target() {
        let dir = std::env::temp_dir().join(format!("bake-sandbox-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("real")).expect("dir");
        std::os::unix::fs::symlink(dir.join("real"), dir.join("link")).expect("link");
        let resolved = canonical_path(&dir.join("link").join("new.txt"));
        assert_eq!(resolved, canonical_path(&dir.join("real")).join("new.txt"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
