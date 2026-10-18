//! The `grep` tool, ported from Pi `src/core/tools/grep.ts` (v1.1.0; see
//! the crate's `NOTICE`): `rg --json` from `PATH`, its matches formatted as
//! `path:line: text` with `path-line- text` context lines, limited to
//! `limit` matches and 50 KB, with lines cut to 500 characters.
//!
//! Context lines and lines `rg` reports as bytes come from the file, split
//! on `\r\n`, `\r`, and `\n` as Pi splits them.
//!
//! # Deviations from Pi
//!
//! - **No download.** Pi downloads ripgrep when it is not on `PATH`; Bake
//!   fails with Pi's message ([`crate::tools::process`]).
//! - **Streaming.** Pi formats after `rg` exits and reads each file with
//!   matches whole. Bake formats each match as it arrives, keeps only the
//!   output its 50 KB head can show, reads a file whole only up to 1 MiB
//!   (larger ones are streamed up to the lines needed), and keeps at most
//!   500 characters of any line; the output and details are the same.
//! - `rg` output lines over 16 MiB are skipped.
//! - Pi's pluggable `GrepOperations` are not ported.

use std::ffi::OsString;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use bake_agent::{AgentTool, AgentToolResult};
use bake_ai::AbortSignal;
use serde_json::{Map, Value, json};

use crate::owned_work::WorkPermit;
use crate::tools::definition::{
    ABORTED, ToolDefinition, aborted, admit, blocking_under, bool_arg, boxed, number_arg,
    optional_string_arg, string_arg, text_result,
};
use crate::tools::head::HeadCollector;
use crate::tools::js::{as_index, number_to_string, number_value, trim};
use crate::tools::ls::MAX_SAFE_INTEGER;
use crate::tools::path_utils::resolve_to_cwd;
use crate::tools::process::{self, Ended, Flow, node_basename, node_relative};
use crate::tools::truncate::{
    DEFAULT_MAX_BYTES, GREP_MAX_LINE_LENGTH, TruncationOptions, format_size, truncate_line,
};

/// Pi's `grepToolSystemPromptContribution` snippet; the definition has no
/// `promptGuidelines`.
pub const GREP_PROMPT_SNIPPET: &str = "Search file contents for patterns (respects .gitignore)";

const DEFAULT_LIMIT: f64 = 100.0;

/// Files up to this size are read whole and kept for later matches.
const CACHED_FILE_BYTES: u64 = 1024 * 1024;

/// Bytes of a line kept: enough for [`GREP_MAX_LINE_LENGTH`] UTF-16 units
/// however the line decodes.
const KEPT_LINE_BYTES: usize = 2048;

fn parameters() -> Value {
    json!({
        "type": "object",
        "required": ["pattern"],
        "properties": {
            "pattern": { "type": "string", "description": "Search pattern (regex or literal string)" },
            "path": { "type": "string", "description": "Directory or file to search (default: current directory)" },
            "glob": { "type": "string", "description": "Filter files by glob pattern, e.g. '*.ts' or '**/*.spec.ts'" },
            "ignoreCase": { "type": "boolean", "description": "Case-insensitive search (default: false)" },
            "literal": { "type": "boolean", "description": "Treat pattern as literal string instead of regex (default: false)" },
            "context": { "type": "number", "description": "Number of lines to show before and after each match (default: 0)" },
            "limit": { "type": "number", "description": "Maximum number of matches to return (default: 100)" },
        },
    })
}

/// Pi's `createGrepToolDefinition`. Its `rg` processes have no
/// owner: a dropped call's tree is killed but not awaited.
pub fn create_grep_tool_definition(cwd: &Path) -> ToolDefinition {
    owned_grep_tool_definition(cwd, None)
}

/// [`create_grep_tool_definition`] whose `rg` processes `owner` owns
/// ([`crate::child_process`]), with the blocking work that formats their
/// output: its close waits for each call's tree and work.
pub(crate) fn owned_grep_tool_definition(
    cwd: &Path,
    owner: Option<crate::owned_work::WorkGate>,
) -> ToolDefinition {
    let cwd = cwd.to_path_buf();
    let mut tool = AgentTool::new(
        "grep",
        "grep",
        format!(
            "Search file contents for a pattern. Returns matching lines with file paths and line numbers. Respects .gitignore. Output is truncated to {} matches or {}KB (whichever is hit first). Long lines are truncated to {GREP_MAX_LINE_LENGTH} chars.",
            number_to_string(DEFAULT_LIMIT),
            DEFAULT_MAX_BYTES / 1024
        ),
        parameters(),
        move |_id, args, signal, _update| boxed(execute(cwd.clone(), owner.clone(), args, signal)),
    );
    tool.read_only = true;
    ToolDefinition {
        tool,
        prompt_snippet: Some(GREP_PROMPT_SNIPPET.into()),
        prompt_guidelines: None,
    }
}

/// Pi's `createGrepTool`.
pub fn create_grep_tool(cwd: &Path) -> AgentTool {
    create_grep_tool_definition(cwd).tool
}

/// The search a call asks for.
#[derive(Debug, Clone)]
pub(crate) struct GrepRequest {
    pub pattern: String,
    pub search_path: PathBuf,
    pub is_directory: bool,
    pub glob: Option<String>,
    pub ignore_case: bool,
    pub literal: bool,
    pub context: f64,
    pub limit: f64,
}

impl GrepRequest {
    /// Pi's `rg` arguments.
    pub(crate) fn args(&self) -> Vec<OsString> {
        let mut args: Vec<OsString> = ["--json", "--line-number", "--color=never", "--hidden"]
            .map(OsString::from)
            .to_vec();
        if self.ignore_case {
            args.push("--ignore-case".into());
        }
        if self.literal {
            args.push("--fixed-strings".into());
        }
        if let Some(glob) = &self.glob {
            args.push("--glob".into());
            args.push(glob.into());
        }
        args.push("--".into());
        args.push((&self.pattern).into());
        args.push(self.search_path.clone().into_os_string());
        args
    }

    /// Pi's `formatPath`.
    fn format_path(&self, file_path: &str) -> String {
        if self.is_directory {
            let relative = node_relative(&self.search_path, Path::new(file_path));
            if !relative.is_empty() && !relative.starts_with("..") {
                return relative.replace('\\', "/");
            }
        }
        node_basename(file_path)
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
    let Some(rg) = process::find_executable(&["rg"]) else {
        return Err("ripgrep (rg) is not available and could not be downloaded".into());
    };
    let path = optional_string_arg(&args, "path").filter(|path| !path.is_empty());
    let search_path = resolve_to_cwd(path.as_deref().unwrap_or("."), &cwd)?;
    let is_directory = match std::fs::metadata(&search_path) {
        Ok(metadata) => metadata.is_dir(),
        Err(_) => return Err(format!("Path not found: {}", search_path.display())),
    };
    let context = number_arg(&args, "context")
        .filter(|context| *context > 0.0)
        .unwrap_or(0.0);
    let request = GrepRequest {
        pattern: string_arg(&args, "pattern"),
        search_path,
        is_directory,
        glob: optional_string_arg(&args, "glob").filter(|glob| !glob.is_empty()),
        ignore_case: bool_arg(&args, "ignoreCase"),
        literal: bool_arg(&args, "literal"),
        context,
        limit: number_arg(&args, "limit").unwrap_or(DEFAULT_LIMIT).max(1.0),
    };
    // Held through the run, so each line's blocking work, which reads the
    // context lines, holds a share and the owner waits for it.
    let permit =
        admit(owner.as_ref()).map_err(|refused| format!("Failed to run ripgrep: {refused}"))?;
    let formatter = Arc::new(Mutex::new(GrepFormatter::new(request.clone())));
    let line_formatter = Arc::clone(&formatter);
    let line_signal = signal.clone();
    let ended = process::run_lines(
        &rg,
        &request.args(),
        owner.as_ref(),
        signal.as_ref(),
        move |line| {
            let formatter = Arc::clone(&line_formatter);
            let signal = line_signal.clone();
            let work = blocking_under(permit.as_ref().map(WorkPermit::share), move || {
                formatter
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .line(&line, signal.as_ref())
            });
            async move { work.await.unwrap_or(Flow::Stop) }
        },
    )
    .await
    .map_err(|error| format!("Failed to run ripgrep: {error}"))?;
    let formatter = std::mem::replace(
        &mut *formatter.lock().unwrap_or_else(PoisonError::into_inner),
        GrepFormatter::new(request),
    );
    formatter.finish(ended)
}

type Line = (String, bool);

fn cut_line(raw: &[u8], overflow: bool) -> Line {
    let decoded = String::from_utf8_lossy(raw);
    let cut = truncate_line(&decoded, GREP_MAX_LINE_LENGTH);
    (cut.text, cut.was_truncated || overflow)
}

/// Streams the lines of `path` as Pi's `getFileLines` splits them (on
/// `\r\n`, `\r`, and `\n`), each cut as `truncateLine` cuts it. Lines with
/// indices `from..=to` go to `visit`; reading stops after `to`. Returns the
/// line count when the whole file was read, `Some(None)` when it stopped
/// early, and `None` when the file cannot be read.
fn read_file_lines(
    path: &Path,
    from: usize,
    to: usize,
    signal: Option<&AbortSignal>,
    visit: &mut dyn FnMut(usize, Line),
) -> Option<Option<usize>> {
    let file = std::fs::File::open(path).ok()?;
    let mut reader = BufReader::with_capacity(64 * 1024, file);
    let mut index = 0usize;
    let mut raw: Vec<u8> = Vec::new();
    let mut overflow = false;
    let mut after_cr = false;
    loop {
        if signal.is_some_and(AbortSignal::aborted) {
            return None;
        }
        let buffer = reader.fill_buf().ok()?;
        if buffer.is_empty() {
            break;
        }
        let consumed = buffer.len();
        for byte in buffer {
            if after_cr {
                after_cr = false;
                if *byte == b'\n' {
                    continue;
                }
            }
            if *byte == b'\n' || *byte == b'\r' {
                if index >= from {
                    visit(index, cut_line(&raw, overflow));
                }
                raw.clear();
                overflow = false;
                after_cr = *byte == b'\r';
                if index >= to {
                    return Some(None);
                }
                index += 1;
                continue;
            }
            if index >= from && !overflow {
                if raw.len() < KEPT_LINE_BYTES {
                    raw.push(*byte);
                } else {
                    overflow = true;
                }
            }
        }
        reader.consume(consumed);
    }
    if index >= from {
        visit(index, cut_line(&raw, overflow));
    }
    Some(Some(index + 1))
}

/// A small file's lines, kept for the matches after the first.
struct CachedFile {
    path: String,
    lines: Option<Vec<Line>>,
}

/// Pi's grep formatting, a match at a time.
pub(crate) struct GrepFormatter {
    request: GrepRequest,
    match_count: f64,
    limit_reached: bool,
    lines_truncated: bool,
    output: HeadCollector,
    cache: Option<CachedFile>,
}

impl GrepFormatter {
    pub(crate) fn new(request: GrepRequest) -> Self {
        Self {
            request,
            match_count: 0.0,
            limit_reached: false,
            lines_truncated: false,
            output: HeadCollector::new(TruncationOptions {
                max_lines: MAX_SAFE_INTEGER,
                max_bytes: DEFAULT_MAX_BYTES,
            }),
            cache: None,
        }
    }

    fn emit(&mut self, line: &str) {
        self.output.push(line);
    }

    /// One line of `rg --json` output.
    pub(crate) fn line(&mut self, line: &[u8], signal: Option<&AbortSignal>) -> Flow {
        let text = String::from_utf8_lossy(line);
        let text = text.strip_suffix('\r').unwrap_or(&text);
        if trim(text).is_empty() || self.match_count >= self.request.limit {
            return Flow::Continue;
        }
        let Ok(event) = serde_json::from_str::<Value>(text) else {
            return Flow::Continue;
        };
        if event.get("type").and_then(Value::as_str) != Some("match") {
            return Flow::Continue;
        }
        self.match_count += 1.0;
        let data = event.get("data");
        let file_path = data
            .and_then(|data| data.get("path"))
            .and_then(|path| path.get("text"))
            .and_then(Value::as_str)
            .filter(|path| !path.is_empty());
        let line_number = data
            .and_then(|data| data.get("line_number"))
            .and_then(Value::as_f64);
        let line_text = data
            .and_then(|data| data.get("lines"))
            .and_then(|lines| lines.get("text"))
            .and_then(Value::as_str);
        if let (Some(file_path), Some(line_number)) = (file_path, line_number) {
            self.format_match(file_path, line_number, line_text, signal);
        }
        if self.match_count >= self.request.limit {
            self.limit_reached = true;
            return Flow::Stop;
        }
        Flow::Continue
    }

    fn format_match(
        &mut self,
        file_path: &str,
        line_number: f64,
        line_text: Option<&str>,
        signal: Option<&AbortSignal>,
    ) {
        let relative = self.request.format_path(file_path);
        if self.request.context == 0.0
            && let Some(text) = line_text
        {
            let sanitized = text.replace("\r\n", "\n").replace('\r', "");
            let sanitized = sanitized.strip_suffix('\n').unwrap_or(&sanitized);
            let cut = truncate_line(sanitized, GREP_MAX_LINE_LENGTH);
            self.lines_truncated |= cut.was_truncated;
            self.emit(&format!(
                "{relative}:{}: {}",
                number_to_string(line_number),
                cut.text
            ));
            return;
        }
        self.format_block(file_path, &relative, line_number, signal);
    }

    /// Visits the lines with 0-based indices `from..=to`; returns the
    /// file's line count when known, `Some(None)` when the file has more
    /// than `to + 1` lines, and `None` when it cannot be read.
    fn visit_lines(
        &mut self,
        file_path: &str,
        from: usize,
        to: usize,
        signal: Option<&AbortSignal>,
        visit: &mut dyn FnMut(&mut Self, usize, Line),
    ) -> Option<Option<usize>> {
        let path = Path::new(file_path);
        let small =
            std::fs::metadata(path).is_ok_and(|metadata| metadata.len() <= CACHED_FILE_BYTES);
        if !small {
            return read_file_lines(path, from, to, signal, &mut |index, line| {
                visit(self, index, line);
            });
        }
        let cached = match self.cache.take() {
            Some(cached) if cached.path == file_path => cached,
            _ => {
                let mut lines = Vec::new();
                let read = read_file_lines(path, 0, usize::MAX, signal, &mut |_, line| {
                    lines.push(line);
                });
                CachedFile {
                    path: file_path.to_owned(),
                    lines: read.map(|_| lines),
                }
            }
        };
        let result = cached.lines.as_ref().map(|lines| {
            let end = to.saturating_add(1).min(lines.len());
            for (index, line) in lines.iter().enumerate().take(end).skip(from) {
                visit(self, index, line.clone());
            }
            Some(lines.len())
        });
        self.cache = Some(cached);
        result
    }

    /// Pi's `formatBlock`.
    fn format_block(
        &mut self,
        file_path: &str,
        relative: &str,
        line_number: f64,
        signal: Option<&AbortSignal>,
    ) {
        let context = self.request.context;
        let start = if context > 0.0 {
            (line_number - context).max(1.0)
        } else {
            line_number
        };
        let wanted_end = if context > 0.0 {
            line_number + context
        } else {
            line_number
        };
        let last = (wanted_end - 1.0).floor().clamp(0.0, usize::MAX as f64) as usize;
        let emit_line = |this: &mut Self, current: f64, line: Line| {
            this.lines_truncated |= line.1;
            let number = number_to_string(current);
            if current == line_number {
                this.emit(&format!("{relative}:{number}: {}", line.0));
            } else {
                this.emit(&format!("{relative}-{number}- {}", line.0));
            }
        };
        match as_index(start - 1.0) {
            // Integral line numbers: each line of the window, in order.
            Some(first) => {
                let mut emitted = false;
                let read =
                    self.visit_lines(file_path, first, last, signal, &mut |this, index, line| {
                        emitted = true;
                        emit_line(this, index as f64 + 1.0, line);
                    });
                match read {
                    None => self.emit(&format!(
                        "{relative}:{}: (unable to read file)",
                        number_to_string(line_number)
                    )),
                    // Pi: a match past the end of the file shows an empty line.
                    Some(_) if !emitted && context == 0.0 => {
                        emit_line(self, line_number, (String::new(), false));
                    }
                    Some(_) => {}
                }
            }
            // Fractional line numbers read as `undefined`: empty lines.
            None => {
                let Some(total) = self.visit_lines(
                    file_path,
                    last.saturating_add(1),
                    last,
                    signal,
                    &mut |_, _, _| {},
                ) else {
                    self.emit(&format!(
                        "{relative}:{}: (unable to read file)",
                        number_to_string(line_number)
                    ));
                    return;
                };
                let end = match total {
                    Some(total) if context > 0.0 => (total as f64).min(wanted_end),
                    _ => wanted_end,
                };
                let mut current = start;
                while current <= end {
                    emit_line(self, current, (String::new(), false));
                    current += 1.0;
                    if signal.is_some_and(AbortSignal::aborted) {
                        return;
                    }
                }
            }
        }
    }

    /// Pi's `close` handler.
    pub(crate) fn finish(self, ended: Ended) -> Result<AgentToolResult, String> {
        let (code, stderr, stopped) = match ended {
            Ended::Aborted => return Err(ABORTED.into()),
            Ended::Exited {
                code,
                stderr,
                stopped,
            } => (code, stderr, stopped),
        };
        let killed_for_limit = stopped && self.limit_reached;
        if !killed_for_limit && code != Some(0) && code != Some(1) {
            let message = trim(&stderr);
            return Err(if message.is_empty() {
                format!("ripgrep exited with code {}", process::code_text(code))
            } else {
                message.to_owned()
            });
        }
        if self.match_count == 0.0 {
            return Ok(text_result("No matches found", None));
        }
        let limit = self.request.limit;
        let truncation = self.output.finish();
        let mut output = truncation.content.clone();
        let mut details = Map::new();
        let mut notices: Vec<String> = Vec::new();
        if self.limit_reached {
            notices.push(format!(
                "{} matches limit reached. Use limit={} for more, or refine pattern",
                number_to_string(limit),
                number_to_string(limit * 2.0)
            ));
            details.insert("matchLimitReached".into(), number_value(limit));
        }
        if truncation.truncated {
            notices.push(format!("{} limit reached", format_size(DEFAULT_MAX_BYTES)));
            details.insert("truncation".into(), json!(truncation));
        }
        if self.lines_truncated {
            notices.push(format!(
                "Some lines truncated to {GREP_MAX_LINE_LENGTH} chars. Use read tool to see full lines"
            ));
            details.insert("linesTruncated".into(), Value::Bool(true));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(path: &Path, from: usize, to: usize) -> (Vec<Line>, Option<Option<usize>>) {
        let mut lines = Vec::new();
        let total = read_file_lines(path, from, to, None, &mut |_, line| lines.push(line));
        (lines, total)
    }

    #[test]
    fn file_lines_split_like_pi() {
        let dir = std::env::temp_dir().join(format!("bake-grep-lines-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("f.txt");
        std::fs::write(&file, "a\r\nb\rc\n\nd").expect("write");
        let (all, total) = lines(&file, 0, usize::MAX);
        let texts: Vec<&str> = all.iter().map(|(text, _)| text.as_str()).collect();
        assert_eq!(texts, ["a", "b", "c", "", "d"]);
        assert_eq!(total, Some(Some(5)));
        let (window, total) = lines(&file, 1, 2);
        let texts: Vec<&str> = window.iter().map(|(text, _)| text.as_str()).collect();
        assert_eq!(texts, ["b", "c"]);
        assert_eq!(total, Some(None));
        std::fs::write(&file, "x".repeat(5000)).expect("write");
        let (long, _) = lines(&file, 0, usize::MAX);
        assert!(long[0].1);
        assert!(long[0].0.ends_with("... [truncated]"));
        assert_eq!(lines(&dir.join("missing"), 0, 0).1, None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
