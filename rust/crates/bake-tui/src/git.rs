//! Reads the checked-out branch for the status line, once at startup, from
//! the repository's `HEAD` file. No `git` process runs; a directory outside a
//! repository, or a `HEAD` that cannot be read, reports no branch.

use std::fs;
use std::path::{Path, PathBuf};

use bake_tui_view::status::Branch;

/// Characters of a detached commit the status line shows, as `git` abbreviates.
const SHORT_COMMIT: usize = 7;

/// The branch checked out in the repository holding `dir`, or `None`.
pub fn branch(dir: &Path) -> Option<Branch> {
    let head = fs::read_to_string(git_dir(dir)?.join("HEAD")).ok()?;
    parse_head(head.trim())
}

/// The nearest `.git` at or above `dir`: a directory, or a file naming one,
/// as a linked worktree or a submodule has.
fn git_dir(dir: &Path) -> Option<PathBuf> {
    for candidate in dir.ancestors() {
        let dot = candidate.join(".git");
        if dot.is_dir() {
            return Some(dot);
        }
        if dot.is_file() {
            let text = fs::read_to_string(&dot).ok()?;
            let target = text.trim().strip_prefix("gitdir:")?.trim();
            return Some(candidate.join(target));
        }
    }
    None
}

fn parse_head(head: &str) -> Option<Branch> {
    if let Some(reference) = head.strip_prefix("ref:") {
        let reference = reference.trim();
        let name = reference.strip_prefix("refs/heads/").unwrap_or(reference);
        return (!name.is_empty()).then(|| Branch {
            name: name.to_owned(),
            detached: false,
        });
    }
    (head.len() >= SHORT_COMMIT && head.bytes().all(|b| b.is_ascii_hexdigit())).then(|| Branch {
        name: head[..SHORT_COMMIT].to_owned(),
        detached: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A private directory removed when the test ends, pass or fail.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("bake-tui-git-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn named(name: &str) -> Option<Branch> {
        Some(Branch {
            name: name.into(),
            detached: false,
        })
    }

    #[test]
    fn head_names_a_branch_or_a_detached_commit() {
        assert_eq!(parse_head("ref: refs/heads/feat/x"), named("feat/x"));
        assert_eq!(
            parse_head("ref: refs/remotes/origin/main"),
            named("refs/remotes/origin/main")
        );
        assert_eq!(
            parse_head("0123456789abcdef0123456789abcdef01234567"),
            Some(Branch {
                name: "0123456".into(),
                detached: true,
            })
        );
        assert_eq!(parse_head("ref: "), None);
        assert_eq!(parse_head("garbage"), None);
    }

    #[test]
    fn the_branch_is_found_from_a_subdirectory_and_through_a_worktree_file() {
        let scratch = Scratch::new("repo");
        let repo = scratch.0.join("repo");
        fs::create_dir_all(repo.join(".git")).unwrap();
        fs::create_dir_all(repo.join("src/deep")).unwrap();
        fs::write(repo.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        assert_eq!(branch(&repo.join("src/deep")), named("main"));

        let linked = scratch.0.join("linked");
        let admin = repo.join(".git/worktrees/linked");
        fs::create_dir_all(&admin).unwrap();
        fs::create_dir_all(&linked).unwrap();
        fs::write(admin.join("HEAD"), "ref: refs/heads/topic\n").unwrap();
        fs::write(
            linked.join(".git"),
            format!("gitdir: {}\n", admin.display()),
        )
        .unwrap();
        assert_eq!(branch(&linked), named("topic"));

        // Outside any repository the scratch root's ancestors are searched; a
        // missing HEAD in a fresh `.git` reports nothing.
        let bare = scratch.0.join("bare");
        fs::create_dir_all(bare.join(".git")).unwrap();
        assert_eq!(branch(&bare), None);
    }
}
