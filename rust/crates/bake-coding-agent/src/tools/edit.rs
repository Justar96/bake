//! The `edit` tool, ported from Pi `src/core/tools/edit.ts` (v1.1.0; see
//! the crate's `NOTICE`).
//!
//! The file is read as UTF-8 with replacement characters, its BOM set
//! aside and its line endings normalized to `\n`; every edit is matched
//! against that original as [`apply_edits_to_normalized_content`] does, and
//! the result is written back with the BOM and the file's first line ending.
//! The work runs inside the file's mutation queue on Tokio's blocking pool;
//! an abort is observed between steps. Under the session's owner that work
//! holds a permit, so shutdown waits for an edit even when its run was
//! dropped ([`crate::tools::index::ToolsOptions::owner`]). The result's
//! details hold Pi's numbered diff, unified patch, and first changed line.
//!
//! `prepare_arguments` accepts Pi's legacy inputs: top-level `oldText` and
//! `newText`, `edits` as a JSON string, and a single edit object.
//!
//! # Deviations from Pi
//!
//! - A file larger than [`MAX_EDIT_FILE_BYTES`] is refused before it is
//!   read; Pi reads files of any size.
//! - Pi's pluggable `EditOperations` are not ported; [`execute_guarded`]
//!   is Bake's seam for the sandbox's containment check.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use bake_agent::{AgentTool, AgentToolResult, ToolReplay};
use bake_ai::{AbortSignal, ConstrainedSampling, StrictPreference};
use serde_json::{Map, Value, json};

use crate::tools::definition::{
    ABORTED, MutationGuard, ToolDefinition, boxed, mutation_target, run_blocking, string_arg,
    target_error, text_result,
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

/// Pi's `createEditToolDefinition`. Its blocking work has no owner: a
/// dropped call's edit runs to its end unawaited.
pub fn create_edit_tool_definition(cwd: &Path) -> ToolDefinition {
    owned_edit_tool_definition(cwd, None)
}

/// [`create_edit_tool_definition`] whose blocking work `owner` owns: its
/// close waits for a call's edit, even one whose run was dropped, and a
/// call made once it is closing is refused.
pub(crate) fn owned_edit_tool_definition(
    cwd: &Path,
    owner: Option<crate::owned_work::WorkGate>,
) -> ToolDefinition {
    let cwd = cwd.to_path_buf();
    let mut tool = AgentTool::new(
        "edit",
        "edit",
        "Edit a single file using exact text replacement. Every edits[].oldText must match a unique, non-overlapping region of the original file. If two changes affect the same block or nearby lines, merge them into one edit instead of emitting overlapping edits. Do not include large unchanged regions just to connect distant changes.",
        parameters(),
        move |_id, args, signal, _update| {
            boxed(execute_owned(
                cwd.clone(),
                args,
                signal,
                None,
                owner.clone(),
            ))
        },
    );
    tool.replay = Some(ToolReplay::Never);
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

/// Pi's `validateEditInput`, as its error text: the check `execute` makes
/// before it touches the file.
pub fn validate_edit_input(args: &Value) -> Result<(), String> {
    edits_of(args).map(|_| ())
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

/// Runs an `edit` call, first passing the target to `guard` inside its
/// mutation queue, after the first abort check and before the file is
/// checked or read; the file is then read and written through the target
/// the guard returns. The blocking work has no owner.
pub async fn execute_guarded(
    cwd: PathBuf,
    args: Value,
    signal: Option<AbortSignal>,
    guard: Option<MutationGuard>,
) -> Result<AgentToolResult, String> {
    execute_owned(cwd, args, signal, guard, None).await
}

/// [`execute_guarded`] with the blocking work under `owner`.
pub(crate) async fn execute_owned(
    cwd: PathBuf,
    args: Value,
    signal: Option<AbortSignal>,
    guard: Option<MutationGuard>,
    owner: Option<crate::owned_work::WorkGate>,
) -> Result<AgentToolResult, String> {
    let edits = edits_of(&args)?;
    let path = string_arg(&args, "path");
    let absolute = resolve_to_cwd(&path, &cwd)?;
    run_blocking(owner, move || {
        let check = || {
            if signal.as_ref().is_some_and(AbortSignal::aborted) {
                Err(ABORTED.to_owned())
            } else {
                Ok(())
            }
        };
        let outcome = with_file_mutation_queue(&absolute, || {
            Ok(edit_file(&absolute, &path, &edits, &check, guard.as_ref()))
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
    guard: Option<&MutationGuard>,
) -> Result<Value, String> {
    check()?;
    // The guard's target carries the read and the write, so the call edits
    // what the guard checked.
    let target = mutation_target(guard, absolute)?;
    let target = target.as_ref();
    if let Err(error) = node_error::access(absolute, true) {
        check()?;
        return Err(format!(
            "Could not edit file: {path}. Error code: {}.",
            node_error::code(&error)
        ));
    }
    check()?;
    let file = target
        .open_read()
        .map_err(|error| target_error(target, &error, "open", absolute))?;
    let mut bytes = Vec::new();
    file.take(MAX_EDIT_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| target_error(target, &error, "read", absolute))?;
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
    target
        .create()
        .and_then(|mut file| file.write_all(final_content.as_bytes()))
        .map_err(|error| target_error(target, &error, "open", absolute))?;
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
