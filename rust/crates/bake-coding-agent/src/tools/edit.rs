//! The `edit` tool, ported from Pi `src/core/tools/edit.ts` (v1.1.0; see
//! the crate's `NOTICE`).
//!
//! The file is read as UTF-8 with replacement characters, its BOM set
//! aside and its line endings normalized to `\n`; every edit is matched
//! against that original as [`apply_edits_to_normalized_content`] does, and
//! the result is written back with the BOM and the file's first line ending.
//! The work runs inside the file's mutation queue on Tokio's blocking pool;
//! an abort is observed between steps. The result's details hold Pi's
//! numbered diff, unified patch, and first changed line.
//!
//! `prepare_arguments` accepts Pi's legacy inputs: top-level `oldText` and
//! `newText`, `edits` as a JSON string, and a single edit object.
//!
//! # Deviations from Pi
//!
//! - A file larger than [`MAX_EDIT_FILE_BYTES`] is refused before it is
//!   read; Pi reads files of any size.
//! - Pi's pluggable `EditOperations` are not ported.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use bake_agent::{AgentTool, AgentToolResult};
use bake_ai::{AbortSignal, ConstrainedSampling, StrictPreference};
use serde_json::{Map, Value, json};

use crate::tools::definition::{
    ABORTED, ToolDefinition, boxed, run_blocking, string_arg, text_result,
};
use crate::tools::edit_diff::{
    Edit, apply_edits_to_normalized_content, detect_line_ending, generate_diff_string,
    generate_unified_patch, normalize_to_lf, restore_line_endings,
};
use crate::tools::file_mutation_queue::with_file_mutation_queue;
use crate::tools::node_error;
use crate::tools::path_utils::resolve_to_cwd;

/// The largest file the tool edits.
pub const MAX_EDIT_FILE_BYTES: u64 = 64 * 1024 * 1024;

/// Pi's `editToolSystemPromptContribution` snippet.
pub const EDIT_PROMPT_SNIPPET: &str = "Make precise file edits with exact text replacement, including multiple disjoint edits in one call";
/// Pi's `editToolSystemPromptContribution` guidelines.
pub const EDIT_PROMPT_GUIDELINES: &[&str] = &[
    "Use edit for precise changes (edits[].oldText must match exactly)",
    "When changing multiple separate locations in one file, use one edit call with multiple entries in edits[] instead of multiple edit calls",
    "Each edits[].oldText is matched against the original file, not after earlier edits are applied. Do not emit overlapping or nested edits. Merge nearby changes into one edit.",
    "Keep edits[].oldText as small as possible while still being unique in the file. Do not pad with large unchanged regions.",
];

fn parameters() -> Value {
    json!({
        "type": "object",
        "required": ["path", "edits"],
        "properties": {
            "path": { "type": "string", "description": "Path to the file to edit (relative or absolute)" },
            "edits": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["oldText", "newText"],
                    "properties": {
                        "oldText": {
                            "type": "string",
                            "description": "Exact text for one targeted replacement. It must be unique in the original file and must not overlap with any other edits[].oldText in the same call.",
                        },
                        "newText": { "type": "string", "description": "Replacement text for this targeted edit." },
                    },
                },
                "description": "One or more targeted replacements. Each edit is matched against the original file, not incrementally. Do not include overlapping or nested edits. If two changes touch the same block or nearby lines, merge them into one edit instead.",
            },
        },
    })
}

fn is_single_edit(value: &Value) -> bool {
    value.as_object().is_some_and(|edit| {
        edit.get("oldText").is_some_and(Value::is_string)
            && edit.get("newText").is_some_and(Value::is_string)
    })
}

/// Pi's `prepareEditArguments`: folds legacy inputs into `edits`. Anything
/// that is not an object passes through unchanged.
pub fn prepare_edit_arguments(input: Value) -> Value {
    let Value::Object(mut args) = input else {
        return input;
    };
    match args.get("edits") {
        Some(Value::String(text)) => match serde_json::from_str::<Value>(text) {
            Ok(parsed @ Value::Array(_)) => {
                args.insert("edits".into(), parsed);
            }
            Ok(parsed) if is_single_edit(&parsed) => {
                args.insert("edits".into(), Value::Array(vec![parsed]));
            }
            _ => {}
        },
        Some(edit) if is_single_edit(edit) => {
            let edit = edit.clone();
            args.insert("edits".into(), Value::Array(vec![edit]));
        }
        _ => {}
    }
    let (Some(Value::String(old_text)), Some(Value::String(new_text))) =
        (args.get("oldText"), args.get("newText"))
    else {
        return Value::Object(args);
    };
    let legacy = json!({ "oldText": old_text, "newText": new_text });
    let mut edits = match args.get("edits") {
        Some(Value::Array(edits)) => edits.clone(),
        _ => Vec::new(),
    };
    edits.push(legacy);
    args.shift_remove("oldText");
    args.shift_remove("newText");
    args.insert("edits".into(), Value::Array(edits));
    Value::Object(args)
}

/// Pi's `createEditToolDefinition`.
pub fn create_edit_tool_definition(cwd: &Path) -> ToolDefinition {
    let cwd = cwd.to_path_buf();
    let mut tool = AgentTool::new(
        "edit",
        "edit",
        "Edit a single file using exact text replacement. Every edits[].oldText must match a unique, non-overlapping region of the original file. If two changes affect the same block or nearby lines, merge them into one edit instead of emitting overlapping edits. Do not include large unchanged regions just to connect distant changes.",
        parameters(),
        move |_id, args, signal, _update| boxed(execute(cwd.clone(), args, signal)),
    );
    tool.constrained_sampling = Some(ConstrainedSampling::JsonSchema {
        strict: StrictPreference::Prefer,
    });
    tool.prepare_arguments = Some(Arc::new(|input| Ok(prepare_edit_arguments(input))));
    ToolDefinition {
        tool,
        prompt_snippet: Some(EDIT_PROMPT_SNIPPET.into()),
        prompt_guidelines: Some(
            EDIT_PROMPT_GUIDELINES
                .iter()
                .map(|g| (*g).to_owned())
                .collect(),
        ),
    }
}

/// Pi's `createEditTool`.
pub fn create_edit_tool(cwd: &Path) -> AgentTool {
    create_edit_tool_definition(cwd).tool
}

/// Pi's `validateEditInput`.
fn edits_of(args: &Value) -> Result<Vec<Edit>, String> {
    let invalid =
        || "Edit tool input is invalid. edits must contain at least one replacement.".to_owned();
    let edits = args
        .get("edits")
        .and_then(Value::as_array)
        .ok_or_else(invalid)?;
    if edits.is_empty() {
        return Err(invalid());
    }
    Ok(edits
        .iter()
        .map(|edit| Edit {
            old_text: string_arg(edit, "oldText"),
            new_text: string_arg(edit, "newText"),
        })
        .collect())
}

async fn execute(
    cwd: PathBuf,
    args: Value,
    signal: Option<AbortSignal>,
) -> Result<AgentToolResult, String> {
    let edits = edits_of(&args)?;
    let path = string_arg(&args, "path");
    let absolute = resolve_to_cwd(&path, &cwd)?;
    run_blocking(move || {
        let check = || {
            if signal.as_ref().is_some_and(AbortSignal::aborted) {
                Err(ABORTED.to_owned())
            } else {
                Ok(())
            }
        };
        let outcome = with_file_mutation_queue(&absolute, || {
            Ok(edit_file(&absolute, &path, &edits, &check))
        })
        .map_err(|error| node_error::message(&error, "realpath", Some(&absolute)))?;
        let details = outcome?;
        Ok(text_result(
            format!("Successfully replaced {} block(s) in {path}.", edits.len()),
            Some(details),
        ))
    })
    .await
}

fn edit_file(
    absolute: &Path,
    path: &str,
    edits: &[Edit],
    check: &dyn Fn() -> Result<(), String>,
) -> Result<Value, String> {
    check()?;
    if let Err(error) = node_error::access(absolute, true) {
        check()?;
        return Err(format!(
            "Could not edit file: {path}. Error code: {}.",
            node_error::code(&error)
        ));
    }
    check()?;
    let file = std::fs::File::open(absolute)
        .map_err(|error| node_error::message(&error, "open", Some(absolute)))?;
    let mut bytes = Vec::new();
    file.take(MAX_EDIT_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| node_error::message(&error, "read", Some(absolute)))?;
    if bytes.len() as u64 > MAX_EDIT_FILE_BYTES {
        return Err(format!(
            "Could not edit file: {path}. It is larger than {} MB.",
            MAX_EDIT_FILE_BYTES / (1024 * 1024)
        ));
    }
    let raw = String::from_utf8_lossy(&bytes);
    check()?;
    let (bom, content) = match raw.strip_prefix('\u{FEFF}') {
        Some(rest) => ("\u{FEFF}", rest),
        None => ("", &raw[..]),
    };
    let ending = detect_line_ending(content);
    let normalized = normalize_to_lf(content);
    let applied = apply_edits_to_normalized_content(&normalized, edits, path)?;
    check()?;
    let final_content = format!(
        "{bom}{}",
        restore_line_endings(&applied.new_content, ending)
    );
    std::fs::write(absolute, final_content)
        .map_err(|error| node_error::message(&error, "open", Some(absolute)))?;
    check()?;
    let diff = generate_diff_string(&applied.base_content, &applied.new_content);
    let patch = generate_unified_patch(path, &applied.base_content, &applied.new_content);
    let mut details = Map::new();
    details.insert("diff".into(), Value::String(diff.diff));
    details.insert("patch".into(), Value::String(patch));
    if let Some(line) = diff.first_changed_line {
        details.insert("firstChangedLine".into(), json!(line));
    }
    Ok(Value::Object(details))
}
