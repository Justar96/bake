//! Context files and prompt inputs, from Pi's resource loader.
//!
//! Ported from Pi `packages/coding-agent/src/core/resource-loader.ts`
//! (v1.1.0, with v1.2.0's comparison of a shadowed file by its directory's
//! real path): `loadContextFileFromDir`, `findShadowedContextFile`,
//! `loadProjectContextFiles`, and `resolvePromptInput`, with `findGitPaths`
//! from `footer-data-provider.ts`.
//!
//! Skills are loaded by [`crate::skills`]. The rest of the resource loader
//! (extensions, prompt templates, themes, packages, and the `SYSTEM.md` and
//! `APPEND_SYSTEM.md` discovery that needs project trust) is not ported.
//!
//! # Deviations from Pi
//!
//! - **`AGENTS.md` only.** Each directory's context file is the first of
//!   `AGENTS.override.md`, `AGENTS.md`, and `AGENTS.MD`; Pi also falls back
//!   to `CLAUDE.md`, which Bake does not read.
//! - **Global file.** The global context file comes from the universal
//!   `~/.agents` directory where Pi reads `~/.pi/agent`.
//!
//! A context file larger than [`MAX_CONTEXT_FILE_BYTES`] is skipped with
//! Pi's read warning. Warnings are returned instead of printed, without
//! the `Warning: ` prefix the caller adds.

use std::fs;
use std::path::{Path, PathBuf};

use crate::auth_storage::{read_text_file, strip_bom};
use crate::session::paths::{resolve_path, resolve_path_string};

/// The largest context file read, in bytes.
pub const MAX_CONTEXT_FILE_BYTES: u64 = 16 * 1024 * 1024;

/// A loaded context file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextFile {
    /// Its path.
    pub path: String,
    /// Its text, without a byte-order mark.
    pub content: String,
}

const CANDIDATES: [&str; 3] = ["AGENTS.override.md", "AGENTS.md", "AGENTS.MD"];

/// The user's universal agent directory, `~/.agents`, which holds the
/// global `AGENTS.md` and the user's skills.
pub fn user_agents_dir(user_home: &Path) -> PathBuf {
    user_home.join(".agents")
}

/// Pi's `loadContextFileFromDir`: the first candidate that is a readable
/// file.
fn load_context_file_from_dir(dir: &Path, warnings: &mut Vec<String>) -> Option<ContextFile> {
    for name in CANDIDATES {
        let path = dir.join(name);
        if !path.exists() {
            continue;
        }
        let display = path.to_string_lossy().into_owned();
        match fs::metadata(&path) {
            Ok(metadata) if !metadata.is_file() => continue,
            Ok(_) => {}
            Err(error) => {
                warnings.push(format!("Could not read {display}: {error}"));
                continue;
            }
        }
        match read_text_file(&path, MAX_CONTEXT_FILE_BYTES) {
            Ok(Some(content)) => {
                return Some(ContextFile {
                    path: display,
                    content: strip_bom(&content).to_owned(),
                });
            }
            Ok(None) => continue,
            Err(error) => warnings.push(format!("Could not read {display}: {error}")),
        }
    }
    None
}

/// Pi's `canonicalizePath`: the real path, else the path as given.
fn canonicalize(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

struct GitPaths {
    repo_dir: PathBuf,
    common_git_dir: PathBuf,
}

/// Pi's `findGitPaths`: the nearest `.git` directory or `gitdir:` file.
fn find_git_paths(cwd: &Path) -> Option<GitPaths> {
    let mut dir = cwd.to_path_buf();
    loop {
        let git = dir.join(".git");
        if git.exists() {
            let metadata = fs::metadata(&git).ok()?;
            if metadata.is_file() {
                let content = read_text_file(&git, 64 * 1024).ok()??;
                let content = content.trim();
                let target = content.strip_prefix("gitdir: ")?;
                let git_dir = resolve_path(&dir.join(target.trim()));
                if !git_dir.join("HEAD").exists() {
                    return None;
                }
                let common = git_dir.join("commondir");
                let common_git_dir = if common.exists() {
                    let text = read_text_file(&common, 64 * 1024).ok()??;
                    resolve_path(&git_dir.join(text.trim()))
                } else {
                    git_dir
                };
                return Some(GitPaths {
                    repo_dir: dir,
                    common_git_dir,
                });
            }
            if metadata.is_dir() {
                if !git.join("HEAD").exists() {
                    return None;
                }
                return Some(GitPaths {
                    repo_dir: dir,
                    common_git_dir: git,
                });
            }
        }
        let parent = dir.parent()?.to_path_buf();
        if parent == dir {
            return None;
        }
        dir = parent;
    }
}

/// Pi's `findShadowedContextFile`: the main repository's context file that
/// a nested linked worktree's own copy shadows.
fn find_shadowed_context_file(cwd: &Path) -> Option<PathBuf> {
    let paths = find_git_paths(cwd)?;
    let common = canonicalize(&paths.common_git_dir);
    let worktree_root = canonicalize(&paths.repo_dir);
    let main_root = common.parent()?.to_path_buf();
    if worktree_root == main_root || !worktree_root.starts_with(&main_root) {
        return None;
    }
    if canonicalize(&main_root.join(".git")) != common {
        return None;
    }
    let file = load_context_file_from_dir(&worktree_root, &mut Vec::new())?;
    let name = Path::new(&file.path).file_name()?.to_owned();
    Some(main_root.join(name))
}

/// Pi's `loadProjectContextFiles`: the global file from `global_dir`
/// (`~/.agents`; none when `None`), then one file per directory from the
/// filesystem root down to `cwd`, each path once, leaving out the main
/// repository's file that a nested worktree shadows. Returns the files and
/// any read warnings.
pub fn load_project_context_files(
    cwd: &str,
    global_dir: Option<&Path>,
) -> (Vec<ContextFile>, Vec<String>) {
    let mut warnings = Vec::new();
    let cwd = PathBuf::from(resolve_path_string(cwd));
    let mut files = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    if let Some(global_dir) = global_dir
        && let Some(global) = load_context_file_from_dir(&resolve_path(global_dir), &mut warnings)
    {
        seen.push(global.path.clone());
        files.push(global);
    }
    let shadowed = find_shadowed_context_file(&cwd);
    let mut ancestors: Vec<ContextFile> = Vec::new();
    let mut dir = Some(cwd.as_path());
    while let Some(current) = dir {
        if let Some(file) = load_context_file_from_dir(current, &mut warnings) {
            // The directory's real path, not the file's: a worktree file that
            // links to the main repository's resolves to the shadowed path.
            let is_shadowed = shadowed.as_ref().is_some_and(|shadowed| {
                Path::new(&file.path)
                    .file_name()
                    .is_some_and(|name| canonicalize(current).join(name) == *shadowed)
            });
            if !is_shadowed && !seen.contains(&file.path) {
                seen.push(file.path.clone());
                ancestors.insert(0, file);
            }
        }
        dir = current.parent().filter(|parent| *parent != current);
    }
    files.extend(ancestors);
    (files, warnings)
}

/// Pi's `resolvePromptInput`: the contents of the file `input` names, else
/// `input` itself; `None` for no or empty input. A file that fails to read
/// is used as text with Pi's warning.
pub fn resolve_prompt_input(
    input: Option<&str>,
    description: &str,
    warnings: &mut Vec<String>,
) -> Option<String> {
    let input = input.filter(|input| !input.is_empty())?;
    let path = Path::new(input);
    if path.exists() {
        return match read_text_file(path, MAX_CONTEXT_FILE_BYTES) {
            Ok(Some(text)) => Some(strip_bom(&text).to_owned()),
            Ok(None) => Some(input.to_owned()),
            Err(error) => {
                warnings.push(format!(
                    "Could not read {description} file {input}: {error}"
                ));
                Some(input.to_owned())
            }
        };
    }
    Some(input.to_owned())
}

#[cfg(test)]
mod tests {
    //! Cases from Pi `test/resource-loader.test.ts` (v1.1.0): the context
    //! file cases of `describe("reload")` and every case of
    //! `describe("loadProjectContextFiles - nested worktree dedup")`.

    use super::*;
    use crate::test_support::{TempDir, write};

    fn contents(cwd: &Path, agent_dir: &Path) -> Vec<String> {
        let (files, warnings) = load_project_context_files(&cwd.to_string_lossy(), Some(agent_dir));
        assert_eq!(warnings, Vec::<String>::new());
        files.into_iter().map(|file| file.content).collect()
    }

    // Pi: "should prefer AGENTS.override.md within each directory while
    // preserving ancestor layering".
    #[test]
    fn overrides_win_per_directory_and_ancestors_layer() {
        let temp = TempDir::new("context-layering");
        let agent = temp.join("agent");
        let cwd = temp.join("project");
        let nested = cwd.join("services").join("api");
        write(&agent.join("AGENTS.md"), "global instructions");
        write(&agent.join("AGENTS.override.md"), "global override");
        write(&cwd.join("AGENTS.md"), "project instructions");
        write(&nested.join("AGENTS.md"), "service instructions");
        write(&nested.join("AGENTS.override.md"), "service override");
        let (files, _) = load_project_context_files(&nested.to_string_lossy(), Some(&agent));
        let paths: Vec<(String, String)> = files
            .into_iter()
            .map(|file| (file.path, file.content))
            .collect();
        let path = |p: PathBuf| p.to_string_lossy().into_owned();
        assert_eq!(
            paths,
            vec![
                (
                    path(agent.join("AGENTS.override.md")),
                    "global override".into()
                ),
                (path(cwd.join("AGENTS.md")), "project instructions".into()),
                (
                    path(nested.join("AGENTS.override.md")),
                    "service override".into()
                ),
            ]
        );
    }

    // Pi: "should ignore context file candidates that are directories" and
    // "should discover AGENTS.md context files", without Pi's `CLAUDE.md`
    // fallback.
    #[test]
    fn directories_are_skipped_and_claude_md_is_not_read() {
        let temp = TempDir::new("context-dirs");
        let agent = temp.join("agent");
        let cwd = temp.join("project");
        std::fs::create_dir_all(cwd.join("AGENTS.override.md")).expect("dir");
        write(&cwd.join("AGENTS.md"), "\u{feff}Project instructions");
        write(&cwd.join("CLAUDE.md"), "Claude instructions");
        assert_eq!(contents(&cwd, &agent), ["Project instructions"]);

        let other = temp.join("claude-only");
        write(&other.join("CLAUDE.md"), "Claude instructions");
        write(&agent.join("CLAUDE.md"), "Global Claude instructions");
        assert_eq!(contents(&other, &agent), Vec::<String>::new());
    }

    #[test]
    fn without_a_global_directory_only_project_files_load() {
        let temp = TempDir::new("context-no-global");
        let cwd = temp.join("project");
        write(&cwd.join("AGENTS.md"), "project instructions");
        let (files, warnings) = load_project_context_files(&cwd.to_string_lossy(), None);
        assert!(warnings.is_empty());
        assert_eq!(
            files
                .into_iter()
                .map(|file| file.content)
                .collect::<Vec<_>>(),
            ["project instructions"]
        );
    }

    #[cfg(unix)]
    /// Pi's `linkWorktree`.
    fn link_worktree(main: &Path, worktree: &Path, name: &str) {
        let git_dir = main.join(".git/worktrees").join(name);
        write(&main.join(".git/HEAD"), "ref: refs/heads/main\n");
        write(&git_dir.join("HEAD"), "ref: refs/heads/feat\n");
        write(&git_dir.join("commondir"), "../..");
        write(
            &worktree.join(".git"),
            &format!("gitdir: {}\n", git_dir.to_string_lossy()),
        );
    }

    #[cfg(unix)]
    struct Nested {
        temp: TempDir,
        outer: PathBuf,
        main: PathBuf,
        worktree: PathBuf,
        src: PathBuf,
    }

    #[cfg(unix)]
    /// Pi's `setupNestedWorktree`.
    fn nested(name: &str) -> Nested {
        let temp = TempDir::new(name);
        let outer = temp.join("outer");
        let main = outer.join("main");
        let worktree = main.join("worktrees").join("feat");
        let src = worktree.join("src");
        std::fs::create_dir_all(&src).expect("dirs");
        link_worktree(&main, &worktree, "feat");
        Nested {
            temp,
            outer,
            main,
            worktree,
            src,
        }
    }

    #[cfg(unix)]
    #[test]
    fn nested_worktrees_drop_only_the_shadowed_file() {
        // "should skip the main repo's duplicate when the worktree root has
        // its own context"
        let n = nested("wt-skip");
        write(&n.main.join("AGENTS.md"), "main repo instructions");
        write(&n.worktree.join("AGENTS.md"), "worktree instructions");
        assert_eq!(
            contents(&n.src, &n.temp.join("agent")),
            ["worktree instructions"]
        );

        // "should still inherit the main repo's context when the worktree
        // root has none"
        let n = nested("wt-inherit");
        write(&n.main.join("AGENTS.md"), "main repo instructions");
        assert_eq!(
            contents(&n.src, &n.temp.join("agent")),
            ["main repo instructions"]
        );

        // "should only skip the same filename, not a differently named
        // context file", with `AGENTS.override.md` for Pi's `CLAUDE.md`
        let n = nested("wt-names");
        write(&n.main.join("AGENTS.override.md"), "main repo instructions");
        write(&n.worktree.join("AGENTS.md"), "worktree instructions");
        assert_eq!(
            contents(&n.src, &n.temp.join("agent")),
            ["main repo instructions", "worktree instructions"]
        );

        // "should load the worktree's context once when it symlinks to the
        // main repo's file" (v1.2.0)
        let n = nested("wt-link");
        write(&n.main.join("AGENTS.md"), "main repo instructions");
        std::os::unix::fs::symlink(n.main.join("AGENTS.md"), n.worktree.join("AGENTS.md"))
            .expect("link");
        let (files, _) =
            load_project_context_files(&n.src.to_string_lossy(), Some(&n.temp.join("agent")));
        assert_eq!(
            files.into_iter().map(|file| file.path).collect::<Vec<_>>(),
            [n.worktree.join("AGENTS.md").to_string_lossy().into_owned()]
        );

        // "should skip the main repo's duplicate when the main repo's file
        // is a symlink" (v1.2.0)
        let n = nested("wt-shared");
        let shared = n.outer.join("shared-agents.md");
        write(&shared, "shared instructions");
        std::os::unix::fs::symlink(&shared, n.main.join("AGENTS.md")).expect("link");
        std::os::unix::fs::symlink(&shared, n.worktree.join("AGENTS.md")).expect("link");
        let (files, _) =
            load_project_context_files(&n.src.to_string_lossy(), Some(&n.temp.join("agent")));
        assert_eq!(
            files.into_iter().map(|file| file.path).collect::<Vec<_>>(),
            [n.worktree.join("AGENTS.md").to_string_lossy().into_owned()]
        );

        // "should keep loading ancestors above the main repo"
        let n = nested("wt-ancestors");
        write(&n.outer.join("AGENTS.md"), "outer instructions");
        write(&n.main.join("AGENTS.md"), "main repo instructions");
        write(&n.worktree.join("AGENTS.md"), "worktree instructions");
        assert_eq!(
            contents(&n.src, &n.temp.join("agent")),
            ["outer instructions", "worktree instructions"]
        );
    }

    #[cfg(unix)]
    #[test]
    fn other_layouts_keep_every_file() {
        // "should NOT skip the container's context in a bare layout"
        let temp = TempDir::new("wt-bare");
        let proj = temp.join("proj");
        let bare = proj.join(".bare");
        let worktree = proj.join("main");
        let git_dir = bare.join("worktrees").join("main");
        write(&bare.join("HEAD"), "ref: refs/heads/main\n");
        write(&git_dir.join("HEAD"), "ref: refs/heads/main\n");
        write(&git_dir.join("commondir"), "../..");
        write(
            &worktree.join(".git"),
            &format!("gitdir: {}\n", git_dir.to_string_lossy()),
        );
        write(&proj.join("AGENTS.md"), "container instructions");
        write(&worktree.join("AGENTS.md"), "worktree instructions");
        assert_eq!(
            contents(&worktree, &temp.join("agent")),
            ["container instructions", "worktree instructions"]
        );

        // "should NOT skip anything for a sibling worktree"
        let temp = TempDir::new("wt-sibling");
        let outer = temp.join("outer");
        let main = outer.join("main");
        let sib = outer.join("sib-feat");
        std::fs::create_dir_all(sib.join("src")).expect("dirs");
        std::fs::create_dir_all(&main).expect("dirs");
        write(&outer.join("AGENTS.md"), "outer instructions");
        write(&sib.join("AGENTS.md"), "sibling worktree instructions");
        link_worktree(&main, &sib, "sib");
        assert_eq!(
            contents(&sib.join("src"), &temp.join("agent")),
            ["outer instructions", "sibling worktree instructions"]
        );

        // "should NOT skip the superproject's context from inside a submodule"
        let temp = TempDir::new("wt-submodule");
        let sup = temp.join("super");
        let sub = sup.join("vendor").join("lib");
        std::fs::create_dir_all(sub.join("src")).expect("dirs");
        write(&sup.join("AGENTS.md"), "superproject instructions");
        write(&sub.join("AGENTS.md"), "submodule instructions");
        let sub_git = sup.join(".git/modules/vendor/lib");
        write(&sub_git.join("HEAD"), "ref: refs/heads/main\n");
        write(
            &sub.join(".git"),
            &format!("gitdir: {}\n", sub_git.to_string_lossy()),
        );
        assert_eq!(
            contents(&sub.join("src"), &temp.join("agent")),
            ["superproject instructions", "submodule instructions"]
        );

        // "should keep climbing past an ordinary repo root"
        let temp = TempDir::new("wt-ordinary");
        let outer = temp.join("outer");
        let repo = outer.join("repo");
        let leaf = repo.join("src");
        write(&repo.join(".git/HEAD"), "ref: refs/heads/main\n");
        write(&outer.join("AGENTS.md"), "outer instructions");
        write(&repo.join("AGENTS.md"), "repo instructions");
        write(&leaf.join("AGENTS.md"), "leaf instructions");
        assert_eq!(
            contents(&leaf, &temp.join("agent")),
            [
                "outer instructions",
                "repo instructions",
                "leaf instructions"
            ]
        );

        // "should climb normally when the gitdir: target does not exist"
        let temp = TempDir::new("wt-corrupt");
        let repo = temp.join("corrupt");
        let src = repo.join("src");
        write(
            &repo.join(".git"),
            "gitdir: /nonexistent/path/worktrees/feat\n",
        );
        write(&repo.join("AGENTS.md"), "repo instructions");
        write(&src.join("AGENTS.md"), "src instructions");
        assert_eq!(
            contents(&src, &temp.join("agent")),
            ["repo instructions", "src instructions"]
        );
    }

    #[test]
    fn prompt_inputs_read_files_or_pass_text() {
        let temp = TempDir::new("prompt-input");
        let file = temp.join("SYSTEM.md");
        write(&file, "From a file.");
        let mut warnings = Vec::new();
        assert_eq!(
            resolve_prompt_input(
                Some(&file.to_string_lossy()),
                "system prompt",
                &mut warnings
            )
            .as_deref(),
            Some("From a file.")
        );
        assert_eq!(
            resolve_prompt_input(Some("Literal text"), "system prompt", &mut warnings).as_deref(),
            Some("Literal text")
        );
        assert_eq!(resolve_prompt_input(Some(""), "x", &mut warnings), None);
        assert!(warnings.is_empty());
        // A directory exists but cannot be read as text.
        let dir = temp.path().to_string_lossy().into_owned();
        assert_eq!(
            resolve_prompt_input(Some(&dir), "system prompt", &mut warnings),
            Some(dir.clone())
        );
        assert_eq!(warnings.len(), 1);
        assert!(
            warnings[0].starts_with(&format!("Could not read system prompt file {dir}: ")),
            "{warnings:?}"
        );
    }
}
