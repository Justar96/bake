//! The `find` tool, ported from Pi `src/core/tools/find.ts` (v1.1.0; the
//! limit handling from v1.5.2; see the crate's `NOTICE`): `fd --glob` from
//! `PATH`, its paths made relative to the search directory with `/`
//! separators, limited to `limit` results and 50 KB. Outside a git
//! repository `--no-require-git` keeps `.gitignore` in force; a pattern
//! with `/` matches full paths. A limit below 1 means 1, a fractional one
//! is floored, and fd is asked for one result past it, so the limit notice
//! appears only when more results exist.
//!
//! # Deviations from Pi
//!
//! - **No download.** Pi downloads fd when neither `fd` nor `fdfind` is on
//!   `PATH`; Bake fails with Pi's message ([`crate::tools::process`]).
//! - Output is kept only as far as its 50 KB head; the result is the same.
//! - Pi's pluggable `FindOperations` (a custom `glob`) are not ported.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use bake_agent::{AgentTool, AgentToolResult, ToolReplay};
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
        move |_id, args, signal, _update, _ctx| {
            boxed(execute(cwd.clone(), owner.clone(), args, signal))
        },
    );
    tool.read_only = true;
    tool.replay = Some(ToolReplay::Safe);
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

/// Pi v1.5.2's `effectiveLimit`, `Math.max(1, Math.floor(limit))`: fd
/// reads `--max-results 0` as no limit.
pub(crate) fn effective_limit(limit: Option<f64>) -> f64 {
    limit.unwrap_or(DEFAULT_LIMIT).floor().max(1.0)
}

/// Pi's `fd` arguments for a search returning at most `limit` results:
/// fd is asked for one more, which tells "exactly `limit`" apart from
/// "more than `limit`".
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
    args.push(number_to_string(limit + 1.0).into());
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

/// Pi's handling of fd's output, a line at a time (v1.5.2).
pub(crate) struct FindCollector {
    search_path: PathBuf,
    limit: f64,
    results: f64,
    limit_reached: bool,
    output: HeadCollector,
}

impl FindCollector {
    /// `limit` is the [`effective_limit`].
    pub(crate) fn new(search_path: PathBuf, limit: f64) -> Self {
        Self {
            search_path,
            limit,
            results: 0.0,
            limit_reached: false,
            output: HeadCollector::new(TruncationOptions {
                max_lines: MAX_SAFE_INTEGER,
                max_bytes: DEFAULT_MAX_BYTES,
            }),
        }
    }

    pub(crate) fn line(&mut self, raw: &[u8]) {
        let text = String::from_utf8_lossy(raw);
        let line = trim(text.strip_suffix('\r').unwrap_or(&text));
        if line.is_empty() {
            return;
        }
        // A result past the limit only shows that more exist.
        if self.results == self.limit {
            self.limit_reached = true;
            return;
        }
        self.results += 1.0;
        self.output
            .push(&relativize_find_result_path(line, &self.search_path));
    }

    pub(crate) fn finish(self, ended: Ended) -> Result<AgentToolResult, String> {
        let (code, stderr) = match ended {
            Ended::Aborted => return Err(ABORTED.into()),
            Ended::Exited { code, stderr, .. } => (code, stderr),
        };
        if code != Some(0) && self.results == 0.0 {
            let message = trim(&stderr);
            return Err(if message.is_empty() {
                format!("fd exited with code {}", process::code_text(code))
            } else {
                message.to_owned()
            });
        }
        if self.results == 0.0 {
            return Ok(text_result("No files found matching pattern", None));
        }
        let limit_reached = self.limit_reached;
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
    let limit = effective_limit(number_arg(&args, "limit"));
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
    let ended = process::run_lines(
        &fd,
        &fd_args,
        None,
        owner.as_ref(),
        signal.as_ref(),
        |line| {
            collector.line(&line);
            std::future::ready(Flow::Continue)
        },
    )
    .await
    .map_err(|error| format!("Failed to run fd: {error}"))?;
    collector.finish(ended)
}

// Every test here is Unix-only.
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::tools::truncate::truncate_head;

    /// fd's output for `names` under `/w`, as the collector receives it,
    /// and the collector's result.
    fn collect(limit: f64, names: &[String]) -> AgentToolResult {
        let mut collector = FindCollector::new(PathBuf::from("/w"), limit);
        for name in names {
            collector.line(format!("/w/{name}").as_bytes());
        }
        collector
            .finish(Ended::Exited {
                code: Some(0),
                stderr: String::new(),
                stopped: false,
            })
            .expect("result")
    }

    fn text(result: &AgentToolResult) -> String {
        serde_json::to_value(&result.content).expect("content")[0]["text"]
            .as_str()
            .expect("text")
            .to_owned()
    }

    fn max_results(limit: f64) -> String {
        let args = fd_args("*.txt", Path::new("/w"), limit);
        let at = args
            .iter()
            .position(|arg| arg == "--max-results")
            .expect("--max-results");
        args[at + 1].to_string_lossy().into_owned()
    }

    fn names(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    // Pi v1.5.2 `tools.test.ts` "should report the result limit only when
    // more results exist" (#7121, #11747), with fd's output for three
    // files under each `--max-results`.
    #[test]
    fn reports_the_result_limit_only_when_more_results_exist() {
        assert_eq!(max_results(3.0), "4");
        let exact = collect(3.0, &names(&["a.txt", "b.txt", "c.txt"]));
        assert_eq!(text(&exact), "a.txt\nb.txt\nc.txt");
        assert_eq!(exact.details, None);

        assert_eq!(max_results(2.0), "3");
        let over = collect(2.0, &names(&["a.txt", "b.txt", "c.txt"]));
        assert_eq!(
            text(&over),
            "a.txt\nb.txt\n\n[2 results limit reached. Use limit=4 for more, or refine pattern]"
        );
        assert_eq!(over.details, Some(json!({ "resultLimitReached": 2 })));
    }

    // Pi v1.5.2 "should treat limits below 1 as 1 and floor fractional
    // limits" (#11747).
    #[test]
    fn limits_below_one_mean_one_and_fractions_are_floored() {
        for limit in [0.0, -5.0, 0.5, 1.9] {
            let effective = effective_limit(Some(limit));
            assert_eq!(effective, 1.0, "{limit}");
            assert_eq!(max_results(effective), "2", "{limit}");
            let result = collect(effective, &names(&["a.txt", "b.txt"]));
            assert_eq!(
                text(&result),
                "a.txt\n\n[1 results limit reached. Use limit=2 for more, or refine pattern]",
                "{limit}"
            );
            assert_eq!(result.details, Some(json!({ "resultLimitReached": 1 })));
        }
        assert_eq!(effective_limit(None), 1000.0);
    }

    // Pi v1.5.2 "should report full output totals past the byte cap": 300
    // names of 200 bytes, about 60 KB.
    #[test]
    fn totals_past_the_byte_cap_are_those_of_the_full_output() {
        let names: Vec<String> = (0..300)
            .map(|index| format!("{index:03}{}.txt", "x".repeat(193)))
            .collect();
        let result = collect(effective_limit(None), &names);
        let expected = truncate_head(
            &names.join("\n"),
            TruncationOptions {
                max_lines: MAX_SAFE_INTEGER,
                max_bytes: DEFAULT_MAX_BYTES,
            },
        );
        let details = result.details.clone().expect("details");
        let mut truncation = json!(expected);
        truncation["content"] = details["truncation"]["content"].clone();
        assert_eq!(details["truncation"], truncation);
        assert_eq!(
            details["truncation"]["content"]
                .as_str()
                .expect("content")
                .split('\n')
                .count(),
            expected.output_lines
        );
        assert!(details.get("resultLimitReached").is_none());
        assert!(text(&result).ends_with("\n\n[50.0KB limit reached]"));
    }

    // Pi v1.5.2: fd's output of only blank lines finds nothing, and a
    // failed run is an error only when it found nothing.
    #[test]
    fn blank_output_finds_nothing() {
        let ended = |code| Ended::Exited {
            code: Some(code),
            stderr: String::new(),
            stopped: false,
        };
        let blank = |code| {
            let mut collector = FindCollector::new(PathBuf::from("/w"), 1000.0);
            collector.line(b"");
            collector.line(b"  \r");
            collector.finish(ended(code))
        };
        assert_eq!(
            text(&blank(0).expect("result")),
            "No files found matching pattern"
        );
        assert_eq!(blank(1).err().as_deref(), Some("fd exited with code 1"));
    }

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
