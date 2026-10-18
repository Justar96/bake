//! The `ls` tool, ported from Pi `src/core/tools/ls.ts` (v1.1.0; see the
//! crate's `NOTICE`): a directory's entries sorted case-insensitively,
//! directories marked with `/`, dotfiles included, limited to `limit`
//! entries and 50 KB.
//!
//! # Deviations from Pi
//!
//! - **Collation.** Pi sorts with `localeCompare` on lowercased names, ICU's
//!   root collation. Bake orders ASCII as ICU does (whitespace, punctuation
//!   and symbols in ICU's order, digits, letters), a letter with accents
//!   after the plain letter (acute, grave, breve, circumflex, caron, ring,
//!   diaeresis, double acute, tilde, dot), and other characters after `z` by
//!   code point. Names outside Latin script may order differently from Pi.
//! - Pi's pluggable `LsOperations` are not ported.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};

use bake_agent::{AgentTool, AgentToolResult, ToolReplay};
use bake_ai::AbortSignal;
use serde_json::{Map, Value, json};

use crate::tools::definition::{
    ABORTED, ToolDefinition, boxed, number_arg, optional_string_arg, run_blocking_abortable,
    text_result,
};
use crate::tools::head::HeadCollector;
use crate::tools::js::{number_to_string, number_value};
use crate::tools::path_utils::{path_exists, resolve_to_cwd};
use crate::tools::truncate::{DEFAULT_MAX_BYTES, TruncationOptions, format_size};

/// Pi's `lsToolSystemPromptContribution` snippet; the definition has no
/// `promptGuidelines`.
pub const LS_PROMPT_SNIPPET: &str = "List directory contents";

const DEFAULT_LIMIT: f64 = 500.0;

/// JavaScript's `Number.MAX_SAFE_INTEGER`, Pi's "no line limit".
pub(crate) const MAX_SAFE_INTEGER: usize = 9_007_199_254_740_991;

fn parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "path": { "type": "string", "description": "Directory to list (default: current directory)" },
            "limit": { "type": "number", "description": "Maximum number of entries to return (default: 500)" },
        },
    })
}

/// Pi's `createLsToolDefinition`.
pub fn create_ls_tool_definition(cwd: &Path) -> ToolDefinition {
    owned_ls_tool_definition(cwd, None)
}

/// The native listing whose blocking worker belongs to the session's gate.
pub(crate) fn owned_ls_tool_definition(
    cwd: &Path,
    owner: Option<crate::owned_work::WorkGate>,
) -> ToolDefinition {
    let cwd = cwd.to_path_buf();
    let mut tool = AgentTool::new(
        "ls",
        "ls",
        format!(
            "List directory contents. Returns entries sorted alphabetically, with '/' suffix for directories. Includes dotfiles. Output is truncated to {} entries or {}KB (whichever is hit first).",
            number_to_string(DEFAULT_LIMIT),
            DEFAULT_MAX_BYTES / 1024
        ),
        parameters(),
        move |_id, args, signal, _update| boxed(execute(cwd.clone(), args, signal, owner.clone())),
    );
    tool.read_only = true;
    tool.replay = Some(ToolReplay::Safe);
    ToolDefinition {
        tool,
        prompt_snippet: Some(LS_PROMPT_SNIPPET.into()),
        prompt_guidelines: None,
    }
}

/// Pi's `createLsTool`.
pub fn create_ls_tool(cwd: &Path) -> AgentTool {
    create_ls_tool_definition(cwd).tool
}

async fn execute(
    cwd: PathBuf,
    args: Value,
    signal: Option<AbortSignal>,
    owner: Option<crate::owned_work::WorkGate>,
) -> Result<AgentToolResult, String> {
    let path = optional_string_arg(&args, "path").filter(|path| !path.is_empty());
    let limit = number_arg(&args, "limit").unwrap_or(DEFAULT_LIMIT);
    let work_signal = signal.clone();
    run_blocking_abortable(signal, owner, move || {
        let dir = resolve_to_cwd(path.as_deref().unwrap_or("."), &cwd)?;
        list(&dir, limit, work_signal.as_ref())
    })
    .await
}

fn list(dir: &Path, limit: f64, signal: Option<&AbortSignal>) -> Result<AgentToolResult, String> {
    if !path_exists(dir) {
        return Err(format!("Path not found: {}", dir.display()));
    }
    let metadata = std::fs::metadata(dir)
        .map_err(|error| crate::tools::node_error::message(&error, "stat", Some(dir)))?;
    if !metadata.is_dir() {
        return Err(format!("Not a directory: {}", dir.display()));
    }
    let entries = std::fs::read_dir(dir).map_err(|error| {
        format!(
            "Cannot read directory: {}",
            crate::tools::node_error::message(&error, "scandir", Some(dir))
        )
    })?;
    let mut names: Vec<String> = Vec::new();
    for entry in entries {
        if signal.is_some_and(AbortSignal::aborted) {
            return Err(ABORTED.into());
        }
        let entry = entry.map_err(|error| {
            format!(
                "Cannot read directory: {}",
                crate::tools::node_error::message(&error, "scandir", Some(dir))
            )
        })?;
        names.push(entry.file_name().to_string_lossy().into_owned());
    }
    let mut keyed: Vec<(CollationKey, String)> = names
        .into_iter()
        .map(|name| (CollationKey::new(&name.to_lowercase()), name))
        .collect();
    keyed.sort_by(|(a, _), (b, _)| a.cmp(b));

    let mut results: Vec<String> = Vec::new();
    let mut limit_reached = false;
    for (_, name) in keyed {
        if results.len() as f64 >= limit {
            limit_reached = true;
            break;
        }
        if signal.is_some_and(AbortSignal::aborted) {
            return Err(ABORTED.into());
        }
        // Entries that cannot be stat'ed, such as broken links, are skipped.
        let Ok(metadata) = std::fs::metadata(dir.join(&name)) else {
            continue;
        };
        results.push(if metadata.is_dir() {
            format!("{name}/")
        } else {
            name
        });
    }
    if results.is_empty() {
        return Ok(text_result("(empty directory)", None));
    }
    let mut collector = HeadCollector::new(TruncationOptions {
        max_lines: MAX_SAFE_INTEGER,
        max_bytes: DEFAULT_MAX_BYTES,
    });
    for line in &results {
        collector.push(line);
    }
    let truncation = collector.finish();
    let mut output = truncation.content.clone();
    let mut details = Map::new();
    let mut notices: Vec<String> = Vec::new();
    if limit_reached {
        notices.push(format!(
            "{} entries limit reached. Use limit={} for more",
            number_to_string(limit),
            number_to_string(limit * 2.0)
        ));
        details.insert("entryLimitReached".into(), number_value(limit));
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

/// ICU's root order of the lowercase printable ASCII characters.
const ASCII_ORDER: &str = " _-,;:!?.'\"()[]{}@*/\\&#%`^+<=>|~$0123456789abcdefghijklmnopqrstuvwxyz";

/// Secondary weights of common combining marks, in ICU's order.
const MARK_ORDER: [char; 10] = [
    '\u{0301}', '\u{0300}', '\u{0306}', '\u{0302}', '\u{030C}', '\u{030A}', '\u{0308}', '\u{030B}',
    '\u{0303}', '\u{0307}',
];

fn primary(c: char) -> Option<u32> {
    let code = c as u32;
    if (9..=13).contains(&code) {
        return Some(code - 9);
    }
    if code < 0x20 || code == 0x7f {
        return None;
    }
    if let Some(index) = ASCII_ORDER.find(c) {
        return Some(5 + index as u32);
    }
    let ascii = match c {
        '\u{00A0}' | '\u{2000}'..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}' => ' ',
        '\u{2018}' | '\u{2019}' => '\'',
        '\u{201C}' | '\u{201D}' => '"',
        '\u{2010}'..='\u{2015}' => '-',
        _ => return Some(0x1000 + code),
    };
    ASCII_ORDER.find(ascii).map(|index| 5 + index as u32)
}

/// An approximation of ICU's root collation key, primary then secondary.
#[derive(Debug, PartialEq, Eq)]
struct CollationKey {
    primary: Vec<u32>,
    secondary: Vec<u32>,
}

impl CollationKey {
    fn new(text: &str) -> Self {
        let decomposed = icu_normalizer::DecomposingNormalizerBorrowed::new_nfd().normalize(text);
        let mut key = Self {
            primary: Vec::new(),
            secondary: Vec::new(),
        };
        for c in decomposed.chars() {
            if ('\u{0300}'..='\u{036F}').contains(&c) {
                let weight = MARK_ORDER
                    .iter()
                    .position(|mark| *mark == c)
                    .map_or(0x100 + c as u32, |index| 1 + index as u32);
                if let Some(last) = key.secondary.last_mut() {
                    *last = (*last).max(weight);
                }
                continue;
            }
            if c == '\u{00DF}' {
                key.primary
                    .extend(primary('s').into_iter().chain(primary('s')));
                // ICU orders `ß` after `ss` at the tertiary level.
                key.secondary.extend([0, 0x200]);
                continue;
            }
            if let Some(weight) = primary(c) {
                key.primary.push(weight);
                key.secondary.push(0);
            }
        }
        key
    }
}

impl Ord for CollationKey {
    fn cmp(&self, other: &Self) -> Ordering {
        self.primary
            .cmp(&other.primary)
            .then_with(|| self.secondary.cmp(&other.secondary))
    }
}

impl PartialOrd for CollationKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Orders recorded from Node 24's `localeCompare`.
    #[test]
    fn collation_follows_icu_for_latin_names() {
        let mut names = vec![
            "ab", "a.b", "a1", "a-b", "a_b", "a b", "a", "9", "10", "~x", ".x", "_a", "z", "é",
            "è", "e", "f", "ß", "ss", "s", "α", "а", "一",
        ];
        names.sort_by_key(|name| CollationKey::new(name));
        assert_eq!(
            names,
            [
                "_a", ".x", "~x", "10", "9", "a", "a b", "a_b", "a-b", "a.b", "a1", "ab", "e", "é",
                "è", "f", "s", "ss", "ß", "z", "α", "а", "一"
            ]
        );
    }
}
