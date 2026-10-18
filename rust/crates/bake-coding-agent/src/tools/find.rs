//! The `find` tool, ported from Pi `src/core/tools/find.ts` (v1.1.0; see
//! the crate's `NOTICE`): `fd --glob` from `PATH`, its paths made relative
//! to the search directory with `/` separators, limited to `limit` results
//! and 50 KB. Outside a git repository `--no-require-git` keeps
//! `.gitignore` in force; a pattern with `/` matches full paths.
//!
//! # Deviations from Pi
//!
//! - **No download.** Pi downloads fd when neither `fd` nor `fdfind` is on
//!   `PATH`; Bake fails with Pi's message ([`crate::tools::process`]).
//! - Output is kept only as far as its 50 KB head; the result is the same.
//! - Pi's pluggable `FindOperations` (a custom `glob`) are not ported.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use bake_agent::{AgentTool, AgentToolResult};
use bake_ai::AbortSignal;
use serde_json::{Map, Value, json};

use crate::tools::definition::{
    ABORTED, ToolDefinition, aborted, admit, blocking_under, boxed, number_arg,
    optional_string_arg, string_arg, text_result,
};
use crate::tools::head::HeadCollector;
use crate::tools::js::{number_to_string, number_value, trim};
use crate::tools::ls::MAX_SAFE_INTEGER;
use crate::tools::path_utils::{path_exists, resolve_to_cwd};
use crate::tools::process::{self, Ended, Flow, node_relative};
use crate::tools::truncate::{DEFAULT_MAX_BYTES, TruncationOptions, format_size};

/// Pi's `findToolSystemPromptContribution` snippet; the definition has no
/// `promptGuidelines`.
pub const FIND_PROMPT_SNIPPET: &str = "Find files by glob pattern (respects .gitignore)";

const DEFAULT_LIMIT: f64 = 1000.0;

fn parameters() -> Value {
    json!({
        "type": "object",
        "required": ["pattern"],
        "properties": {
            "pattern": {
                "type": "string",
                "description": "Glob pattern to match files, e.g. '*.ts', '**/*.json', or 'src/**/*.spec.ts'",
            },
            "path": { "type": "string", "description": "Directory to search in (default: current directory)" },
            "limit": { "type": "number", "description": "Maximum number of results (default: 1000)" },
        },
    })
}

/// Pi's `createFindToolDefinition`. Its `fd` processes have no
/// owner: a dropped call's tree is killed but not awaited.
pub fn create_find_tool_definition(cwd: &Path) -> ToolDefinition {
    owned_find_tool_definition(cwd, None)
}

/// [`create_find_tool_definition`] whose `fd` processes `owner` owns
/// ([`crate::child_process`]), with the blocking work that builds their
/// arguments: its close waits for each call's tree and work.
pub(crate) fn owned_find_tool_definition(
    cwd: &Path,
    owner: Option<crate::owned_work::WorkGate>,
) -> ToolDefinition {
    let cwd = cwd.to_path_buf();
    let mut tool = AgentTool::new(
        "find",
        "find",
        format!(
            "Search for files by glob pattern. Returns matching file paths relative to the search directory. Respects .gitignore. Output is truncated to {} results or {}KB (whichever is hit first).",
            number_to_string(DEFAULT_LIMIT),
            DEFAULT_MAX_BYTES / 1024
        ),
        parameters(),
        move |_id, args, signal, _update| boxed(execute(cwd.clone(), owner.clone(), args, signal)),
    );
    tool.read_only = true;
    ToolDefinition {
        tool,
        prompt_snippet: Some(FIND_PROMPT_SNIPPET.into()),
        prompt_guidelines: None,
    }
}

/// Pi's `createFindTool`.
pub fn create_find_tool(cwd: &Path) -> AgentTool {
    create_find_tool_definition(cwd).tool
}

/// Pi's `relativizeFindResultPath`.
pub fn relativize_find_result_path(result_path: &str, search_path: &Path) -> String {
    let separator = std::path::MAIN_SEPARATOR;
    let had_trailing =
        result_path.ends_with(separator) || (cfg!(windows) && result_path.ends_with('/'));
    let relative = if Path::new(result_path).is_absolute() {
        node_relative(search_path, Path::new(result_path))
    } else {
        result_path.to_owned()
    };
    let posix = relative.split(separator).collect::<Vec<_>>().join("/");
    if had_trailing && !posix.ends_with('/') {
        format!("{posix}/")
    } else {
        posix
    }
}

/// Pi's `fd` arguments for a search.
pub(crate) fn fd_args(pattern: &str, search_path: &Path, limit: f64) -> Vec<OsString> {
    let mut args: Vec<OsString> = ["--glob", "--color=never", "--hidden"]
        .map(OsString::from)
        .to_vec();
    let mut inside_git = false;
    let mut current = search_path;
    loop {
        if path_exists(&current.join(".git")) {
            inside_git = true;
            break;
        }
        match current.parent() {
            Some(parent) => current = parent,
            None => break,
        }
    }
    if !inside_git {
        args.push("--no-require-git".into());
    }
    args.push("--max-results".into());
    args.push(number_to_string(limit).into());
    let mut effective = pattern.to_owned();
    if pattern.contains('/') {
        args.push("--full-path".into());
        if !pattern.starts_with('/') && !pattern.starts_with("**/") && pattern != "**" {
            effective = format!("**/{pattern}");
        }
        if cfg!(windows) {
            effective = effective.replace('/', r"[/\\]");
        }
    }
    args.push("--".into());
    args.push(effective.into());
    args.push(search_path.as_os_str().to_owned());
    args
}

/// Pi's handling of fd's output, a line at a time.
pub(crate) struct FindCollector {
    search_path: PathBuf,
    limit: f64,
    raw_lines: usize,
    first_raw_empty: bool,
    results: f64,
    output: HeadCollector,
}

impl FindCollector {
    pub(crate) fn new(search_path: PathBuf, limit: f64) -> Self {
        Self {
            search_path,
            limit,
            raw_lines: 0,
            first_raw_empty: false,
            results: 0.0,
            output: HeadCollector::new(TruncationOptions {
                max_lines: MAX_SAFE_INTEGER,
                max_bytes: DEFAULT_MAX_BYTES,
            }),
        }
    }

    pub(crate) fn line(&mut self, raw: &[u8]) {
        let text = String::from_utf8_lossy(raw);
        if self.raw_lines == 0 {
            self.first_raw_empty = text.is_empty();
        }
        self.raw_lines += 1;
        let line = trim(text.strip_suffix('\r').unwrap_or(&text));
        if line.is_empty() {
            return;
        }
        self.results += 1.0;
        self.output
            .push(&relativize_find_result_path(line, &self.search_path));
    }

    /// Pi's `lines.join("\n")` is empty.
    fn output_empty(&self) -> bool {
        self.raw_lines == 0 || (self.raw_lines == 1 && self.first_raw_empty)
    }

    pub(crate) fn finish(self, ended: Ended) -> Result<AgentToolResult, String> {
        let (code, stderr) = match ended {
            Ended::Aborted => return Err(ABORTED.into()),
            Ended::Exited { code, stderr, .. } => (code, stderr),
        };
        if code != Some(0) && self.output_empty() {
            let message = trim(&stderr);
            return Err(if message.is_empty() {
                format!("fd exited with code {}", process::code_text(code))
            } else {
                message.to_owned()
            });
        }
        if self.output_empty() {
            return Ok(text_result("No files found matching pattern", None));
        }
        let limit_reached = self.results >= self.limit;
        let truncation = self.output.finish();
        let mut output = truncation.content.clone();
        let mut details = Map::new();
        let mut notices: Vec<String> = Vec::new();
        if limit_reached {
            notices.push(format!(
                "{} results limit reached. Use limit={} for more, or refine pattern",
                number_to_string(self.limit),
                number_to_string(self.limit * 2.0)
            ));
            details.insert("resultLimitReached".into(), number_value(self.limit));
        }
        if truncation.truncated {
            notices.push(format!("{} limit reached", format_size(DEFAULT_MAX_BYTES)));
            details.insert("truncation".into(), json!(truncation));
        }
        if !notices.is_empty() {
            output.push_str(&format!("\n\n[{}]", notices.join(". ")));
        }
        Ok(text_result(
            output,
            (!details.is_empty()).then_some(Value::Object(details)),
        ))
    }
}

async fn execute(
    cwd: PathBuf,
    owner: Option<crate::owned_work::WorkGate>,
    args: Value,
    signal: Option<AbortSignal>,
) -> Result<AgentToolResult, String> {
    if aborted(signal.as_ref()) {
        return Err(ABORTED.into());
    }
    let pattern = string_arg(&args, "pattern");
    let path = optional_string_arg(&args, "path").filter(|path| !path.is_empty());
    let search_path = resolve_to_cwd(path.as_deref().unwrap_or("."), &cwd)?;
    let limit = number_arg(&args, "limit").unwrap_or(DEFAULT_LIMIT);
    let Some(fd) = process::find_executable(&["fd", "fdfind"]) else {
        return Err("fd is not available and could not be downloaded".into());
    };
    let permit = admit(owner.as_ref()).map_err(|refused| format!("Failed to run fd: {refused}"))?;
    let fd_args = {
        let search_path = search_path.clone();
        let pattern = pattern.clone();
        blocking_under(permit, move || fd_args(&pattern, &search_path, limit))
            .await
            .map_err(|error| format!("Failed to run fd: {error}"))?
    };
    let mut collector = FindCollector::new(search_path, limit);
    let ended = process::run_lines(&fd, &fd_args, owner.as_ref(), signal.as_ref(), |line| {
        collector.line(&line);
        std::future::ready(Flow::Continue)
    })
    .await
    .map_err(|error| format!("Failed to run fd: {error}"))?;
    collector.finish(ended)
}

// Every test here is Unix-only.
#[cfg(all(test, unix))]
mod tests {
    use super::*;

    // Pi `test/suite/regressions/6104-find-root-relativization.test.ts`,
    // POSIX cases; the Windows cases need Windows paths.
    #[cfg(unix)]
    #[test]
    fn relativizes_like_pi_issue_6104() {
        let cases = [
            ("/home/user/file.txt", "/", "home/user/file.txt"),
            ("/home/user/project/", "/", "home/user/project/"),
            ("/home/user/file\\", "/home/user", "file\\"),
            (
                "/tmp/results/file.txt",
                "/workspace/project",
                "../../tmp/results/file.txt",
            ),
            (
                "/tmp/results/dir/",
                "/workspace/project",
                "../../tmp/results/dir/",
            ),
            ("relative/dir/", "/", "relative/dir/"),
        ];
        for (result, search, expected) in cases {
            assert_eq!(
                relativize_find_result_path(result, Path::new(search)),
                expected,
                "{result} under {search}"
            );
        }
    }
}
