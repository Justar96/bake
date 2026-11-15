//! Context files found while working, the `nestedContextFiles` setting.
//!
//! A successful `read`, `edit`, `write`, `grep`, `find`, or `ls` call, the
//! model's own or a script's through `codemode`, on a path below the
//! session's working directory makes each directory from just below the
//! working directory down to the path's directory (the path itself when it
//! is one) be checked as the startup search checks a directory: the first
//! of `AGENTS.override.md`, `AGENTS.md`, and `AGENTS.MD`
//! ([`crate::system_prompt::context`]). A file found there that the session
//! has not loaded joins the `nested_context` section, outermost first,
//! before the next model request ([`section_patch`]); `bash` is not
//! inspected.
//!
//! The set of files is recorded as `nested-context` custom entries, each
//! naming the files one call added, so the current branch's entries give
//! the set back on resume, in a fork, after `/tree`, and after compaction
//! ([`recorded_paths`]). The files' text is read again when the set is
//! restored, as the startup context files are read at each start.
//!
//! The section holds at most [`MAX_NESTED_CONTEXT_BYTES`] of file text; a
//! file that would pass that is left out with a warning for the user.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use bake_ai::{Message, SystemContent, SystemMessage};
use serde_json::{Value, json};

use super::recovery::Core;
use crate::session::{SessionEntry, SessionManager};
use crate::system_prompt::ContextFile;
use crate::system_prompt::context::{context_file_in, shadowed_context_file};

/// The custom entry type that records the files one call added.
pub const ENTRY_TYPE: &str = "nested-context";

/// The prompt section the files found while working are in.
pub const SECTION: &str = "nested_context";

/// The most file text the section holds, in bytes.
pub const MAX_NESTED_CONTEXT_BYTES: usize = 64 * 1024;

/// The file tools whose `path` is observed.
const FILE_TOOLS: [&str; 6] = ["read", "edit", "write", "grep", "find", "ls"];

/// What one observed call found: the files added and the warnings for the
/// user.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Found {
    /// The paths of the files added, outermost first.
    pub loaded: Vec<String>,
    /// Why a file was left out or could not be read.
    pub warnings: Vec<String>,
}

/// One session's files found while working.
pub struct NestedContext {
    /// Whether `nestedContextFiles` is on and context files are loaded.
    enabled: bool,
    /// The working directory, absolute and normalized.
    cwd: PathBuf,
    /// The context files loaded at the start.
    startup: Vec<String>,
    /// The files found, in the order found.
    files: Mutex<Vec<ContextFile>>,
}

impl NestedContext {
    /// The session's set, empty, for `cwd` with `startup` loaded at the
    /// start; it observes nothing unless `enabled`.
    pub fn new(enabled: bool, cwd: &str, startup: &[ContextFile]) -> Self {
        Self {
            enabled,
            cwd: crate::tools::path_utils::resolve_to_cwd(cwd, Path::new("/"))
                .unwrap_or_else(|_| PathBuf::from(cwd)),
            startup: startup.iter().map(|file| file.path.clone()).collect(),
            files: Mutex::default(),
        }
    }

    fn files(&self) -> std::sync::MutexGuard<'_, Vec<ContextFile>> {
        self.files.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Makes the set the files `paths` names, in order: a file already
    /// held keeps its text, another is read again. A file that can no
    /// longer be read, or would pass the bound, is left out.
    pub fn restore(&self, paths: &[String]) {
        if !self.enabled {
            return;
        }
        let mut files = self.files();
        let held = std::mem::take(&mut *files);
        let mut bytes = 0;
        for path in paths {
            let file = held
                .iter()
                .find(|file| &file.path == path)
                .cloned()
                .or_else(|| read_context_file(Path::new(path)));
            if let Some(file) = file
                && bytes + file.content.len() <= MAX_NESTED_CONTEXT_BYTES
                && !files.iter().any(|known| known.path == file.path)
            {
                bytes += file.content.len();
                files.push(file);
            }
        }
    }

    /// The paths of the files found, in order.
    #[cfg(test)]
    pub fn paths(&self) -> Vec<String> {
        self.files().iter().map(|file| file.path.clone()).collect()
    }

    /// The directories a successful call of `tool` with `args` makes this
    /// session check, outermost first; empty for any other call.
    pub fn directories(&self, tool: &str, args: &Value) -> Vec<PathBuf> {
        if !self.enabled || !FILE_TOOLS.contains(&tool) {
            return Vec::new();
        }
        let Some(path) = args
            .get("path")
            .and_then(Value::as_str)
            .filter(|path| !path.is_empty())
        else {
            return Vec::new();
        };
        let Ok(path) = crate::tools::path_utils::resolve_to_cwd(path, &self.cwd) else {
            return Vec::new();
        };
        let Ok(below) = path.strip_prefix(&self.cwd) else {
            return Vec::new();
        };
        let below = if path.is_dir() {
            below.to_path_buf()
        } else {
            below.parent().map(Path::to_path_buf).unwrap_or_default()
        };
        let mut directories = Vec::new();
        let mut current = self.cwd.clone();
        for component in below.components() {
            let std::path::Component::Normal(name) = component else {
                return Vec::new();
            };
            current.push(name);
            directories.push(current.clone());
        }
        directories
    }

    /// Checks `directories`, outermost first, and adds each context file
    /// the session has not loaded, within the bound, once `record` has
    /// written the paths being added. Holding the set throughout, so two
    /// calls never add one file twice.
    pub fn observe(
        &self,
        directories: &[PathBuf],
        record: impl FnOnce(&[String]) -> Result<(), String>,
    ) -> Found {
        let mut found = Found::default();
        if directories.is_empty() {
            return found;
        }
        let mut files = self.files();
        let mut bytes: usize = files.iter().map(|file| file.content.len()).sum();
        let mut added: Vec<ContextFile> = Vec::new();
        let Some(innermost) = directories.last() else {
            return found;
        };
        let shadowed = shadowed_context_file(innermost);
        for directory in directories {
            let Some(file) = context_file_in(directory, &mut found.warnings) else {
                continue;
            };
            let is_shadowed = shadowed.as_ref().is_some_and(|shadowed| {
                Path::new(&file.path).file_name().is_some_and(|name| {
                    std::fs::canonicalize(directory)
                        .unwrap_or_else(|_| directory.clone())
                        .join(name)
                        == *shadowed
                })
            });
            let known = self.startup.contains(&file.path)
                || files.iter().any(|known| known.path == file.path)
                || added.iter().any(|known| known.path == file.path);
            if is_shadowed || known {
                continue;
            }
            if bytes + file.content.len() > MAX_NESTED_CONTEXT_BYTES {
                found.warnings.push(format!(
                    "Skipped context file {}: the context files found while working may hold at most {} KiB.",
                    file.path,
                    MAX_NESTED_CONTEXT_BYTES / 1024
                ));
                continue;
            }
            bytes += file.content.len();
            added.push(file);
        }
        if added.is_empty() {
            return found;
        }
        let paths: Vec<String> = added.iter().map(|file| file.path.clone()).collect();
        if let Err(error) = record(&paths) {
            found
                .warnings
                .push(format!("Could not record the context files found: {error}"));
            return found;
        }
        files.extend(added);
        found.loaded = paths;
        found
    }

    /// The section's text, unwrapped; `None` while no file is found.
    pub fn section(&self) -> Option<String> {
        let files = self.files();
        (!files.is_empty()).then(|| render(&files))
    }
}

/// The section's text for `files`.
fn render(files: &[ContextFile]) -> String {
    let mut parts = vec![
        "Instructions for subdirectories, found while working; each applies to the files under its directory:"
            .to_owned(),
    ];
    parts.extend(files.iter().map(|file| {
        format!(
            "<project_instructions path=\"{}\">\n{}\n</project_instructions>",
            file.path, file.content
        )
    }));
    parts.join("\n\n")
}

/// The section as the prompt carries it, in its tags.
pub fn wrapped(section: &str) -> String {
    format!("<{SECTION}>\n{section}\n</{SECTION}>")
}

/// The system message that brings `messages`' `nested_context` section to
/// `section`, or `None` when it already reads so.
pub fn section_patch(messages: &[Message], section: Option<&str>) -> Option<SystemMessage> {
    let section = section?;
    let current = bake_ai::transcript::get_current_system_message(messages)
        .and_then(|system| system.sections)
        .and_then(|sections| {
            sections
                .0
                .into_iter()
                .find(|(name, _)| name == SECTION)
                .and_then(|(_, text)| text)
        });
    let text = wrapped(section);
    (current.as_deref() != Some(text.as_str())).then(|| SystemMessage {
        content: SystemContent::Text(String::new()),
        sections: Some(bake_ai::Sections(vec![(SECTION.to_owned(), Some(text))])),
        tools_added: None,
        tools_removed: None,
        timestamp: bake_ai::now_ms(),
    })
}

/// The files the branch's `nested-context` entries name, in order, each
/// once.
pub fn recorded_paths(branch: &[&SessionEntry]) -> Vec<String> {
    let mut paths: Vec<String> = Vec::new();
    for entry in branch {
        if entry.entry_type() != "custom"
            || entry.get("customType").and_then(Value::as_str) != Some(ENTRY_TYPE)
        {
            continue;
        }
        let named = entry
            .get("data")
            .and_then(|data| data.get("paths"))
            .and_then(Value::as_array);
        for path in named.into_iter().flatten().filter_map(Value::as_str) {
            if !paths.iter().any(|known| known == path) {
                paths.push(path.to_owned());
            }
        }
    }
    paths
}

/// Checks `directories` for `core`'s session off the async workers,
/// records what it adds, and tells the listeners what was added or left
/// out ([`crate::agent_session::SessionEvent::NestedContext`]).
pub(super) async fn find(core: &Arc<Core>, directories: Vec<PathBuf>) {
    let Ok(task) = core.owned_context_commit(move |core| {
        core.nested.observe(&directories, |paths| {
            super::write_session(&core.session, &core.gap, |session| record(session, paths))
                .map_err(|refused| refused.message())
        })
    }) else {
        return;
    };
    let Ok(found) = task.await else {
        return;
    };
    if !found.loaded.is_empty() || !found.warnings.is_empty() {
        core.emit(&super::SessionEvent::NestedContext {
            loaded: &found.loaded,
            warnings: &found.warnings,
        });
    }
}

/// Records that `paths` were found.
pub fn record(session: &mut SessionManager, paths: &[String]) -> Result<(), String> {
    session
        .append_custom_entry(ENTRY_TYPE, Some(json!({ "paths": paths })))
        .map(drop)
        .map_err(|error| error.to_string())
}

/// The context file at `path`, read as the startup search reads one.
fn read_context_file(path: &Path) -> Option<ContextFile> {
    let directory = path.parent()?;
    let file = context_file_in(directory, &mut Vec::new())?;
    (Path::new(&file.path) == path).then_some(file)
}
