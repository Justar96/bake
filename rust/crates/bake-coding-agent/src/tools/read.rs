//! The `read` tool, ported from Pi `src/core/tools/read.ts` (v1.1.0; see
//! the crate's `NOTICE`).
//!
//! Text is read as UTF-8 with replacement characters, split on `\n`, and
//! returned from `offset` (1-based) for `limit` lines, then truncated to
//! 2000 lines or 50 KB with Pi's continuation notices. Files that sniff as
//! JPEG, PNG, GIF, WebP, or BMP return an image block, converted and
//! resized as [`crate::tools::image::process_image`] does. `offset` and
//! `limit` follow JavaScript's number semantics, fractions and negatives
//! included, since Pi passes them to `Array.prototype.slice`.
//!
//! # Deviations from Pi
//!
//! - **Streaming.** Pi reads the whole file into memory. Bake streams it,
//!   holding only the lines the result can show, so a file of any size
//!   costs bounded memory; the text and details are the same.
//! - **Image size.** An image file larger than [`MAX_IMAGE_FILE_BYTES`] is
//!   not loaded; the result says it could not be resized, as Pi's does for
//!   an image Photon cannot process.
//! - **No pluggable operations.** Pi's `ReadOperations` (for remote
//!   files) are not ported.

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use bake_agent::{AgentTool, AgentToolResult};
use bake_ai::{
    AbortSignal, ConstrainedSampling, ImageContent, InputModality, Model, StrictPreference,
    TextContent, UserContentBlock,
};
use serde_json::{Value, json};

use crate::tools::definition::{
    ABORTED, ToolDefinition, aborted, boxed, number_arg, run_blocking_abortable, string_arg,
};
use crate::tools::head::HeadCollector;
use crate::tools::image::{self, ImageResizeOptions, ProcessedImage};
use crate::tools::js::{as_index, number_to_string, slice_index};
use crate::tools::node_error;
use crate::tools::path_utils::resolve_read_path;
use crate::tools::truncate::{
    DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, TruncatedBy, TruncationOptions, TruncationResult,
    format_size,
};

/// Pi's `readToolSystemPromptContribution` snippet.
pub const READ_PROMPT_SNIPPET: &str = "Read file contents";
/// Pi's `readToolSystemPromptContribution` guidelines.
pub const READ_PROMPT_GUIDELINES: &[&str] = &["Use read to examine files instead of cat or sed."];

/// The largest image file read.
pub const MAX_IMAGE_FILE_BYTES: u64 = 64 * 1024 * 1024;

const READ_CHUNK_BYTES: usize = 64 * 1024;

/// The model the session runs, which Pi's tools find in their context.
pub type ModelSource = Arc<dyn Fn() -> Option<Model> + Send + Sync>;

/// Pi's `ReadToolOptions` without custom operations.
#[derive(Clone)]
pub struct ReadToolOptions {
    /// Resize images to the inline limits; default true.
    pub auto_resize_images: bool,
    /// Resize limits when the model has none.
    pub resize_options: Option<ImageResizeOptions>,
    /// The session's model: one without image input gets Pi's note, and
    /// its `inputLimits.images.resize` wins over [`Self::resize_options`].
    pub model: Option<ModelSource>,
}

impl Default for ReadToolOptions {
    fn default() -> Self {
        Self {
            auto_resize_images: true,
            resize_options: None,
            model: None,
        }
    }
}

impl std::fmt::Debug for ReadToolOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReadToolOptions")
            .field("auto_resize_images", &self.auto_resize_images)
            .field("resize_options", &self.resize_options)
            .field("model", &self.model.is_some())
            .finish()
    }
}

fn description() -> String {
    format!(
        "Read the contents of a file. Supports text files and images (jpg, png, gif, webp, bmp). Images are sent as attachments. For text files, output is truncated to {DEFAULT_MAX_LINES} lines or {}KB (whichever is hit first). Use offset/limit for large files. When you need the full file, continue with offset until complete.",
        DEFAULT_MAX_BYTES / 1024
    )
}

fn parameters() -> Value {
    json!({
        "type": "object",
        "required": ["path"],
        "properties": {
            "path": { "type": "string", "description": "Path to the file to read (relative or absolute)" },
            "offset": { "type": "number", "description": "Line number to start reading from (1-indexed)" },
            "limit": { "type": "number", "description": "Maximum number of lines to read" },
        },
    })
}

fn output_schema() -> Value {
    json!({
        "anyOf": [
            { "type": "string" },
            {
                "type": "object",
                "required": ["type", "data", "mimeType", "note"],
                "properties": {
                    "type": { "type": "string", "const": "image" },
                    "data": { "type": "string" },
                    "mimeType": { "type": "string" },
                    "note": { "type": "string" },
                },
            },
        ],
    })
}

/// Pi's `createReadToolDefinition`.
pub fn create_read_tool_definition(cwd: &Path, options: ReadToolOptions) -> ToolDefinition {
    owned_read_tool_definition(cwd, options, None)
}

/// The native read whose blocking worker belongs to the session's gate.
pub(crate) fn owned_read_tool_definition(
    cwd: &Path,
    options: ReadToolOptions,
    owner: Option<crate::owned_work::WorkGate>,
) -> ToolDefinition {
    let cwd = cwd.to_path_buf();
    let options = Arc::new(options);
    let mut tool = AgentTool::new(
        "read",
        "read",
        description(),
        parameters(),
        move |_id, args, signal, _update| {
            let cwd = cwd.clone();
            let options = Arc::clone(&options);
            boxed(execute(cwd, options, args, signal, owner.clone()))
        },
    );
    tool.output_schema = Some(output_schema());
    tool.read_only = true;
    tool.constrained_sampling = Some(ConstrainedSampling::JsonSchema {
        strict: StrictPreference::Prefer,
    });
    ToolDefinition {
        tool,
        prompt_snippet: Some(READ_PROMPT_SNIPPET.into()),
        prompt_guidelines: Some(
            READ_PROMPT_GUIDELINES
                .iter()
                .map(|g| (*g).to_owned())
                .collect(),
        ),
    }
}

/// Pi's `createReadTool`.
pub fn create_read_tool(cwd: &Path, options: ReadToolOptions) -> AgentTool {
    create_read_tool_definition(cwd, options).tool
}

const NON_VISION_NOTE: &str =
    "[Current model does not support images. The image will be omitted from this request.]";

async fn execute(
    cwd: PathBuf,
    options: Arc<ReadToolOptions>,
    args: Value,
    signal: Option<AbortSignal>,
    owner: Option<crate::owned_work::WorkGate>,
) -> Result<AgentToolResult, String> {
    if aborted(signal.as_ref()) {
        return Err(ABORTED.into());
    }
    let path = string_arg(&args, "path");
    let offset = number_arg(&args, "offset");
    let limit = number_arg(&args, "limit");
    let model = options.model.as_ref().and_then(|model| model());
    let non_vision = model
        .as_ref()
        .is_some_and(|model| !model.input.contains(&InputModality::Image));
    let resize = model
        .as_ref()
        .and_then(|model| ImageResizeOptions::from_input_limits(model.input_limits.as_ref()))
        .or(options.resize_options)
        .unwrap_or_default();
    let auto_resize = options.auto_resize_images;
    let work_signal = signal.clone();
    let (content, details) = run_blocking_abortable(signal, owner, move || {
        let request = ReadRequest {
            cwd: &cwd,
            path: &path,
            offset,
            limit,
            auto_resize,
            resize,
            non_vision,
            signal: work_signal.as_ref(),
        };
        read(&request)
    })
    .await?;
    let structured = structured_content(&content);
    Ok(AgentToolResult {
        content,
        details,
        structured_content: Some(structured),
        ..AgentToolResult::default()
    })
}

/// Pi's `toReadOutput`.
fn structured_content(content: &[UserContentBlock]) -> Value {
    let text = content
        .iter()
        .find_map(|block| match block {
            UserContentBlock::Text(text) => Some(text.text.clone()),
            UserContentBlock::Image(_) => None,
        })
        .unwrap_or_default();
    match content.iter().find_map(|block| match block {
        UserContentBlock::Image(image) => Some(image),
        UserContentBlock::Text(_) => None,
    }) {
        Some(image) => {
            json!({ "type": "image", "data": image.data, "mimeType": image.mime_type, "note": text })
        }
        None => Value::String(text),
    }
}

struct ReadRequest<'a> {
    cwd: &'a Path,
    path: &'a str,
    offset: Option<f64>,
    limit: Option<f64>,
    auto_resize: bool,
    resize: ImageResizeOptions,
    non_vision: bool,
    signal: Option<&'a AbortSignal>,
}

type ReadOutput = (Vec<UserContentBlock>, Option<Value>);

fn text_block(text: String) -> UserContentBlock {
    UserContentBlock::Text(TextContent::new(text))
}

fn read(request: &ReadRequest<'_>) -> Result<ReadOutput, String> {
    let absolute = resolve_read_path(request.path, request.cwd)?;
    node_error::access(&absolute, false)
        .map_err(|error| node_error::message(&error, "access", Some(&absolute)))?;
    let mime = sniff(&absolute)?;
    match mime {
        Some(mime) => read_image(request, &absolute, mime),
        None => read_text(request, &absolute),
    }
}

fn sniff(path: &Path) -> Result<Option<&'static str>, String> {
    let mut file = std::fs::File::open(path)
        .map_err(|error| node_error::message(&error, "open", Some(path)))?;
    let mut buffer = Vec::with_capacity(image::mime::IMAGE_TYPE_SNIFF_BYTES);
    (&mut file)
        .take(image::mime::IMAGE_TYPE_SNIFF_BYTES as u64)
        .read_to_end(&mut buffer)
        .map_err(|error| node_error::message(&error, "read", None))?;
    Ok(image::mime::detect_supported_image_mime_type(&buffer))
}

fn read_image(request: &ReadRequest<'_>, path: &Path, mime: &str) -> Result<ReadOutput, String> {
    let file = std::fs::File::open(path)
        .map_err(|error| node_error::message(&error, "open", Some(path)))?;
    let mut bytes = Vec::new();
    file.take(MAX_IMAGE_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| node_error::message(&error, "read", None))?;
    if request.signal.is_some_and(AbortSignal::aborted) {
        return Err(ABORTED.into());
    }
    let processed = if bytes.len() as u64 > MAX_IMAGE_FILE_BYTES {
        ProcessedImage::Failed(
            "[Image omitted: could not be resized below the inline image size limit.]".into(),
        )
    } else {
        image::process_image(&bytes, mime, request.auto_resize, request.resize)
    };
    Ok(match processed {
        ProcessedImage::Failed(message) => {
            let mut note = format!("Read image file [{mime}]\n{message}");
            if request.non_vision {
                note.push('\n');
                note.push_str(NON_VISION_NOTE);
            }
            (vec![text_block(note)], None)
        }
        ProcessedImage::Ok {
            data,
            mime_type,
            hints,
        } => {
            let mut note = format!("Read image file [{mime_type}]");
            if !hints.is_empty() {
                note.push('\n');
                note.push_str(&hints.join("\n"));
            }
            if request.non_vision {
                note.push('\n');
                note.push_str(NON_VISION_NOTE);
            }
            (
                vec![
                    text_block(note),
                    UserContentBlock::Image(ImageContent { data, mime_type }),
                ],
                None,
            )
        }
    })
}

/// The decoded UTF-8 length of a byte stream, with Node's replacement of
/// invalid sequences, fed in pieces.
#[derive(Default)]
struct LossyLength {
    carry: Vec<u8>,
    len: usize,
}

impl LossyLength {
    fn feed(&mut self, bytes: &[u8]) {
        let joined;
        let mut input = if self.carry.is_empty() {
            bytes
        } else {
            let mut buffer = std::mem::take(&mut self.carry);
            buffer.extend_from_slice(bytes);
            joined = buffer;
            &joined[..]
        };
        loop {
            match std::str::from_utf8(input) {
                Ok(valid) => {
                    self.len += valid.len();
                    return;
                }
                Err(error) => {
                    self.len += error.valid_up_to();
                    match error.error_len() {
                        Some(skip) => {
                            self.len += '\u{FFFD}'.len_utf8();
                            input = &input[error.valid_up_to() + skip..];
                        }
                        None => {
                            self.carry = input[error.valid_up_to()..].to_vec();
                            return;
                        }
                    }
                }
            }
        }
    }

    fn finish(&mut self) -> usize {
        if !self.carry.is_empty() {
            self.carry.clear();
            self.len += '\u{FFFD}'.len_utf8();
        }
        std::mem::take(&mut self.len)
    }
}

/// What a pass over a text file found.
struct TextScan {
    /// Pi's `allLines.length`.
    total_lines: usize,
    /// `truncateHead` of the selected lines joined with `\n`.
    truncation: TruncationResult,
    /// The decoded length of the first selected line.
    first_selected_len: Option<usize>,
}

/// Counts the lines of `path` as `split("\n")` does.
fn count_lines(path: &Path, signal: Option<&AbortSignal>) -> Result<usize, String> {
    let file = std::fs::File::open(path)
        .map_err(|error| node_error::message(&error, "open", Some(path)))?;
    let mut reader = BufReader::with_capacity(READ_CHUNK_BYTES, file);
    let mut lines = 1usize;
    loop {
        if signal.is_some_and(AbortSignal::aborted) {
            return Err(ABORTED.into());
        }
        let buffer = reader
            .fill_buf()
            .map_err(|error| node_error::message(&error, "read", None))?;
        if buffer.is_empty() {
            return Ok(lines);
        }
        lines += buffer.iter().filter(|byte| **byte == b'\n').count();
        let consumed = buffer.len();
        reader.consume(consumed);
    }
}

/// One pass over `path`: lines `start..end` go through [`HeadCollector`],
/// the rest are only counted.
fn scan_text(
    path: &Path,
    start: usize,
    end: Option<usize>,
    signal: Option<&AbortSignal>,
) -> Result<TextScan, String> {
    let file = std::fs::File::open(path)
        .map_err(|error| node_error::message(&error, "open", Some(path)))?;
    let mut reader = BufReader::with_capacity(READ_CHUNK_BYTES, file);
    let mut collector = HeadCollector::new(TruncationOptions::default());
    let cap = collector.max_line_bytes().saturating_add(1);
    let selected = |line: usize| line >= start && end.is_none_or(|end| line < end);

    let mut line = 0usize;
    let mut length = LossyLength::default();
    let mut raw: Vec<u8> = Vec::new();
    let mut keep = selected(0) && collector.wants_text();
    let mut overflow = false;
    let mut first_selected_len = None;

    let mut end_line = |line: usize,
                        length: &mut LossyLength,
                        raw: &mut Vec<u8>,
                        keep: bool,
                        overflow: bool,
                        collector: &mut HeadCollector| {
        if !selected(line) {
            return;
        }
        let len = length.finish();
        if line == start {
            first_selected_len = Some(len);
        }
        if keep && !overflow {
            collector.push(&String::from_utf8_lossy(raw));
        } else {
            collector.push_len(len);
        }
        raw.clear();
    };

    loop {
        if signal.is_some_and(AbortSignal::aborted) {
            return Err(ABORTED.into());
        }
        let buffer = reader
            .fill_buf()
            .map_err(|error| node_error::message(&error, "read", None))?;
        if buffer.is_empty() {
            end_line(line, &mut length, &mut raw, keep, overflow, &mut collector);
            break;
        }
        let consumed = buffer.len();
        let mut rest = buffer;
        while !rest.is_empty() {
            if end.is_some_and(|end| line >= end) {
                // Past the selection: only count.
                line += rest.iter().filter(|byte| **byte == b'\n').count();
                break;
            }
            let newline = rest.iter().position(|byte| *byte == b'\n');
            let piece = &rest[..newline.unwrap_or(rest.len())];
            if selected(line) {
                length.feed(piece);
                if keep && !overflow {
                    if raw.len() + piece.len() > cap {
                        overflow = true;
                        raw = Vec::new();
                    } else {
                        raw.extend_from_slice(piece);
                    }
                }
            }
            match newline {
                Some(index) => {
                    end_line(line, &mut length, &mut raw, keep, overflow, &mut collector);
                    line += 1;
                    keep = selected(line) && collector.wants_text();
                    overflow = false;
                    rest = &rest[index + 1..];
                }
                None => rest = &[],
            }
        }
        reader.consume(consumed);
    }
    Ok(TextScan {
        total_lines: line + 1,
        truncation: collector.finish(),
        first_selected_len,
    })
}

fn read_text(request: &ReadRequest<'_>, path: &Path) -> Result<ReadOutput, String> {
    // Pi: `offset ? Math.max(0, offset - 1) : 0`.
    let start_line = match request.offset {
        Some(offset) if offset != 0.0 && !offset.is_nan() => (offset - 1.0).max(0.0),
        _ => 0.0,
    };
    let start_display = start_line + 1.0;
    let start = slice_index(start_line, usize::MAX);
    let scan = match request.limit {
        Some(limit) if start_line + limit < 0.0 => {
            // A negative end counts from the end of the file.
            let total = count_lines(path, request.signal)?;
            let end = slice_index(start_line + limit, total);
            scan_text(path, start, Some(end), request.signal)?
        }
        Some(limit) => {
            let end = slice_index(start_line + limit, usize::MAX);
            scan_text(path, start, Some(end), request.signal)?
        }
        None => scan_text(path, start, None, request.signal)?,
    };
    let total = scan.total_lines as f64;
    if start_line >= total {
        return Err(format!(
            "Offset {} is beyond end of file ({} lines total)",
            number_to_string(request.offset.unwrap_or(0.0)),
            scan.total_lines
        ));
    }
    let user_limited_lines = request
        .limit
        .map(|limit| (start_line + limit).min(total) - start_line);
    let truncation = scan.truncation;
    let max = format_size(DEFAULT_MAX_BYTES);
    let display = number_to_string(start_display);
    let (text, details) = if truncation.first_line_exceeds_limit {
        let Some(first_len) = scan
            .first_selected_len
            .filter(|_| as_index(start_line).is_some())
        else {
            // Pi reads `allLines[startLine]` with a fractional index.
            return Err("The \"string\" argument must be of type string or an instance of Buffer or ArrayBuffer. Received undefined".into());
        };
        let text = format!(
            "[Line {display} is {}, exceeds {max} limit. Use bash: sed -n '{display}p' {} | head -c {DEFAULT_MAX_BYTES}]",
            format_size(first_len),
            request.path
        );
        (text, Some(truncation))
    } else if truncation.truncated {
        let end_display = start_display + truncation.output_lines as f64 - 1.0;
        let next = number_to_string(end_display + 1.0);
        let end_display = number_to_string(end_display);
        let mut text = truncation.content.clone();
        if truncation.truncated_by == Some(TruncatedBy::Lines) {
            text.push_str(&format!(
                "\n\n[Showing lines {display}-{end_display} of {}. Use offset={next} to continue.]",
                scan.total_lines
            ));
        } else {
            text.push_str(&format!(
                "\n\n[Showing lines {display}-{end_display} of {} ({max} limit). Use offset={next} to continue.]",
                scan.total_lines
            ));
        }
        (text, Some(truncation))
    } else if let Some(limited) = user_limited_lines.filter(|limited| start_line + limited < total)
    {
        let remaining = number_to_string(total - (start_line + limited));
        let next = number_to_string(start_line + limited + 1.0);
        (
            format!(
                "{}\n\n[{remaining} more lines in file. Use offset={next} to continue.]",
                truncation.content
            ),
            None,
        )
    } else {
        (truncation.content, None)
    };
    let details = details.map(|truncation| json!({ "truncation": truncation }));
    Ok((vec![text_block(text)], details))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lossy_length_matches_from_utf8_lossy_across_pieces() {
        let samples: [&[u8]; 6] = [
            b"plain",
            &[0x61, 0xff, 0x62],
            &[0xe2, 0x82],
            &[0xf0, 0x9f, 0x98, 0x80, 0xc3],
            &[0xed, 0xa0, 0x80, 0x41],
            &[0xe2, 0x82, 0xac, 0xe2, 0x28, 0xa1],
        ];
        for sample in samples {
            let expected = String::from_utf8_lossy(sample).len();
            for split in 0..=sample.len() {
                let mut length = LossyLength::default();
                length.feed(&sample[..split]);
                length.feed(&sample[split..]);
                assert_eq!(length.finish(), expected, "{sample:?} at {split}");
            }
        }
    }
}
