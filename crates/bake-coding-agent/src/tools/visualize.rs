//! The `visualize` tool: Bake's own, with no Pi source. The model calls it
//! to show the user a chart (bars, stacked bars, lines, areas, points,
//! shares, or a heatmap), a table, a Mermaid diagram, or an SVG image in
//! the terminal.
//!
//! The tool checks the arguments and answers with one line: what was shown
//! and how much of it, then any warnings, such as values shown as gaps or
//! an SVG image's text that passes the canvas's edge. Invalid arguments
//! fail with an error that says what to fix. The terminal draws the visual
//! from the call's arguments, since a drawing has to be laid out at each
//! width, so no drawing is stored in the result's `details`.
//!
//! With `preview` set, the model gets the drawing back to check it: an SVG
//! image as a PNG, when the session's model reads images, and a diagram as
//! the text the terminal draws. Charts and tables are drawn from their rows
//! as given, so they have no preview.
//!
//! The tool is registered with the other built-in tools; the CLI makes it a
//! default of a session someone sees while the `visualize` setting is on
//! (it is unless set to `false`), by the owner's decision of 2026-11-14,
//! though no paired live-model evaluation has measured it. Scripts
//! cannot call it ([`AgentTool::nested_callable`] is cleared): a script's
//! calls never reach the transcript, so a visual drawn there would never be
//! seen. Code mode's `only` mode therefore keeps it declared.
//!
//! The arguments are checked as the terminal will draw them: a label's
//! width is counted in terminal cells, after line breaks and tabs become
//! spaces and other control characters are dropped, since the model has to
//! see how much of a label the user sees.
//!
//! A diagram's source is read by [`bake_mermaid`], which the terminal
//! draws it with, so the tool accepts exactly what will be drawn. Its
//! outline (family, nodes, links, and size) is the result; one the
//! renderer does not read fails with an error ([`diagnose`]) that
//! names the line and what to change; one wider than [`WIDE_DIAGRAM`]
//! columns is shown with a warning, since a terminal too narrow for it
//! shows its source instead.
//!
//! An SVG image's source is read by [`bake_svg`], which the terminal draws
//! it with, under the same limits: no linked resources, entities, or
//! nesting that multiplies the tree. Its outline (size and elements) is
//! the result. Its warnings ([`svg`]) name what the model cannot see for
//! itself: text and shapes outside the canvas, labels that overlap, text
//! too small to read, a transparent background, and elements that do not
//! draw. Text is measured with the system's fonts, read once, on the
//! blocking pool.

mod svg;

use bake_agent::{AgentTool, AgentToolResult, ToolReplay};
use bake_ai::{AbortSignal, InputModality, TextContent, UserContentBlock};
use bake_mermaid::{Family, Outline, RenderError};
use serde_json::{Map, Value, json};
use unicode_width::UnicodeWidthStr;

use crate::tools::definition::{ToolDefinition, boxed, run_blocking_abortable};
use crate::tools::image::ImageResizeOptions;
use crate::tools::read::ModelSource;

/// The tool's name.
pub const VISUALIZE_TOOL_NAME: &str = "visualize";
/// Most rows a visual takes.
pub const MAX_ROWS: usize = 500;
/// Most columns a visual takes: a label column and up to seven series.
pub const MAX_COLUMNS: usize = 8;
/// Most columns a heatmap takes: a label column and up to 24 of values.
pub const MAX_HEATMAP_COLUMNS: usize = 25;
/// Most slices a pie chart draws; the smallest of more are gathered into
/// one.
pub const MAX_SLICES: usize = 7;
/// Columns past which a diagram has no preview.
pub const MAX_PREVIEW_COLUMNS: usize = 200;
/// Most terminal cells a column name or a label takes.
pub const MAX_LABEL_CELLS: usize = 40;
/// Most characters a title takes.
pub const MAX_TITLE_CHARS: usize = 120;
/// Most characters a unit takes.
pub const MAX_UNIT_CHARS: usize = 12;
/// Most bytes a diagram's source takes: [`bake_mermaid`]'s own limit.
pub const MAX_SOURCE_BYTES: usize = 16 * 1024;
/// Most bytes an SVG image's source takes: [`bake_svg`]'s own limit.
pub const MAX_SVG_BYTES: usize = bake_svg::MAX_SOURCE_BYTES;
/// Columns past which a diagram's result warns that a narrow terminal shows
/// its source.
pub const WIDE_DIAGRAM: usize = 100;
/// Characters of a source line an error quotes.
const QUOTED_LINE_CHARS: usize = 48;

/// The kinds of visual the tool draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VisualKind {
    Bar,
    Stacked,
    Line,
    Area,
    Scatter,
    Pie,
    Heatmap,
    Table,
    Diagram,
    Svg,
}

/// The kinds' names, as errors list them.
const KINDS: &str = "bar, stacked, line, area, scatter, pie, heatmap, table, diagram, or svg";

impl VisualKind {
    /// The kind a `kind` argument names.
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "bar" => Some(Self::Bar),
            "stacked" => Some(Self::Stacked),
            "line" => Some(Self::Line),
            "area" => Some(Self::Area),
            "scatter" => Some(Self::Scatter),
            "pie" => Some(Self::Pie),
            "heatmap" => Some(Self::Heatmap),
            "table" => Some(Self::Table),
            "diagram" => Some(Self::Diagram),
            "svg" => Some(Self::Svg),
            _ => None,
        }
    }

    /// What the result calls it: `bar chart`, `line chart`, `table`,
    /// `diagram`.
    fn noun(self) -> &'static str {
        match self {
            Self::Bar => "bar chart",
            Self::Stacked => "stacked bar chart",
            Self::Line => "line chart",
            Self::Area => "area chart",
            Self::Scatter => "scatter chart",
            Self::Pie => "pie chart",
            Self::Heatmap => "heatmap",
            Self::Table => "table",
            Self::Diagram => "diagram",
            Self::Svg => "SVG image",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Bar => "bar",
            Self::Stacked => "stacked",
            Self::Line => "line",
            Self::Area => "area",
            Self::Scatter => "scatter",
            Self::Pie => "pie",
            Self::Heatmap => "heatmap",
            Self::Table => "table",
            Self::Diagram => "diagram",
            Self::Svg => "svg",
        }
    }

    /// Whether the columns after the first are numeric series.
    fn charts(self) -> bool {
        !matches!(self, Self::Table | Self::Diagram | Self::Svg)
    }

    /// What a missing value of the kind shows as.
    fn missing(self) -> &'static str {
        match self {
            Self::Bar => "shown without a bar",
            Self::Line | Self::Area => "shown as gaps",
            Self::Heatmap => "shown as blank cells",
            _ => "left out",
        }
    }
}

/// Whether a drawing is asked to be large.
fn large(map: &Map<String, Value>) -> Result<bool, String> {
    match map.get("size") {
        None | Some(Value::Null) => Ok(false),
        Some(Value::String(size)) if size == "normal" => Ok(false),
        Some(Value::String(size)) if size == "large" => Ok(true),
        Some(other) => Err(format!(
            "size must be normal or large, not {}",
            quoted(other)
        )),
    }
}

/// Whether the call asks for the drawing back.
fn previewed(map: &Map<String, Value>) -> Result<bool, String> {
    match map.get("preview") {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(preview)) => Ok(*preview),
        Some(other) => Err(format!(
            "preview must be true or false, not {}",
            quoted(other)
        )),
    }
}

const DESCRIPTION: &str = "Show the user a chart, table, Mermaid diagram, or SVG image in the terminal. Use it to explain: comparisons (bar, stacked), trends (line, area), relationships (scatter), shares (pie), grids of values (heatmap), tables, structure and flows (diagram), or a picture you draw yourself (svg), such as an architecture sketch, a layout, a timeline, or an annotated illustration. The user sees the drawing as you write it; you get a short confirmation, with warnings about what will not show well, so do not repeat the data in prose. For svg, write one self-contained <svg> with a viewBox of about 960 by 400 (1440 by 640 with size large) and a full-size background rect. It draws sharp where the terminal shows images and in coloured blocks elsewhere, so prefer bold shapes, strong contrast, and text of at least 16 units at that scale; reuse shapes with <defs> and <use>. Linked images, scripts, and animation do not show. For a complex svg or diagram, set preview to get the drawing back and check it.";

/// The diagram types `source` may start with, as errors list them.
const DIAGRAM_TYPES: &str = "flowchart (or graph) with TD, TB, BT, LR, or RL, sequenceDiagram, stateDiagram-v2, classDiagram, or erDiagram";

fn parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "kind": {
                "type": "string",
                "enum": ["bar", "stacked", "line", "area", "scatter", "pie", "heatmap", "table", "diagram", "svg"],
                "description": "bar: a bar per row and series; stacked: one bar per row, its series end to end; line: series over the rows in order; area: lines filled down to 0; scatter: points, the first column their x and the rest their y; pie: one series as shares of its total; heatmap: a cell per row and column, coloured by value; table: rows and columns; diagram: Mermaid source; svg: SVG source."
            },
            "title": { "type": "string", "maxLength": MAX_TITLE_CHARS },
            "size": {
                "type": "string",
                "enum": ["normal", "large"],
                "description": "large: a taller plot for line, area, scatter, and pie, and an svg up to 144 by 32 cells instead of 96 by 20. Use it when detail needs the room."
            },
            "preview": {
                "type": "boolean",
                "description": "svg and diagram: true also returns the drawing to you, an svg as an image and a diagram as text, so you can check it and fix what is wrong."
            },
            "columns": {
                "type": "array",
                "maxItems": MAX_HEATMAP_COLUMNS,
                "items": { "type": "string" },
                "description": "Column names. The first labels each row (its category or x); for charts, the rest are numeric series: up to 7, one for pie, up to 24 for heatmap."
            },
            "y": {
                "type": "object",
                "properties": {
                    "min": { "type": "number" },
                    "max": { "type": "number" },
                    "unit": { "type": "string", "maxLength": MAX_UNIT_CHARS }
                },
                "description": "Charts: the value axis (a heatmap's colour scale), when not taken from the data, and the values' unit."
            },
            "rows": {
                "type": "array",
                "maxItems": MAX_ROWS,
                "items": {
                    "type": "array",
                    "items": { "type": ["string", "number", "null"] }
                },
                "description": "One array per row, in column order. Series values are numbers, or null when missing; a scatter's first value is its x, a number."
            },
            "source": {
                "type": "string",
                "maxLength": MAX_SVG_BYTES,
                "description": "diagram: Mermaid flowchart, sequenceDiagram, stateDiagram-v2, classDiagram, or erDiagram, at most 16384 bytes. svg: an <svg> document with a viewBox, at most 131072 bytes."
            }
        },
        "required": ["kind"]
    })
}

/// What the tool takes from its session.
#[derive(Clone, Default)]
pub struct VisualizeOptions {
    /// The session's model: one without image input gets a note in place
    /// of a preview's image, and its `inputLimits.images.resize` wins over
    /// [`Self::resize_options`].
    pub model: Option<ModelSource>,
    /// Resize limits of a preview's image when the model has none.
    pub resize_options: Option<ImageResizeOptions>,
    /// The owner of the tool's blocking work: the session's
    /// [`crate::owned_work::WorkGate`], so its shutdown waits for it.
    pub owner: Option<crate::owned_work::WorkGate>,
}

impl std::fmt::Debug for VisualizeOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VisualizeOptions")
            .field("model", &self.model.is_some())
            .field("resize_options", &self.resize_options)
            .field("owner", &self.owner.is_some())
            .finish()
    }
}

/// The `visualize` tool outside a session: any model reads its previews.
pub fn create_visualize_tool_definition() -> ToolDefinition {
    create_visualize_tool_definition_with(VisualizeOptions::default())
}

/// The `visualize` tool of a session.
pub fn create_visualize_tool_definition_with(options: VisualizeOptions) -> ToolDefinition {
    let mut tool = AgentTool::new(
        VISUALIZE_TOOL_NAME,
        VISUALIZE_TOOL_NAME,
        DESCRIPTION,
        parameters(),
        move |_id, args, signal, _update, _ctx| boxed(run(options.clone(), args, signal)),
    );
    tool.read_only = true;
    tool.replay = Some(ToolReplay::Safe);
    // A script's calls never reach the transcript, so its visual would not
    // be seen.
    tool.nested_callable = false;
    ToolDefinition {
        tool,
        prompt_snippet: None,
        prompt_guidelines: None,
    }
}

/// Checks the call and builds its preview on the blocking pool: an SVG
/// image is parsed, laid out with the system's fonts, and rasterized.
async fn run(
    options: VisualizeOptions,
    args: Value,
    signal: Option<AbortSignal>,
) -> Result<AgentToolResult, String> {
    let model = options.model.as_ref().and_then(|model| model());
    let sight = Sight {
        images: model
            .as_ref()
            .is_none_or(|model| model.input.contains(&InputModality::Image)),
        resize: model
            .as_ref()
            .and_then(|model| ImageResizeOptions::from_input_limits(model.input_limits.as_ref()))
            .or(options.resize_options)
            .unwrap_or_default(),
    };
    run_blocking_abortable(signal, options.owner, move || {
        // Only an SVG image with text needs fonts; reading them is slow.
        let fonts = if svg::has_text(&args) {
            svg::system_fonts()
        } else {
            bake_svg::Fonts::none()
        };
        execute(&args, &fonts, sight)
    })
    .await
}

/// What the session's model takes of a preview.
#[derive(Clone, Copy, Debug)]
struct Sight {
    /// It reads images.
    images: bool,
    resize: ImageResizeOptions,
}

fn execute(args: &Value, fonts: &bake_svg::Fonts, sight: Sight) -> Result<AgentToolResult, String> {
    let checked = check_with(args, fonts)?;
    let mut text = checked.text();
    let mut image = None;
    if checked.preview {
        match preview(args, &checked, fonts, sight) {
            Preview::Text(drawing) => {
                text.push('\n');
                text.push_str(&drawing);
            }
            Preview::Image(note, block) => {
                text.push('\n');
                text.push_str(&note);
                image = Some(block);
            }
        }
    }
    let mut content = vec![UserContentBlock::Text(TextContent {
        text,
        text_signature: None,
    })];
    content.extend(image);
    Ok(AgentToolResult {
        content,
        details: Some(checked.details()),
        ..AgentToolResult::default()
    })
}

/// A preview: lines of text after the result's, or a note and an image.
enum Preview {
    Text(String),
    Image(String, UserContentBlock),
}

/// The drawing of a call that asked for it, as the model can take it.
fn preview(args: &Value, checked: &Checked, fonts: &bake_svg::Fonts, sight: Sight) -> Preview {
    let source = args
        .get("source")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match checked.size {
        Size::Svg(_) => svg::preview(source, checked.large, fonts, sight),
        Size::Diagram(outline) if outline.width > MAX_PREVIEW_COLUMNS => Preview::Text(format!(
            "No preview: the diagram is {} columns wide; at most {MAX_PREVIEW_COLUMNS} are returned.",
            outline.width
        )),
        Size::Diagram(outline) => match bake_mermaid::render(source, outline.width.max(1)) {
            Ok(drawing) => Preview::Text(format!(
                "Preview, as drawn in {} columns:\n{}",
                outline.width,
                drawing.trim_end_matches('\n')
            )),
            Err(_) => Preview::Text("No preview: the diagram did not draw.".to_owned()),
        },
        // `check_with` warns that a chart or a table has none.
        Size::Grid { .. } => Preview::Text(String::new()),
    }
}

/// What a valid call shows.
#[derive(Clone, Debug, PartialEq)]
pub struct Checked {
    pub kind: VisualKind,
    pub title: String,
    pub size: Size,
    /// The drawing is asked to be large.
    pub large: bool,
    /// The call asks for the drawing back, and its kind has one to give.
    pub preview: bool,
    /// What the model should know about how the data is shown.
    pub warnings: Vec<String>,
}

/// How much a valid call shows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Size {
    /// A chart's or a table's rows and columns, the label column counted.
    Grid { rows: usize, columns: usize },
    /// A diagram's family, nodes, links, and drawn size.
    Diagram(Outline),
    /// An SVG image's size in user units and its elements.
    Svg(bake_svg::Outline),
}

/// A diagram family's name in a result, and what its nodes and links are
/// called, each in the singular and the plural.
fn family_words(family: Family) -> (&'static str, [&'static str; 2], [&'static str; 2]) {
    match family {
        Family::Flowchart => ("flowchart", ["node", "nodes"], ["edge", "edges"]),
        Family::Sequence => (
            "sequence diagram",
            ["participant", "participants"],
            ["message", "messages"],
        ),
        Family::State => (
            "state diagram",
            ["state", "states"],
            ["transition", "transitions"],
        ),
        Family::Class => (
            "class diagram",
            ["class", "classes"],
            ["relationship", "relationships"],
        ),
        Family::Entity => (
            "ER diagram",
            ["entity", "entities"],
            ["relationship", "relationships"],
        ),
    }
}

/// A family's name in a result's `details`.
fn family_name(family: Family) -> &'static str {
    match family {
        Family::Flowchart => "flowchart",
        Family::Sequence => "sequence",
        Family::State => "state",
        Family::Class => "class",
        Family::Entity => "er",
    }
}

impl Checked {
    /// The result's text: one line, then each warning on its own.
    pub fn text(&self) -> String {
        let title = if self.title.is_empty() {
            String::new()
        } else {
            format!(" \"{}\"", self.title)
        };
        let shown = match self.size {
            Size::Grid { rows, columns } => {
                let rows = count(rows, "row", "rows");
                let others = match self.kind {
                    VisualKind::Heatmap => count(columns - 1, "column", "columns"),
                    kind if kind.charts() => count(columns - 1, "series", "series"),
                    _ => count(columns, "column", "columns"),
                };
                format!("{rows}, {others}")
            }
            Size::Diagram(outline) => {
                let (name, [node, nodes], [link, links]) = family_words(outline.family);
                format!(
                    "{name}, {}, {}",
                    count(outline.nodes, node, nodes),
                    count(outline.links, link, links)
                )
            }
            Size::Svg(outline) => format!(
                "{}×{}, {}",
                units(outline.width),
                units(outline.height),
                count(outline.elements, "element", "elements")
            ),
        };
        let mut text = format!(
            "Shown to the user: {}{title} ({shown}). The user can see it; do not repeat the data in prose.",
            self.kind.noun()
        );
        for warning in &self.warnings {
            text.push_str("\nWarning: ");
            text.push_str(warning);
        }
        text
    }

    fn details(&self) -> Value {
        match self.size {
            Size::Grid { rows, columns } => json!({
                "kind": self.kind.name(),
                "rows": rows,
                "columns": columns,
                "warnings": self.warnings,
            }),
            Size::Diagram(outline) => json!({
                "kind": self.kind.name(),
                "family": family_name(outline.family),
                "nodes": outline.nodes,
                "links": outline.links,
                "width": outline.width,
                "height": outline.height,
                "warnings": self.warnings,
            }),
            Size::Svg(outline) => json!({
                "kind": self.kind.name(),
                "width": outline.width,
                "height": outline.height,
                "elements": outline.elements,
                "warnings": self.warnings,
            }),
        }
    }
}

/// A size in user units, without a fraction when it has none.
fn units(value: f32) -> String {
    let rounded = (value * 100.0).round() / 100.0;
    if rounded.fract() == 0.0 {
        format!("{rounded:.0}")
    } else {
        rounded.to_string()
    }
}

fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// `text` without control characters, a line break or tab as a space, as
/// the terminal draws a label.
pub fn clean_label(text: &str) -> String {
    text.chars()
        .filter_map(|c| match c {
            '\n' | '\r' | '\t' => Some(' '),
            c if c.is_control() => None,
            c => Some(c),
        })
        .collect()
}

/// A value as an error quotes it, cut to 24 characters.
fn quoted(value: &Value) -> String {
    let text = value.to_string();
    if text.chars().count() <= 24 {
        return text;
    }
    let kept: String = text.chars().take(23).collect();
    format!("{kept}…")
}

/// The label rules: a string at most [`MAX_LABEL_CELLS`] cells wide once
/// cleaned.
fn check_label(text: &str, at: &str) -> Result<(), String> {
    let cells = clean_label(text).width();
    if cells > MAX_LABEL_CELLS {
        return Err(format!(
            "{at} is {cells} cells wide; labels are at most {MAX_LABEL_CELLS}"
        ));
    }
    Ok(())
}

/// [`check_with`] without fonts: an SVG image's text is not measured.
pub fn check(args: &Value) -> Result<Checked, String> {
    check_with(args, &bake_svg::Fonts::none())
}

/// Checks a call's arguments as the terminal will draw them: the fields
/// its kind needs and no others it cannot use, every row as long as
/// `columns`, series values that are numbers or `null`, a diagram's or an
/// image's source that the renderer reads, and the limits. The error says
/// what to fix, such as `rows[4] has 3 values; columns has 2`. `fonts` lay
/// out an SVG image's text, so its warnings can name text that passes an
/// edge.
pub fn check_with(args: &Value, fonts: &bake_svg::Fonts) -> Result<Checked, String> {
    let Some(map) = args.as_object() else {
        return Err("the arguments must be an object".into());
    };
    let kind = match map.get("kind") {
        None | Some(Value::Null) => {
            return Err(format!("kind is required: {KINDS}"));
        }
        Some(Value::String(name)) => VisualKind::parse(name)
            .ok_or_else(|| format!("kind must be {KINDS}, not {}", quoted(&json!(name))))?,
        Some(other) => {
            return Err(format!("kind must be {KINDS}, not {}", quoted(other)));
        }
    };
    let noun = kind.noun();
    let title = match map.get("title") {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(title)) => clean_label(title),
        Some(other) => return Err(format!("title must be a string, not {}", quoted(other))),
    };
    let title_chars = title.chars().count();
    if title_chars > MAX_TITLE_CHARS {
        return Err(format!(
            "title has {title_chars} characters; at most {MAX_TITLE_CHARS}"
        ));
    }
    let large = large(map)?;
    let preview = previewed(map)?;
    if kind == VisualKind::Diagram {
        return diagram(map, title, large, preview);
    }
    if kind == VisualKind::Svg {
        return svg::check(map, title, large, preview, fonts);
    }
    if given(map, "source") {
        return Err(format!(
            "source applies to a diagram or an SVG image; a {noun} takes columns and rows"
        ));
    }
    let most = if kind == VisualKind::Heatmap {
        MAX_HEATMAP_COLUMNS
    } else {
        MAX_COLUMNS
    };
    let columns = names(map, noun, most)?;
    let least = if kind.charts() { 2 } else { 1 };
    if columns.len() < least {
        return Err(if kind.charts() {
            format!(
                "a {noun} needs at least 2 columns: the label column, then a series; columns has {}",
                columns.len()
            )
        } else {
            "a table needs at least 1 column".into()
        });
    }
    if kind == VisualKind::Pie && columns.len() != 2 {
        return Err(format!(
            "a pie chart takes 2 columns: the label column, then one series; columns has {}",
            columns.len()
        ));
    }
    let mut warnings = Vec::new();
    let axis = axis(map, kind)?;
    let rows = match map.get("rows") {
        None | Some(Value::Null) => return Err(format!("rows is required for a {noun}")),
        Some(Value::Array(rows)) => rows,
        Some(other) => return Err(format!("rows must be an array, not {}", quoted(other))),
    };
    if rows.is_empty() {
        return Err("rows is empty; give at least one row".into());
    }
    if rows.len() > MAX_ROWS {
        return Err(format!(
            "rows has {} entries; at most {MAX_ROWS}",
            rows.len()
        ));
    }
    // A share of a whole has no value below 0.
    let whole = matches!(kind, VisualKind::Stacked | VisualKind::Pie);
    let mut missing = 0;
    let mut outside = 0;
    let mut negative = 0;
    for (r, row) in rows.iter().enumerate() {
        let Value::Array(cells) = row else {
            return Err(format!(
                "rows[{r}] must be an array of values in column order, not {}",
                quoted(row)
            ));
        };
        if cells.len() != columns.len() {
            return Err(format!(
                "rows[{r}] has {}; columns has {}",
                count(cells.len(), "value", "values"),
                columns.len()
            ));
        }
        for (c, cell) in cells.iter().enumerate() {
            let at = format!("rows[{r}][{c}]");
            let series = kind.charts() && c > 0;
            // A point's x is a number, as its y values are.
            let x = kind == VisualKind::Scatter && c == 0;
            match cell {
                Value::Number(number) if series => match number.as_f64() {
                    Some(value) if value.is_finite() => {
                        if whole && value < 0.0 {
                            negative += 1;
                        } else if kind != VisualKind::Pie
                            && (axis.min.is_some_and(|min| value < min)
                                || axis.max.is_some_and(|max| value > max))
                        {
                            outside += 1;
                        }
                    }
                    _ => missing += 1,
                },
                Value::Null if series => missing += 1,
                _ if series => {
                    return Err(format!(
                        "{at} is {}; series values must be numbers or null",
                        quoted(cell)
                    ));
                }
                Value::Number(number) if x => {
                    if !number.as_f64().is_some_and(f64::is_finite) {
                        missing += 1;
                    }
                }
                Value::Null if x => missing += 1,
                _ if x => {
                    return Err(format!(
                        "{at} is {}; a scatter chart's first column is each point's x, a number or null",
                        quoted(cell)
                    ));
                }
                Value::String(text) => check_label(text, &at)?,
                Value::Number(_) | Value::Null => {}
                other => {
                    return Err(format!(
                        "{at} is {}; cells must be strings, numbers, or null",
                        quoted(other)
                    ));
                }
            }
        }
    }
    if missing > 0 {
        warnings.push(format!(
            "{} missing, {}.",
            count(missing, "value is", "values are"),
            kind.missing()
        ));
    }
    if negative > 0 {
        warnings.push(format!(
            "{} below 0, left out: a {noun} shows parts of a whole.",
            count(negative, "value is", "values are")
        ));
    }
    if outside > 0 {
        warnings.push(format!(
            "{} outside y.min to y.max, {}.",
            count(outside, "value is", "values are"),
            if kind == VisualKind::Heatmap {
                "shown at the scale's ends"
            } else {
                "cut at the axis"
            }
        ));
    }
    if kind == VisualKind::Pie && rows.len() > MAX_SLICES {
        warnings.push(format!(
            "A pie chart draws {} slices: the {} largest, and the other {} as one.",
            MAX_SLICES,
            MAX_SLICES - 1,
            rows.len() - (MAX_SLICES - 1)
        ));
    }
    if preview {
        warnings.push(format!(
            "preview applies to svg and diagram; a {noun} is drawn from its rows as given."
        ));
    }
    Ok(Checked {
        kind,
        title,
        size: Size::Grid {
            rows: rows.len(),
            columns: columns.len(),
        },
        large,
        preview: false,
        warnings,
    })
}

/// Whether `key` is given, as anything but `null`.
fn given(map: &Map<String, Value>, key: &str) -> bool {
    map.get(key).is_some_and(|value| !value.is_null())
}

/// A diagram's checks: a `source` and none of the charts' fields, read by
/// the renderer the terminal draws it with.
fn diagram(
    map: &Map<String, Value>,
    title: String,
    large: bool,
    preview: bool,
) -> Result<Checked, String> {
    for field in ["columns", "y", "rows"] {
        if given(map, field) {
            return Err(format!(
                "{field} does not apply to a diagram; give its Mermaid text in source"
            ));
        }
    }
    let source = match map.get("source") {
        None | Some(Value::Null) => {
            return Err(format!(
                "source is required for a diagram: Mermaid text that starts with {DIAGRAM_TYPES}"
            ));
        }
        Some(Value::String(source)) => source,
        Some(other) => return Err(format!("source must be a string, not {}", quoted(other))),
    };
    if source.len() > MAX_SOURCE_BYTES {
        return Err(format!(
            "source has {} bytes; at most {MAX_SOURCE_BYTES}",
            source.len()
        ));
    }
    let outline = bake_mermaid::outline(source).map_err(|error| diagnose(source, error))?;
    let mut warnings = Vec::new();
    if outline.width > WIDE_DIAGRAM {
        warnings.push(format!(
            "The diagram is {} columns wide; it may show as source on narrow terminals.",
            outline.width
        ));
    }
    Ok(Checked {
        kind: VisualKind::Diagram,
        title,
        size: Size::Diagram(outline),
        large,
        preview,
        warnings,
    })
}

/// The family a diagram's first statement names, as the renderer reads it.
fn family_of(header: &str) -> Option<Family> {
    let words: Vec<&str> = header.split_whitespace().collect();
    match words.as_slice() {
        ["flowchart" | "graph"] | ["flowchart" | "graph", "TD" | "TB" | "BT" | "LR" | "RL"] => {
            Some(Family::Flowchart)
        }
        ["sequenceDiagram"] => Some(Family::Sequence),
        ["stateDiagram-v2" | "stateDiagram"] => Some(Family::State),
        ["classDiagram"] => Some(Family::Class),
        ["erDiagram"] => Some(Family::Entity),
        _ => None,
    }
}

/// A source line as an error quotes it: cleaned, trimmed, and cut.
fn quoted_line(line: &str) -> String {
    let line = clean_label(line.trim());
    if line.chars().count() <= QUOTED_LINE_CHARS {
        return format!("\"{line}\"");
    }
    let kept: String = line.chars().take(QUOTED_LINE_CHARS - 1).collect();
    format!("\"{kept}…\"")
}

/// What `source` failed with as the model can act on it: the line where
/// the renderer stops reading it, and why, or the limit it passes.
fn diagnose(source: &str, error: RenderError) -> String {
    let lines: Vec<&str> = source.lines().collect();
    // The renderer skips blank lines and `%%` comments, but not `%%{`
    // directives.
    let Some(head) = lines.iter().position(|line| {
        let line = line.trim();
        !line.is_empty() && (!line.starts_with("%%") || line.starts_with("%%{"))
    }) else {
        return format!("source is empty; start it with {DIAGRAM_TYPES}");
    };
    let header = lines[head].split(';').next().unwrap_or_default().trim();
    let at = |k: usize| format!("Mermaid line {} {}", k + 1, quoted_line(lines[k]));
    let Some(family) = family_of(header) else {
        let why = if header.starts_with("---") {
            "front matter is not supported"
        } else if header.starts_with("%%{") {
            "directives are not supported"
        } else {
            "is not a diagram type drawn here"
        };
        return format!("{} {why}; start the source with {DIAGRAM_TYPES}", at(head));
    };
    let reads = |text: &str| bake_mermaid::outline(text);
    let prefix = |end: usize| lines[..end].join("\n");
    if error == RenderError::Limit {
        let end = (head + 1..=lines.len())
            .find(|&end| reads(&prefix(end)) == Err(RenderError::Limit))
            .unwrap_or(lines.len());
        let line = end - 1;
        let before = reads(&prefix(line)).ok();
        let alone = reads(&format!("{header}\n{}", lines[line]));
        return format!(
            "{} passes a limit: {}. Split the diagram into smaller ones, or shorten its labels.",
            at(line),
            limit(family, before, alone == Err(RenderError::Limit))
        );
    }
    // A block still open at the end: the line that opened it.
    let closer = match family {
        Family::Sequence => Some("end"),
        Family::Class | Family::Entity => Some("}"),
        Family::Flowchart | Family::State => None,
    };
    if let Some(closer) = closer
        && reads(&format!("{source}\n{closer}")).is_ok()
        && let Some(open) = opened(family, &lines)
    {
        return format!(
            "{} opens a block that is never closed; close it with {closer}",
            at(open)
        );
    }
    // The first line whose removal lets the rest read, else the first
    // after the longest run that reads.
    let without = |skip: usize| {
        lines
            .iter()
            .enumerate()
            .filter(|&(k, _)| k != skip)
            .map(|(_, line)| *line)
            .collect::<Vec<_>>()
            .join("\n")
    };
    let culprit = (head + 1..lines.len())
        .find(|&k| !lines[k].trim().is_empty() && reads(&without(k)).is_ok())
        .or_else(|| {
            let read = (head + 1..=lines.len())
                .rev()
                .find(|&end| reads(&prefix(end)).is_ok())
                .unwrap_or(head + 1);
            (read..lines.len()).find(|&k| !lines[k].trim().is_empty())
        });
    let Some(line) = culprit else {
        return format!(
            "{} has nothing to draw after it; {}",
            at(head),
            subset(family)
        );
    };
    let why = unsupported(family, lines[line]).unwrap_or("it is outside the Mermaid drawn here");
    format!("{} cannot be drawn: {why}; {}", at(line), subset(family))
}

/// The line that opened the block still open at the end of `lines`.
fn opened(family: Family, lines: &[&str]) -> Option<usize> {
    let mut open = Vec::new();
    for (k, line) in lines.iter().enumerate() {
        let line = line.trim();
        let word = line.split_whitespace().next().unwrap_or_default();
        let opens = match family {
            Family::Sequence => matches!(word, "loop" | "alt" | "opt" | "critical" | "break"),
            _ => line.ends_with('{'),
        };
        let closes = match family {
            Family::Sequence => line == "end",
            _ => line == "}",
        };
        if opens {
            open.push(k);
        } else if closes {
            open.pop();
        }
    }
    open.first().copied()
}

/// Why `line` cannot be drawn, when it is a construct the renderer leaves
/// out on purpose.
fn unsupported(family: Family, line: &str) -> Option<&'static str> {
    let line = line.trim();
    let word = line.split_whitespace().next().unwrap_or_default();
    if line.starts_with("%%{") {
        return Some("directives are not supported");
    }
    if line.chars().any(char::is_control) {
        return Some("it holds a control character");
    }
    if line.match_indices(['#', '&']).any(|(at, _)| {
        let after = &line[at + 1..];
        let len = after
            .bytes()
            .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_')
            .count();
        len > 0 && after[len..].starts_with(';')
    }) {
        return Some("character entities such as &amp; are not decoded; write the character");
    }
    if line.contains(":::") || matches!(word, "style" | "classDef" | "linkStyle" | "click") {
        return Some("styles, classes, and links are not drawn; remove the line");
    }
    match family {
        Family::Flowchart => {
            if matches!(word, "subgraph" | "end") {
                Some("subgraphs are not drawn; draw the nodes without them")
            } else if word == "class" {
                Some("styles, classes, and links are not drawn; remove the line")
            } else if ["((", "[[", "[(", "{{", "[/", "[\\"]
                .iter()
                .any(|shape| line.contains(shape))
            {
                Some("only [box], {decision}, and ([stadium]) node shapes are drawn")
            } else if ["==>", "~~~", "--o", "--x", "o--", "x--"]
                .iter()
                .any(|edge| line.contains(edge))
            {
                Some("only -->, ---, -.->, -.-, and <--> edges are drawn")
            } else {
                None
            }
        }
        Family::Sequence => match word {
            "activate" | "deactivate" => Some("activations are not drawn"),
            "par" | "and" | "rect" | "box" => {
                Some("only loop, alt with one else, opt, critical, and break blocks are drawn")
            }
            "else" => Some("an alt block takes one else, and only alt takes else"),
            "autonumber" | "create" | "destroy" | "link" | "links" | "title" => {
                Some("only participants, messages, notes over, and blocks are drawn")
            }
            "Note" | "note" if !line.contains(" over ") => Some("only Note over is drawn"),
            _ if ["->>+", "->>-", "->+", "->-", "-x+", "-x-", "-)", "--)"]
                .iter()
                .any(|arrow| line.contains(arrow)) =>
            {
                Some("only ->, ->>, -->, -->>, -x, and --x messages without activation are drawn")
            }
            _ => None,
        },
        Family::State => {
            if line.contains('{') || word == "}" {
                Some("composite states are not drawn; keep the states flat")
            } else if matches!(word, "note" | "end") {
                Some("notes are not drawn")
            } else if line.contains("<<") {
                Some("fork, join, and choice states are not drawn")
            } else if line == "--" {
                Some("concurrent regions are not drawn")
            } else {
                None
            }
        }
        Family::Class => {
            if line.contains("<<") {
                Some("annotations such as <<interface>> are not drawn")
            } else if line.contains('~') {
                Some("generic types such as List~T~ are not drawn")
            } else if matches!(word, "namespace" | "note" | "callback" | "link") {
                Some("namespaces, notes, and links are not drawn")
            } else {
                None
            }
        }
        Family::Entity => None,
    }
}

/// What a family draws, for an error to end with.
fn subset(family: Family) -> &'static str {
    match family {
        Family::Flowchart => {
            "flowcharts draw [box], {decision}, and ([stadium]) nodes joined by -->, ---, -.->, -.-, or <-->, with |labels| and & groups"
        }
        Family::Sequence => {
            "sequence diagrams draw participant and actor lines, messages such as A->>B: text, Note over A,B: text, and loop, alt/else, opt, critical, and break blocks closed by end"
        }
        Family::State => {
            "state diagrams draw flat states: A --> B: label, [*] for the start and end, state \"label\" as ID, and ID : description"
        }
        Family::Class => {
            "class diagrams draw class ID { members }, ID : member, and relationships such as A <|-- B, A *-- B, A o-- B, A --> B, or A ..|> B, with \"1\" cardinalities and : labels"
        }
        Family::Entity => {
            "ER diagrams draw ENTITY { type name PK } bodies and relationships such as A ||--o{ B : label"
        }
    }
}

/// The limit a diagram passes, from what the lines before the one that
/// passes it hold, and whether that line passes a limit alone.
fn limit(family: Family, before: Option<Outline>, alone: bool) -> String {
    let (_, [_, nodes], [_, links]) = family_words(family);
    if family == Family::Sequence {
        if before.is_some_and(|outline| outline.nodes == 8) {
            return "a sequence diagram draws at most 8 participants".into();
        }
        if alone {
            return "a label is wider than 40 cells".into();
        }
        return "a sequence diagram draws at most 8 participants, 64 events (messages, notes, and block lines), and 4 nested blocks, with labels of at most 40 cells".into();
    }
    if before.is_some_and(|outline| outline.nodes == 16) {
        return format!("a diagram draws at most 16 {nodes}");
    }
    if before.is_some_and(|outline| outline.links == 24) {
        return format!("a diagram draws at most 24 {links}, each pair of an & group counted");
    }
    if alone {
        return "a label is wider than 40 cells, or an & group names more than 24 nodes".into();
    }
    format!(
        "a diagram draws at most 16 {nodes}, 24 {links}, and 16 members or descriptions each, with labels of at most 40 cells, in at most 65,536 cells"
    )
}

/// The column names, each a label.
fn names(map: &Map<String, Value>, noun: &str, most: usize) -> Result<Vec<String>, String> {
    let columns = match map.get("columns") {
        None | Some(Value::Null) => return Err(format!("columns is required for a {noun}")),
        Some(Value::Array(columns)) => columns,
        Some(other) => return Err(format!("columns must be an array, not {}", quoted(other))),
    };
    if columns.len() > most {
        return Err(format!(
            "columns has {} entries; a {noun} takes at most {most}",
            columns.len()
        ));
    }
    columns
        .iter()
        .enumerate()
        .map(|(c, name)| match name {
            Value::String(name) => {
                check_label(name, &format!("columns[{c}]"))?;
                Ok(name.clone())
            }
            other => Err(format!(
                "columns[{c}] must be a string, not {}",
                quoted(other)
            )),
        })
        .collect()
}

/// A chart's value axis, as `y` gives it.
#[derive(Default)]
struct Axis {
    min: Option<f64>,
    max: Option<f64>,
}

fn axis(map: &Map<String, Value>, kind: VisualKind) -> Result<Axis, String> {
    let y = match map.get("y") {
        None | Some(Value::Null) => return Ok(Axis::default()),
        Some(Value::Object(y)) => y,
        Some(other) => return Err(format!("y must be an object, not {}", quoted(other))),
    };
    if !kind.charts() {
        return Err("y applies to charts; a table has no value axis".into());
    }
    let bound = |key: &str| match y.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(number)) => Ok(number.as_f64().filter(|value| value.is_finite())),
        Some(other) => Err(format!("y.{key} must be a number, not {}", quoted(other))),
    };
    let (min, max) = (bound("min")?, bound("max")?);
    if let (Some(min), Some(max)) = (min, max)
        && min >= max
    {
        return Err(format!(
            "y.min ({}) must be less than y.max ({})",
            json!(min),
            json!(max)
        ));
    }
    match y.get("unit") {
        None | Some(Value::Null) => {}
        Some(Value::String(unit)) => {
            let chars = clean_label(unit).chars().count();
            if chars > MAX_UNIT_CHARS {
                return Err(format!(
                    "y.unit has {chars} characters; at most {MAX_UNIT_CHARS}"
                ));
            }
        }
        Some(other) => return Err(format!("y.unit must be a string, not {}", quoted(other))),
    }
    Ok(Axis { min, max })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(args: Value) -> String {
        match check(&args) {
            Ok(checked) => checked.text(),
            Err(error) => panic!("{error}"),
        }
    }

    fn err(args: Value) -> String {
        match check(&args) {
            Ok(checked) => panic!("accepted: {}", checked.text()),
            Err(error) => error,
        }
    }

    #[test]
    fn a_valid_call_answers_in_one_line() {
        assert_eq!(
            ok(json!({
                "kind": "bar",
                "title": "Build time by crate",
                "columns": ["crate", "debug", "release"],
                "y": { "unit": "s" },
                "rows": [["bake-ai", 12.5, 30], ["bake-tui", 8, 21]],
            })),
            "Shown to the user: bar chart \"Build time by crate\" (2 rows, 2 series). The user can see it; do not repeat the data in prose."
        );
        assert_eq!(
            ok(json!({
                "kind": "table",
                "columns": ["file", "lines"],
                "rows": [["a.rs", 1]],
            })),
            "Shown to the user: table (1 row, 2 columns). The user can see it; do not repeat the data in prose."
        );
        assert_eq!(
            ok(json!({
                "kind": "line",
                "title": "p50",
                "columns": ["day", "ms"],
                "rows": [[1, 3]],
            })),
            "Shown to the user: line chart \"p50\" (1 row, 1 series). The user can see it; do not repeat the data in prose."
        );
    }

    #[test]
    fn gaps_and_values_outside_the_axis_are_warnings() {
        assert_eq!(
            ok(json!({
                "kind": "line",
                "columns": ["day", "a", "b"],
                "y": { "min": 0, "max": 10 },
                "rows": [[1, null, 3], [2, 4, null], [3, 12, -1]],
            })),
            "Shown to the user: line chart (3 rows, 2 series). The user can see it; do not repeat the data in prose.\nWarning: 2 values are missing, shown as gaps.\nWarning: 2 values are outside y.min to y.max, cut at the axis."
        );
        assert!(
            ok(json!({ "kind": "bar", "columns": ["a", "b"], "rows": [["x", null]] }))
                .ends_with("\nWarning: 1 value is missing, shown without a bar.")
        );
    }

    #[test]
    fn errors_say_what_to_fix() {
        let cases = [
            (
                json!({}),
                "kind is required: bar, stacked, line, area, scatter, pie, heatmap, table, diagram, or svg",
            ),
            (
                json!({ "kind": "donut" }),
                "kind must be bar, stacked, line, area, scatter, pie, heatmap, table, diagram, or svg, not \"donut\"",
            ),
            (
                json!({ "kind": "bar" }),
                "columns is required for a bar chart",
            ),
            (
                json!({ "kind": "bar", "columns": ["a"], "rows": [] }),
                "a bar chart needs at least 2 columns: the label column, then a series; columns has 1",
            ),
            (
                json!({ "kind": "line", "columns": ["a", "b"] }),
                "rows is required for a line chart",
            ),
            (
                json!({ "kind": "table", "columns": ["a"], "rows": [] }),
                "rows is empty; give at least one row",
            ),
            (
                json!({ "kind": "bar", "columns": ["a", "b"], "rows": [["x", 1], ["y", 2], ["z", 3], ["w", 4], ["v", 5, 6]] }),
                "rows[4] has 3 values; columns has 2",
            ),
            (
                json!({ "kind": "bar", "columns": ["a", "b"], "rows": [["x", "12ms"]] }),
                "rows[0][1] is \"12ms\"; series values must be numbers or null",
            ),
            (
                json!({ "kind": "table", "columns": ["a"], "rows": [[true]] }),
                "rows[0][0] is true; cells must be strings, numbers, or null",
            ),
            (
                json!({ "kind": "table", "columns": ["a"], "y": { "max": 1 }, "rows": [[1]] }),
                "y applies to charts; a table has no value axis",
            ),
            (
                json!({ "kind": "bar", "columns": ["a", "b"], "y": { "min": 5, "max": 5 }, "rows": [["x", 1]] }),
                "y.min (5.0) must be less than y.max (5.0)",
            ),
            (
                json!({ "kind": "bar", "columns": ["a", "b"], "y": { "unit": "milliseconds!" }, "rows": [["x", 1]] }),
                "y.unit has 13 characters; at most 12",
            ),
            (
                json!({ "kind": "table", "columns": ["a", "b", "c", "d", "e", "f", "g", "h", "i"], "rows": [] }),
                "columns has 9 entries; a table takes at most 8",
            ),
            (
                json!({ "kind": "table", "columns": ["名前".repeat(11)], "rows": [["x"]] }),
                "columns[0] is 44 cells wide; labels are at most 40",
            ),
            (
                json!({ "kind": "table", "columns": ["a"], "rows": [["x".repeat(41)]] }),
                "rows[0][0] is 41 cells wide; labels are at most 40",
            ),
            (
                json!({ "kind": "table", "title": "t".repeat(121), "columns": ["a"], "rows": [[1]] }),
                "title has 121 characters; at most 120",
            ),
            (
                json!({ "kind": "table", "columns": ["a"], "rows": [1] }),
                "rows[0] must be an array of values in column order, not 1",
            ),
            (
                json!({ "kind": "bar", "source": "flowchart\nA", "columns": ["a", "b"], "rows": [["x", 1]] }),
                "source applies to a diagram or an SVG image; a bar chart takes columns and rows",
            ),
        ];
        for (args, expected) in cases {
            assert_eq!(err(args.clone()), expected, "{args}");
        }
        let many: Vec<Value> = (0..=MAX_ROWS).map(|i| json!([i])).collect();
        assert_eq!(
            err(json!({ "kind": "table", "columns": ["a"], "rows": many })),
            "rows has 501 entries; at most 500"
        );
    }

    #[test]
    fn control_characters_are_stripped_before_labels_are_measured() {
        assert_eq!(clean_label("a\u{1b}[31m\nb\u{7}"), "a[31m b");
        let label = format!("{}\u{1}\u{2}", "x".repeat(40));
        assert!(check(&json!({ "kind": "table", "columns": [label], "rows": [[1]] })).is_ok());
    }

    /// Diagram sources built from pieces of Mermaid, valid and not.
    fn any_source() -> impl proptest::strategy::Strategy<Value = String> {
        use proptest::prelude::*;
        const PIECES: &[&str] = &[
            "flowchart LR\n",
            "sequenceDiagram\n",
            "stateDiagram-v2\n",
            "classDiagram\n",
            "erDiagram\n",
            "pie\n",
            "A --> B\n",
            "A[x] -->|y| B{z}\n",
            "A->>B: hi\n",
            "loop x\n",
            "alt y\n",
            "else z\n",
            "end\n",
            "class C {\n",
            "}\n",
            "+int id\n",
            "[*] --> S\n",
            "subgraph s\n",
            "style A fill:#fff\n",
            "activate A\n",
            "%% c\n",
            "%%{init}%%\n",
            "\n",
            ";",
            "&amp;",
            "\u{1b}[31m",
            "请求",
        ];
        prop_oneof![
            prop::collection::vec(prop::sample::select(PIECES), 0..30)
                .prop_map(|pieces| pieces.concat()),
            any::<String>(),
        ]
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(512))]

        // Any source is checked without a panic: it reads exactly when the
        // renderer reads it, and an error is one line with no control
        // character.
        #[test]
        fn any_diagram_source_is_checked_totally(source in any_source()) {
            let args = json!({ "kind": "diagram", "source": source });
            match check(&args) {
                Ok(checked) => {
                    proptest::prop_assert!(bake_mermaid::outline(&source).is_ok());
                    proptest::prop_assert_eq!(checked.kind, VisualKind::Diagram);
                }
                Err(error) => {
                    proptest::prop_assert!(bake_mermaid::outline(&source).is_err());
                    proptest::prop_assert!(!error.chars().any(char::is_control), "{error:?}");
                }
            }
        }
    }

    #[tokio::test]
    async fn the_tool_answers_with_text_and_no_drawing() {
        let definition = create_visualize_tool_definition();
        let tool = &definition.tool;
        assert_eq!(tool.name, VISUALIZE_TOOL_NAME);
        assert!(!tool.nested_callable, "scripts cannot call it");
        assert!(tool.read_only);
        let result = (tool.execute)(
            "call-1".into(),
            json!({ "kind": "bar", "columns": ["a", "b"], "rows": [["x", 1]] }),
            None,
            bake_agent::AgentToolUpdateCallback::ignore(),
            bake_agent::ToolContext::detached(),
        )
        .await
        .expect("result");
        assert_eq!(
            result.details,
            Some(json!({ "kind": "bar", "rows": 1, "columns": 2, "warnings": [] }))
        );
        let failed = (tool.execute)(
            "call-2".into(),
            json!({ "kind": "bar" }),
            None,
            bake_agent::AgentToolUpdateCallback::ignore(),
            bake_agent::ToolContext::detached(),
        )
        .await;
        assert_eq!(
            failed.err().as_deref(),
            Some("columns is required for a bar chart")
        );
        let diagram = (tool.execute)(
            "call-3".into(),
            json!({ "kind": "diagram", "source": "flowchart LR\n    A --> B" }),
            None,
            bake_agent::AgentToolUpdateCallback::ignore(),
            bake_agent::ToolContext::detached(),
        )
        .await
        .expect("result");
        assert_eq!(
            diagram.details,
            Some(json!({
                "kind": "diagram",
                "family": "flowchart",
                "nodes": 2,
                "links": 1,
                "width": 12,
                "height": 8,
                "warnings": [],
            }))
        );
        // The frame comes before the data, so it streams first; a
        // diagram's source takes the data's place.
        let keys: Vec<&String> = tool.parameters["properties"]
            .as_object()
            .expect("properties")
            .keys()
            .collect();
        assert_eq!(
            keys,
            [
                "kind", "title", "size", "preview", "columns", "y", "rows", "source"
            ]
        );
        assert_eq!(
            tool.parameters["properties"]["kind"]["enum"],
            json!([
                "bar", "stacked", "line", "area", "scatter", "pie", "heatmap", "table", "diagram",
                "svg"
            ])
        );
        assert_eq!(
            tool.parameters["properties"]["source"]["maxLength"],
            json!(MAX_SVG_BYTES)
        );
    }

    /// A model that reads images, with the default limits.
    const SEES: Sight = Sight {
        images: true,
        resize: ImageResizeOptions {
            max_width: None,
            max_height: None,
            max_bytes: None,
            jpeg_quality: None,
        },
    };

    fn run_plain(args: Value) -> Result<AgentToolResult, String> {
        execute(&args, &bake_svg::Fonts::none(), SEES)
    }

    fn result_text(result: &AgentToolResult) -> &str {
        match &result.content[0] {
            UserContentBlock::Text(text) => &text.text,
            UserContentBlock::Image(_) => panic!("the result starts with text"),
        }
    }

    const DONE: &str = "The user can see it; do not repeat the data in prose.";

    #[test]
    fn each_chart_kind_answers_with_what_it_shows() {
        let cases = [
            (
                json!({ "kind": "stacked", "columns": ["crate", "debug", "release"], "rows": [["a", 1, 2], ["b", 3, 4]] }),
                "stacked bar chart (2 rows, 2 series)",
            ),
            (
                json!({ "kind": "area", "columns": ["day", "ms"], "rows": [[1, 3], [2, 4]] }),
                "area chart (2 rows, 1 series)",
            ),
            (
                json!({ "kind": "scatter", "columns": ["size", "time"], "rows": [[1, 3], [2.5, 4], [9, 1]] }),
                "scatter chart (3 rows, 1 series)",
            ),
            (
                json!({ "kind": "pie", "columns": ["lang", "lines"], "rows": [["Rust", 90], ["JS", 10]] }),
                "pie chart (2 rows, 1 series)",
            ),
            (
                json!({ "kind": "heatmap", "columns": ["day", "00", "06", "12", "18"], "rows": [["Mon", 1, 2, 3, 4]] }),
                "heatmap (1 row, 4 columns)",
            ),
        ];
        for (args, shown) in cases {
            assert_eq!(ok(args), format!("Shown to the user: {shown}. {DONE}"));
        }
        // A heatmap takes more columns than the series a chart can tell
        // apart.
        let hours: Vec<String> = std::iter::once("day".to_owned())
            .chain((0..24).map(|hour| format!("{hour:02}")))
            .collect();
        let row: Vec<Value> = std::iter::once(json!("Mon"))
            .chain((0..24).map(|hour| json!(hour)))
            .collect();
        assert!(check(&json!({ "kind": "heatmap", "columns": hours, "rows": [row] })).is_ok());
    }

    #[test]
    fn each_chart_kind_says_what_it_cannot_show() {
        // Parts of a whole have no value below 0.
        assert_eq!(
            ok(
                json!({ "kind": "stacked", "columns": ["a", "b", "c"], "rows": [["x", -1, 2], ["y", null, 3]] })
            ),
            format!(
                "Shown to the user: stacked bar chart (2 rows, 2 series). {DONE}\nWarning: 1 value is missing, left out.\nWarning: 1 value is below 0, left out: a stacked bar chart shows parts of a whole."
            )
        );
        // A pie draws a few slices and gathers the rest.
        let rows: Vec<Value> = (0..10).map(|i| json!([format!("s{i}"), i + 1])).collect();
        assert_eq!(
            ok(json!({ "kind": "pie", "columns": ["a", "b"], "y": { "min": 5 }, "rows": rows })),
            format!(
                "Shown to the user: pie chart (10 rows, 1 series). {DONE}\nWarning: A pie chart draws 7 slices: the 6 largest, and the other 4 as one."
            )
        );
        // A heatmap's scale holds values outside it at its ends.
        assert_eq!(
            ok(
                json!({ "kind": "heatmap", "columns": ["a", "b", "c"], "y": { "max": 5 }, "rows": [["x", 9, null]] })
            ),
            format!(
                "Shown to the user: heatmap (1 row, 2 columns). {DONE}\nWarning: 1 value is missing, shown as blank cells.\nWarning: 1 value is outside y.min to y.max, shown at the scale's ends."
            )
        );
        // A point without an x is left out.
        assert_eq!(
            ok(json!({ "kind": "scatter", "columns": ["x", "y"], "rows": [[null, 1], [2, 3]] })),
            format!(
                "Shown to the user: scatter chart (2 rows, 1 series). {DONE}\nWarning: 1 value is missing, left out."
            )
        );
        let cases = [
            (
                json!({ "kind": "scatter", "columns": ["x", "y"], "rows": [["mon", 1]] }),
                "rows[0][0] is \"mon\"; a scatter chart's first column is each point's x, a number or null",
            ),
            (
                json!({ "kind": "pie", "columns": ["a", "b", "c"], "rows": [["x", 1, 2]] }),
                "a pie chart takes 2 columns: the label column, then one series; columns has 3",
            ),
            (
                json!({ "kind": "heatmap", "columns": (0..26).map(|i| i.to_string()).collect::<Vec<_>>(), "rows": [[1]] }),
                "columns has 26 entries; a heatmap takes at most 25",
            ),
            (
                json!({ "kind": "line", "size": "huge", "columns": ["a", "b"], "rows": [[1, 2]] }),
                "size must be normal or large, not \"huge\"",
            ),
            (
                json!({ "kind": "line", "preview": "yes", "columns": ["a", "b"], "rows": [[1, 2]] }),
                "preview must be true or false, not \"yes\"",
            ),
        ];
        for (args, expected) in cases {
            assert_eq!(err(args), expected);
        }
    }

    #[test]
    fn size_and_preview_are_read_for_every_kind() {
        let line = json!({ "kind": "line", "size": "large", "preview": true, "columns": ["a", "b"], "rows": [[1, 2]] });
        let checked = check(&line).unwrap();
        assert!(checked.large && !checked.preview);
        assert_eq!(
            checked.warnings,
            ["preview applies to svg and diagram; a line chart is drawn from its rows as given."]
        );
        let plain = json!({ "kind": "line", "size": "normal", "preview": false, "columns": ["a", "b"], "rows": [[1, 2]] });
        let checked = check(&plain).unwrap();
        assert!(!checked.large && !checked.preview && checked.warnings.is_empty());
    }

    const PICTURE: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 320 180"><rect width="320" height="180" fill="#101319"/><circle cx="90" cy="90" r="60" fill="#e8a33d"/><rect x="180" y="60" width="100" height="60" fill="#fff"/></svg>"##;

    #[test]
    fn an_svg_image_answers_with_its_size_and_elements() {
        assert_eq!(
            ok(json!({ "kind": "svg", "title": "Status", "source": PICTURE })),
            format!("Shown to the user: SVG image \"Status\" (320×180, 3 elements). {DONE}")
        );
        let result = run_plain(json!({ "kind": "svg", "source": PICTURE })).unwrap();
        assert_eq!(result.content.len(), 1);
        assert_eq!(
            result.details,
            Some(
                json!({ "kind": "svg", "width": 320.0, "height": 180.0, "elements": 3, "warnings": [] })
            )
        );
        // What will not show is named.
        let linked = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10"><image href="a.png" width="10" height="10"/><script>x()</script></svg>"#;
        assert_eq!(
            ok(json!({ "kind": "svg", "source": linked })),
            format!(
                "Shown to the user: SVG image (10×10, 2 elements). {DONE}\nWarning: The SVG has no shapes or text to draw.\nWarning: Linked images are not loaded; they show as nothing.\nWarning: Scripts and animation do not run; the image is drawn as written."
            )
        );
    }

    // What the model cannot see for itself: shapes off the canvas and a
    // background that is the terminal's.
    #[test]
    fn an_svg_image_s_warnings_say_what_does_not_show() {
        let lost = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 50"><rect width="100" height="50" fill="#222"/><circle cx="300" cy="20" r="10" fill="#fff"/><rect x="10" y="80" width="20" height="5"/></svg>"##;
        assert_eq!(
            check(&json!({ "kind": "svg", "source": lost }))
                .unwrap()
                .warnings,
            [
                "2 of 3 shapes are wholly outside the canvas (0 0 100 50) and do not show; check their coordinates."
            ]
        );
        let bare = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100"><rect width="50" height="100" fill="#e8a33d"/></svg>"##;
        assert_eq!(
            check(&json!({ "kind": "svg", "source": bare }))
                .unwrap()
                .warnings,
            [
                "About 50% of the image is transparent and shows the terminal's background, which may be dark or light; add a full-size background rect so its colours read as intended."
            ]
        );
        // Without fonts, text is not measured, and the result says so.
        let words = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 50"><rect width="100" height="50" fill="#222"/><text x="5" y="20" font-size="12" fill="#fff">Hi</text></svg>"##;
        assert_eq!(
            check(&json!({ "kind": "svg", "source": words }))
                .unwrap()
                .warnings,
            [
                "The image's text was not measured: no fonts were read here, and without fonts text is not drawn."
            ]
        );
    }

    // Text as the system's fonts lay it out: outside the canvas, past an
    // edge, over another text, or too small to read.
    #[test]
    fn an_svg_image_s_text_is_measured_with_the_system_s_fonts() {
        let fonts = svg::system_fonts();
        if fonts.faces() == 0 {
            // A host without fonts lays out no text; nothing to check.
            return;
        }
        let warnings = |body: &str, size: &str| {
            let source = format!(
                r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 960 400"><rect width="960" height="400" fill="#111"/>{body}</svg>"##
            );
            let args = json!({ "kind": "svg", "size": size, "source": source });
            assert!(svg::has_text(&args));
            check_with(&args, &fonts).unwrap().warnings
        };
        // Text that reads draws without a warning.
        assert_eq!(
            warnings(
                r##"<text x="40" y="80" font-size="32" fill="#fff">Ready</text>"##,
                "normal"
            ),
            Vec::<String>::new()
        );
        assert_eq!(
            warnings(
                r##"<text x="2000" y="80" font-size="32" fill="#fff">Lost</text>"##,
                "normal"
            ),
            ["1 text is outside the canvas (0 0 960 400) and does not show: \"Lost\"."]
        );
        let cut = warnings(
            r##"<text x="900" y="80" font-size="32" fill="#fff">Far too long a label</text>"##,
            "normal",
        );
        assert_eq!(cut.len(), 1, "{cut:?}");
        assert!(
            cut[0].starts_with(
                "1 text passes the canvas's edge and is cut: \"Far too long a label\" (the right edge, by "
            ) && cut[0].ends_with("). Move or shorten it, or set text-anchor."),
            "{cut:?}"
        );
        assert_eq!(
            warnings(
                r##"<text x="40" y="80" font-size="32" fill="#fff">Parser</text><text x="60" y="84" font-size="32" fill="#fff">Lexer</text>"##,
                "normal"
            ),
            ["Two texts overlap: \"Parser\" and \"Lexer\". Move them apart."]
        );
        // The same words twice in one place are a shadow, not a mistake.
        assert_eq!(
            warnings(
                r##"<text x="40" y="80" font-size="32" fill="#000">Parser</text><text x="41" y="81" font-size="32" fill="#fff">Parser</text>"##,
                "normal"
            ),
            Vec::<String>::new()
        );
        assert_eq!(
            warnings(
                r##"<text x="40" y="80" font-size="6" fill="#fff">tiny</text><text x="40" y="200" font-size="8" fill="#fff">small</text>"##,
                "normal"
            ),
            [
                "2 texts are under 9 pixels tall even where the terminal shows images (the smallest, \"tiny\", has font-size 6, about 6 pixels); use a font-size of at least 9, or size large."
            ]
        );
        // A large image has more pixels for the same units.
        assert_eq!(
            warnings(
                r##"<text x="40" y="80" font-size="5" fill="#fff">tiny</text><text x="40" y="200" font-size="8" fill="#fff">small</text>"##,
                "large"
            ),
            [
                "1 text is under 9 pixels tall even where the terminal shows images (the smallest, \"tiny\", has font-size 5, about 7 pixels); use a font-size of at least 6."
            ]
        );
    }

    // A preview is the drawing itself: an image for a model that reads
    // images, the text a diagram draws as, and a note where there is none.
    #[test]
    fn a_preview_returns_the_drawing() {
        let args = json!({ "kind": "svg", "preview": true, "source": PICTURE });
        let result = run_plain(args.clone()).unwrap();
        assert_eq!(
            result_text(&result),
            format!(
                "Shown to the user: SVG image (320×180, 3 elements). {DONE}\nPreview attached: the image at 711×400 pixels, as terminals that show images draw it. Other terminals draw it in about 71×40 coloured blocks, where fine detail and small text do not show."
            )
        );
        let [_, UserContentBlock::Image(image)] = &result.content[..] else {
            panic!("an image after the text: {:?}", result.content.len());
        };
        assert_eq!(image.mime_type, "image/png");
        let png = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &image.data)
            .expect("base64");
        assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
        // The header's width and height: 711 by 400.
        assert_eq!(&png[16..24], &[0, 0, 2, 199, 0, 0, 1, 144]);
        // A model that does not read images gets a note, and no image.
        let blind = Sight {
            images: false,
            ..SEES
        };
        let result = execute(&args, &bake_svg::Fonts::none(), blind).unwrap();
        assert_eq!(result.content.len(), 1);
        assert!(
            result_text(&result).ends_with("\nNo preview: the current model does not read images.")
        );
        // Without `preview`, nothing is added.
        let quiet = run_plain(json!({ "kind": "svg", "source": PICTURE })).unwrap();
        assert!(result_text(&quiet).ends_with(DONE));
        // A diagram comes back as the text it draws as.
        let diagram = run_plain(json!({
            "kind": "diagram",
            "preview": true,
            "source": "flowchart LR\n    A --> B",
        }))
        .unwrap();
        assert_eq!(
            result_text(&diagram),
            format!(
                "Shown to the user: diagram (flowchart, 2 nodes, 1 edge). {DONE}\nPreview, as drawn in 12 columns:\n{}",
                bake_mermaid::render("flowchart LR\n    A --> B", 12)
                    .unwrap()
                    .trim_end_matches('\n')
            )
        );
        // A chart has none; the warning says why.
        let chart = run_plain(
            json!({ "kind": "bar", "preview": true, "columns": ["a", "b"], "rows": [["x", 1]] }),
        )
        .unwrap();
        assert!(
            result_text(&chart).ends_with(
                "\nWarning: preview applies to svg and diagram; a bar chart is drawn from its rows as given."
            ),
            "{}",
            result_text(&chart)
        );
    }

    #[test]
    fn an_svg_image_s_errors_say_what_to_fix() {
        let cases = [
            (
                json!({ "kind": "svg" }),
                "source is required for an SVG image: an <svg> document with a viewBox",
            ),
            (
                json!({ "kind": "svg", "source": "<svg/>", "rows": [] }),
                "rows does not apply to an SVG image; give its SVG text in source",
            ),
            (
                json!({ "kind": "svg", "source": "<html/>" }),
                "source is not an SVG the terminal can draw: the root element must be <svg>",
            ),
            (
                json!({ "kind": "svg", "source": "<svg viewBox=\"0 0 1 1\">\n<g></svg>" }),
                "source is not an SVG the terminal can draw: line 2, column 4: expected 'g' tag, not 'svg' at 2:4",
            ),
            (
                json!({ "kind": "svg", "source": "x".repeat(MAX_SVG_BYTES + 1) }),
                "source is not an SVG the terminal can draw: the source is 131073 bytes; the limit is 131072",
            ),
        ];
        for (args, error) in cases {
            assert_eq!(run_plain(args.clone()).unwrap_err(), error, "{args}");
        }
    }

    #[test]
    fn a_diagram_answers_with_its_family_and_counts() {
        assert_eq!(
            ok(json!({
                "kind": "diagram",
                "title": "Request flow",
                "source": "flowchart TD\n    A[Read] -->|ok| B{Valid?}\n    B -->|yes| C[Save]\n    B -. no .-> A",
            })),
            "Shown to the user: diagram \"Request flow\" (flowchart, 3 nodes, 3 edges). The user can see it; do not repeat the data in prose."
        );
        let cases = [
            (
                "sequenceDiagram\n    participant U as User\n    U->>Bake: prompt\n    Bake-->>U: answer",
                "sequence diagram, 2 participants, 2 messages",
            ),
            (
                "stateDiagram-v2\n    [*] --> Idle\n    Idle --> [*]",
                "state diagram, 3 states, 2 transitions",
            ),
            (
                "classDiagram\n    class Order {\n        +int id\n    }",
                "class diagram, 1 class, 0 relationships",
            ),
            (
                "erDiagram\n    USER ||--o{ ORDER : places",
                "ER diagram, 2 entities, 1 relationship",
            ),
        ];
        for (source, shown) in cases {
            assert_eq!(
                ok(json!({ "kind": "diagram", "source": source })),
                format!(
                    "Shown to the user: diagram ({shown}). The user can see it; do not repeat the data in prose."
                ),
            );
        }
        // Wider than 100 columns at its natural width: a warning, since a
        // narrow terminal shows the source.
        let wide = format!(
            "flowchart LR\n    {}",
            (0..10)
                .map(|i| format!("N{i}[Step number {i}]"))
                .collect::<Vec<_>>()
                .join(" --> ")
        );
        let text = ok(json!({ "kind": "diagram", "source": wide }));
        let width = bake_mermaid::outline(&wide).expect("reads").width;
        assert!(width > WIDE_DIAGRAM, "{width}");
        assert_eq!(
            text,
            format!(
                "Shown to the user: diagram (flowchart, 10 nodes, 9 edges). The user can see it; do not repeat the data in prose.\nWarning: The diagram is {width} columns wide; it may show as source on narrow terminals."
            )
        );
    }

    #[test]
    fn a_diagram_error_names_the_field_line_or_limit_to_fix() {
        let diagram = |source: &str| json!({ "kind": "diagram", "source": source });
        let cases = [
            (
                json!({ "kind": "diagram" }),
                "source is required for a diagram: Mermaid text that starts with flowchart (or graph) with TD, TB, BT, LR, or RL, sequenceDiagram, stateDiagram-v2, classDiagram, or erDiagram".to_owned(),
            ),
            (
                json!({ "kind": "diagram", "source": 3 }),
                "source must be a string, not 3".to_owned(),
            ),
            (
                json!({ "kind": "diagram", "source": "flowchart\nA", "columns": ["a"] }),
                "columns does not apply to a diagram; give its Mermaid text in source".to_owned(),
            ),
            (
                json!({ "kind": "diagram", "source": "flowchart\nA", "rows": [] }),
                "rows does not apply to a diagram; give its Mermaid text in source".to_owned(),
            ),
            (
                json!({ "kind": "diagram", "source": "flowchart\nA", "y": { "max": 1 } }),
                "y does not apply to a diagram; give its Mermaid text in source".to_owned(),
            ),
            (
                diagram(&"%".repeat(MAX_SOURCE_BYTES + 1)),
                "source has 16385 bytes; at most 16384".to_owned(),
            ),
            (
                diagram("  \n%% only a comment\n"),
                "source is empty; start it with flowchart (or graph) with TD, TB, BT, LR, or RL, sequenceDiagram, stateDiagram-v2, classDiagram, or erDiagram".to_owned(),
            ),
            (
                diagram("pie title Pets\n    \"Dogs\" : 3"),
                "Mermaid line 1 \"pie title Pets\" is not a diagram type drawn here; start the source with flowchart (or graph) with TD, TB, BT, LR, or RL, sequenceDiagram, stateDiagram-v2, classDiagram, or erDiagram".to_owned(),
            ),
            (
                diagram("flowchart TD\n    A --> B\n    subgraph api\n    B --> C\n    end"),
                "Mermaid line 3 \"subgraph api\" cannot be drawn: subgraphs are not drawn; draw the nodes without them; flowcharts draw [box], {decision}, and ([stadium]) nodes joined by -->, ---, -.->, -.-, or <-->, with |labels| and & groups".to_owned(),
            ),
            (
                diagram("flowchart TD\n    A --> B\n    style A fill:#f9f"),
                "Mermaid line 3 \"style A fill:#f9f\" cannot be drawn: styles, classes, and links are not drawn; remove the line; flowcharts draw [box], {decision}, and ([stadium]) nodes joined by -->, ---, -.->, -.-, or <-->, with |labels| and & groups".to_owned(),
            ),
            (
                diagram("sequenceDiagram\n    A->>B: one\n    loop retry\n        B-->>A: two\n        activate A\n    end"),
                "Mermaid line 5 \"activate A\" cannot be drawn: activations are not drawn; sequence diagrams draw participant and actor lines, messages such as A->>B: text, Note over A,B: text, and loop, alt/else, opt, critical, and break blocks closed by end".to_owned(),
            ),
            (
                diagram("sequenceDiagram\n    A->>B: one\n    alt ok\n        B-->>A: two"),
                "Mermaid line 3 \"alt ok\" opens a block that is never closed; close it with end".to_owned(),
            ),
            (
                diagram("stateDiagram-v2\n    [*] --> Busy\n    state Busy {\n        A --> B\n    }"),
                "Mermaid line 3 \"state Busy {\" cannot be drawn: composite states are not drawn; keep the states flat; state diagrams draw flat states: A --> B: label, [*] for the start and end, state \"label\" as ID, and ID : description".to_owned(),
            ),
            (
                diagram(&format!("flowchart TD\n{}", (0..17).map(|i| format!("    N{i} --> N{}\n", i + 1)).collect::<String>())),
                "Mermaid line 17 \"N15 --> N16\" passes a limit: a diagram draws at most 16 nodes. Split the diagram into smaller ones, or shorten its labels.".to_owned(),
            ),
            (
                diagram(&format!("flowchart TD\n    A[{}] --> B", "x".repeat(41))),
                "Mermaid line 2 \"A[xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx] --…\" passes a limit: a label is wider than 40 cells, or an & group names more than 24 nodes. Split the diagram into smaller ones, or shorten its labels.".to_owned(),
            ),
            (
                diagram(&format!("sequenceDiagram\n{}", (0..9).map(|i| format!("    participant P{i}\n")).collect::<String>())),
                "Mermaid line 10 \"participant P8\" passes a limit: a sequence diagram draws at most 8 participants. Split the diagram into smaller ones, or shorten its labels.".to_owned(),
            ),
        ];
        for (args, expected) in cases {
            assert_eq!(err(args.clone()), expected, "{args}");
        }
    }
}
