//! Pure rules for what cleanup may delete, tested on every platform.
//!
//! Cleanup deletes only objects inside the root this run created
//! exclusively, children before parents, and the root itself last.

#![allow(dead_code)]

use std::path::{Component, Path};

/// Whether `path` is `root` or lies below it, compared by components with
/// no `.` or `..` in either. A path with `..` is never inside.
pub fn within(root: &Path, path: &Path) -> bool {
    let plain = |p: &Path| p.components().all(|c| !matches!(c, Component::ParentDir | Component::CurDir));
    plain(root) && plain(path) && path.starts_with(root)
}

/// Indices of `paths` in deletion order: deepest first, and among equal
/// depth the later-recorded first. Paths outside `root` are left out.
pub fn deletion_order<P: AsRef<Path>>(root: &Path, paths: &[P]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..paths.len()).filter(|&i| within(root, paths[i].as_ref())).collect();
    order.sort_by(|&a, &b| {
        let depth = |i: usize| paths[i].as_ref().components().count();
        depth(b).cmp(&depth(a)).then(b.cmp(&a))
    });
    order
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn only_paths_under_the_root_are_within() {
        let root = Path::new("/t/run/bake-trust-probe-1-ab");
        assert!(within(root, root));
        assert!(within(root, &root.join("m1/d")));
        assert!(!within(root, Path::new("/t/run")));
        assert!(!within(root, Path::new("/t/run/bake-trust-probe-1-abc")));
        assert!(!within(root, &root.join("../escape")));
        assert!(!within(root, &root.join("./m1")));
    }

    #[test]
    fn children_are_deleted_before_parents_and_outsiders_never() {
        let root = PathBuf::from("/t/r");
        let paths = [
            root.clone(),
            root.join("m1"),
            root.join("m1/d"),
            PathBuf::from("/elsewhere/x"),
            root.join("m1/f"),
            root.join("m2"),
        ];
        let order = deletion_order(&root, &paths);
        assert_eq!(order, vec![4, 2, 5, 1, 0]);
    }
}
