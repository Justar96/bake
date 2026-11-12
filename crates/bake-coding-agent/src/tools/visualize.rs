//! The `visualize` tool: Bake's own, with no Pi source. The model calls it
//! to show the user a bar chart, a line chart, a table, or a Mermaid
//! diagram in the terminal.
//!
//! The tool only checks the arguments and answers with one line: what was
//! shown and how much of it, then any warnings, such as values shown as
//! gaps or a diagram too wide for narrow terminals. It has no side effects, so it is not wrapped by permissions and
//! needs no approval. The drawing belongs to the terminal: `bake-tui` draws
//! the call's arguments while they stream and when a session is replayed,
//! at each width, so no drawing is returned to the model or stored in the
//! result's `details`.
//!
//! The tool is opt-in: it is registered with the other built-in tools but
//! is not one of [`crate::tools::index::BAKE_DEFAULT_TOOL_NAMES`], and it
//! stays so until a paired live-model evaluation has measured it. Scripts
//! cannot call it ([`AgentTool::nested_callable`] is cleared): a script's
//! calls never reach the transcript, so a visual drawn there would never be
//! seen. Code mode's `only` mode therefore keeps it declared.
//!
//! The schema lists the frame (`kind`, `title`, `columns`, `y`) before the
//! data (`rows`), so models write it first and the terminal can lay the
//! visual out before its rows arrive. Only `kind` is required, since a
//! `diagram` takes a `source` in place of `columns` and `rows`; the checks
//! each kind needs are made here, since providers enforce schemas unevenly.
//!
//! A diagram's source is read by [`bake_mermaid`], the renderer the
//! terminal draws it with, with no width limit, since the tool does not
//! know the terminal's width. A source it cannot read is an error that
//! names the line and what to change; one wider than [`WIDE_DIAGRAM`]
//! columns is shown with a warning, since a terminal too narrow for it
//! shows its source instead.

use bake_agent::{AgentTool, AgentToolResult, ToolReplay};
use bake_mermaid::{Family, Outline, RenderError};
use serde_json::{Map, Value, json};
use unicode_width::UnicodeWidthStr;

use crate::tools::definition::{ToolDefinition, boxed, text_result};

/// The tool's name.
pub const VISUALIZE_TOOL_NAME: &str = "visualize";
/// Most rows a visual takes.
pub const MAX_ROWS: usize = 500;
/// Most columns a visual takes: a label column and up to seven series.
pub const MAX_COLUMNS: usize = 8;
/// Most terminal cells a column name or a label takes.
pub const MAX_LABEL_CELLS: usize = 40;
/// Most characters a title takes.
pub const MAX_TITLE_CHARS: usize = 120;
/// Most characters a unit takes.
pub const MAX_UNIT_CHARS: usize = 12;
/// Most bytes a diagram's source takes: [`bake_mermaid`]'s own limit.
pub const MAX_SOURCE_BYTES: usize = 16 * 1024;
/// Columns past which a diagram's result warns that a narrow terminal shows
/// its source.
pub const WIDE_DIAGRAM: usize = 100;
/// Characters of a source line an error quotes.
const QUOTED_LINE_CHARS: usize = 48;

/// The kinds of visual the tool draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VisualKind {
    Bar,
    Line,
    Table,
    Diagram,
}

impl VisualKind {
    /// The kind a `kind` argument names.
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "bar" => Some(Self::Bar),
            "line" => Some(Self::Line),
            "table" => Some(Self::Table),
            "diagram" => Some(Self::Diagram),
            _ => None,
        }
    }

    /// What the result calls it: `bar chart`, `line chart`, `table`,
    /// `diagram`.
    fn noun(self) -> &'static str {
        match self {
            Self::Bar => "bar chart",
            Self::Line => "line chart",
            Self::Table => "table",
            Self::Diagram => "diagram",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Bar => "bar",
            Self::Line => "line",
            Self::Table => "table",
            Self::Diagram => "diagram",
        }
    }

    /// Whether the columns after the first are numeric series.
    fn charts(self) -> bool {
        matches!(self, Self::Bar | Self::Line)
    }
}

const DESCRIPTION: &str = "Show the user a bar chart, line chart, table, or Mermaid diagram in the terminal. Use it for comparisons, trends, tables, or structure and flows the user should see at a glance. The user sees the drawing; you get a short confirmation, so do not repeat the data in prose.";

/// The diagram types `source` may start with, as errors list them.
const DIAGRAM_TYPES: &str = "flowchart (or graph) with TD, TB, BT, LR, or RL, sequenceDiagram, stateDiagram-v2, classDiagram, or erDiagram";

fn parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "kind": {
                "type": "string",
                "enum": ["bar", "line", "table", "diagram"],
                "description": "bar: a bar per row and series; line: series over the rows in order; table: rows and columns; diagram: Mermaid source."
            },
            "title": { "type": "string", "maxLength": MAX_TITLE_CHARS },
            "columns": {
                "type": "array",
                "maxItems": MAX_COLUMNS,
                "items": { "type": "string" },
                "description": "Column names. The first labels each row (its category or x); for bar and line, the rest are numeric series."
            },
            "y": {
                "type": "object",
                "properties": {
                    "min": { "type": "number" },
                    "max": { "type": "number" },
                    "unit": { "type": "string", "maxLength": MAX_UNIT_CHARS }
                },
                "description": "bar and line: the value axis, when not taken from the data."
            },
            "rows": {
                "type": "array",
                "maxItems": MAX_ROWS,
                "items": {
                    "type": "array",
                    "items": { "type": ["string", "number", "null"] }
                },
                "description": "One array per row, in column order. Series values are numbers, or null when missing."
            },
            "source": {
                "type": "string",
                "maxLength": MAX_SOURCE_BYTES,
                "description": "diagram: Mermaid flowchart, sequenceDiagram, stateDiagram-v2, classDiagram, or erDiagram."
            }
        },
        "required": ["kind"]
    })
}

/// The `visualize` tool.
pub fn create_visualize_tool_definition() -> ToolDefinition {
    let mut tool = AgentTool::new(
        VISUALIZE_TOOL_NAME,
        VISUALIZE_TOOL_NAME,
        DESCRIPTION,
        parameters(),
        |_id, args, _signal, _update, _ctx| boxed(async move { execute(&args) }),
    );
    tool.read_only = true;
    tool.replay = Some(ToolReplay::Safe);
    // A script's calls never reach the transcript, so its visual would not
    // be seen.
    tool.nested_callable = false;
    ToolDefinition {
        tool,
        prompt_snippet: Some("Show the user a chart, table, or diagram".into()),
        prompt_guidelines: None,
    }
}

fn execute(args: &Value) -> Result<AgentToolResult, String> {
    let checked = check(args)?;
    Ok(text_result(checked.text(), Some(checked.details())))
}

/// What a valid call shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checked {
    pub kind: VisualKind,
    pub title: String,
    pub size: Size,
    /// What the model should know about how the data is shown.
    pub warnings: Vec<String>,
}

/// How much a valid call shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Size {
    /// A chart's or a table's rows and columns, the label column counted.
    Grid { rows: usize, columns: usize },
    /// A diagram's family, nodes, links, and drawn size.
    Diagram(Outline),
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
                let others = if self.kind.charts() {
                    count(columns - 1, "series", "series")
                } else {
                    count(columns, "column", "columns")
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
        }
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

/// Checks a call's arguments as the terminal will draw them: the fields
/// its kind needs and no others it cannot use, every row as long as
/// `columns`, series values that are numbers or `null`, a diagram's source
/// that the renderer reads, and the limits. The error says what to fix,
/// such as `rows[4] has 3 values; columns has 2`.
pub fn check(args: &Value) -> Result<Checked, String> {
    let Some(map) = args.as_object() else {
        return Err("the arguments must be an object".into());
    };
    let kind = match map.get("kind") {
        None | Some(Value::Null) => {
            return Err("kind is required: bar, line, table, or diagram".into());
        }
        Some(Value::String(name)) => VisualKind::parse(name).ok_or_else(|| {
            format!(
                "kind must be bar, line, table, or diagram, not {}",
                quoted(&json!(name))
            )
        })?,
        Some(other) => {
            return Err(format!(
                "kind must be bar, line, table, or diagram, not {}",
                quoted(other)
            ));
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
    if kind == VisualKind::Diagram {
        return diagram(map, title);
    }
    if given(map, "source") {
        return Err(format!(
            "source applies to a diagram; a {noun} takes columns and rows"
        ));
    }
    let columns = names(map, noun)?;
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
    let mut missing = 0;
    let mut outside = 0;
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
            match cell {
                Value::Number(number) if series => match number.as_f64() {
                    Some(value) if value.is_finite() => {
                        if axis.min.is_some_and(|min| value < min)
                            || axis.max.is_some_and(|max| value > max)
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
        let shown = match kind {
            VisualKind::Bar => "without a bar",
            _ => "as gaps",
        };
        warnings.push(format!(
            "{} missing, shown {shown}.",
            count(missing, "value is", "values are")
        ));
    }
    if outside > 0 {
        warnings.push(format!(
            "{} outside y.min to y.max, cut at the axis.",
            count(outside, "value is", "values are")
        ));
    }
    Ok(Checked {
        kind,
        title,
        size: Size::Grid {
            rows: rows.len(),
            columns: columns.len(),
        },
        warnings,
    })
}

/// Whether `key` is given, as anything but `null`.
fn given(map: &Map<String, Value>, key: &str) -> bool {
    map.get(key).is_some_and(|value| !value.is_null())
}

/// A diagram's checks: a `source` and none of the charts' fields, read by
/// the renderer the terminal draws it with.
fn diagram(map: &Map<String, Value>, title: String) -> Result<Checked, String> {
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
fn names(map: &Map<String, Value>, noun: &str) -> Result<Vec<String>, String> {
    let columns = match map.get("columns") {
        None | Some(Value::Null) => return Err(format!("columns is required for a {noun}")),
        Some(Value::Array(columns)) => columns,
        Some(other) => return Err(format!("columns must be an array, not {}", quoted(other))),
    };
    if columns.len() > MAX_COLUMNS {
        return Err(format!(
            "columns has {} entries; at most {MAX_COLUMNS}",
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
        return Err("y applies to bar and line charts; a table has no value axis".into());
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
            (json!({}), "kind is required: bar, line, table, or diagram"),
            (
                json!({ "kind": "pie" }),
                "kind must be bar, line, table, or diagram, not \"pie\"",
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
                "y applies to bar and line charts; a table has no value axis",
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
                "columns has 9 entries; at most 8",
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
                "source applies to a diagram; a bar chart takes columns and rows",
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
        assert_eq!(keys, ["kind", "title", "columns", "y", "rows", "source"]);
        assert_eq!(
            tool.parameters["properties"]["kind"]["enum"],
            json!(["bar", "line", "table", "diagram"])
        );
        assert_eq!(
            tool.parameters["properties"]["source"]["maxLength"],
            json!(MAX_SOURCE_BYTES)
        );
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
