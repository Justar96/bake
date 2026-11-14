//! Tasks: what a sample is asked to do and how its result is judged.
//!
//! A task is a directory named in lowercase letters, digits, and `-`:
//!
//! - `prompt.md`: the prompt, sent as written without its trailing newline.
//! - `workspace/`: the files the agent starts with, copied for each sample.
//! - `check.sh`: run with `sh` in the workspace after the agent; exit 0 is a
//!   pass. `BAKE_EVAL_TASK_DIR` names the task directory, so a check can
//!   hold files the agent never sees, and `BAKE_EVAL_ANSWER` names a file
//!   holding the agent's final answer.
//! - `setup.sh`, optional: run with `sh` in the workspace before the agent,
//!   with `BAKE_EVAL_TASK_DIR`, to generate files too large to commit.
//! - `solution/`, optional: files that, copied over the workspace, make the
//!   check pass, and `solution/ANSWER.txt` as the answer. It is never shown
//!   to the agent; [`crate::run::check_tasks`] uses it to prove the check
//!   can pass and that it fails on the untouched workspace.
//! - `home/`, optional: files copied into the sample's home directory, such
//!   as user-level skills.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The answer file inside a task's `solution/`.
pub const SOLUTION_ANSWER: &str = "ANSWER.txt";

/// A loaded task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    /// Its directory name.
    pub name: String,
    /// Its directory.
    pub dir: PathBuf,
    /// The prompt.
    pub prompt: String,
    /// Whether it has a `setup.sh`.
    pub setup: bool,
}

impl Task {
    /// The workspace template.
    pub fn workspace(&self) -> PathBuf {
        self.dir.join("workspace")
    }

    /// The solution overlay, when the task has one.
    pub fn solution(&self) -> Option<PathBuf> {
        let dir = self.dir.join("solution");
        dir.is_dir().then_some(dir)
    }

    /// The home template, when the task has one.
    pub fn home(&self) -> Option<PathBuf> {
        let dir = self.dir.join("home");
        dir.is_dir().then_some(dir)
    }
}

/// Whether `name` can name a task: lowercase letters, digits, and `-`,
/// starting with a letter or digit.
pub fn is_task_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    bytes
        .first()
        .is_some_and(|first| first.is_ascii_lowercase() || first.is_ascii_digit())
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
}

fn load_task(dir: &Path, name: String) -> Result<Task, String> {
    let prompt = fs::read_to_string(dir.join("prompt.md"))
        .map_err(|error| format!("task {name}: cannot read prompt.md: {error}"))?;
    let prompt = prompt.trim_end().to_owned();
    if prompt.is_empty() {
        return Err(format!("task {name}: prompt.md is empty"));
    }
    if !dir.join("workspace").is_dir() {
        return Err(format!("task {name}: no workspace directory"));
    }
    if !dir.join("check.sh").is_file() {
        return Err(format!("task {name}: no check.sh"));
    }
    Ok(Task {
        setup: dir.join("setup.sh").is_file(),
        name,
        dir: dir.to_path_buf(),
        prompt,
    })
}

/// The tasks in `dir`, by name. With `only` not empty, just the tasks it
/// names, each of which must exist.
pub fn load_tasks(dir: &Path, only: &[String]) -> Result<Vec<Task>, String> {
    let entries = fs::read_dir(dir)
        .map_err(|error| format!("cannot read the task directory {}: {error}", dir.display()))?;
    let mut tasks = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| format!("cannot read {}: {error}", dir.display()))?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if !is_task_name(&name) {
            return Err(format!(
                "task directory {name:?}: a name is lowercase letters, digits, and -"
            ));
        }
        if only.is_empty() || only.contains(&name) {
            tasks.push(load_task(&path, name)?);
        }
    }
    for name in only {
        if !tasks.iter().any(|task| &task.name == name) {
            return Err(format!("no task named {name} in {}", dir.display()));
        }
    }
    if tasks.is_empty() {
        return Err(format!("no tasks in {}", dir.display()));
    }
    tasks.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(tasks)
}

/// Copies the files and directories under `from` into `to`, creating `to`
/// and replacing files already there. A link or special file is refused: a
/// template holds only what it shows.
pub fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let target = to.join(entry.file_name());
        if kind.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else if kind.is_file() {
            fs::copy(entry.path(), &target)?;
        } else {
            return Err(io::Error::other(format!(
                "{} is not a file or directory",
                entry.path().display()
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("bake-eval-task-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("scratch");
        dir
    }

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
        fs::write(path, text).expect("write");
    }

    #[test]
    fn names_are_lowercase_words() {
        for name in ["fix-test", "a", "7-up", "log2"] {
            assert!(is_task_name(name), "{name}");
        }
        for name in ["", "-fix", "Fix", "fix_test", "fix test", "fix/..", "."] {
            assert!(!is_task_name(name), "{name}");
        }
    }

    #[test]
    fn tasks_load_sorted_and_filtered() {
        let dir = scratch("load");
        for name in ["beta", "alpha"] {
            write(&dir.join(name).join("prompt.md"), "Do it.\n\n");
            write(&dir.join(name).join("check.sh"), "exit 0\n");
            write(&dir.join(name).join("workspace/file.txt"), "x");
        }
        write(&dir.join("alpha/setup.sh"), "true\n");
        write(&dir.join("notes.txt"), "not a task");
        let tasks = load_tasks(&dir, &[]).expect("tasks");
        assert_eq!(
            tasks
                .iter()
                .map(|task| task.name.as_str())
                .collect::<Vec<_>>(),
            ["alpha", "beta"]
        );
        assert_eq!(tasks[0].prompt, "Do it.");
        assert!(tasks[0].setup && !tasks[1].setup);
        let only = load_tasks(&dir, &["beta".to_owned()]).expect("only");
        assert_eq!(only.len(), 1);
        assert_eq!(
            load_tasks(&dir, &["gamma".to_owned()]).unwrap_err(),
            format!("no task named gamma in {}", dir.display())
        );
        fs::remove_dir_all(&dir).expect("clean");
    }

    #[test]
    fn incomplete_tasks_are_named() {
        let dir = scratch("incomplete");
        write(&dir.join("one/prompt.md"), "Do it.");
        assert_eq!(
            load_tasks(&dir, &[]).unwrap_err(),
            "task one: no workspace directory"
        );
        write(&dir.join("one/workspace/a"), "");
        assert_eq!(load_tasks(&dir, &[]).unwrap_err(), "task one: no check.sh");
        write(&dir.join("one/check.sh"), "");
        write(&dir.join("one/prompt.md"), " \n");
        assert_eq!(
            load_tasks(&dir, &[]).unwrap_err(),
            "task one: prompt.md is empty"
        );
        fs::remove_dir_all(&dir).expect("clean");
    }

    #[test]
    fn trees_copy_over_existing_files() {
        let dir = scratch("copy");
        write(&dir.join("from/a.txt"), "new");
        write(&dir.join("from/sub/b.txt"), "b");
        write(&dir.join("to/a.txt"), "old");
        write(&dir.join("to/kept.txt"), "kept");
        copy_tree(&dir.join("from"), &dir.join("to")).expect("copy");
        let read = |path: &str| fs::read_to_string(dir.join(path)).expect("read");
        assert_eq!(read("to/a.txt"), "new");
        assert_eq!(read("to/sub/b.txt"), "b");
        assert_eq!(read("to/kept.txt"), "kept");
        fs::remove_dir_all(&dir).expect("clean");
    }

    #[cfg(unix)]
    #[test]
    fn links_are_refused() {
        let dir = scratch("link");
        write(&dir.join("from/a.txt"), "a");
        std::os::unix::fs::symlink(dir.join("from/a.txt"), dir.join("from/link")).expect("link");
        assert!(copy_tree(&dir.join("from"), &dir.join("to")).is_err());
        fs::remove_dir_all(&dir).expect("clean");
    }
}
