//! The `visualize` tool: Bake's own, with no Pi source. The model calls it
//! to show the user a bar chart, a line chart, or a table in the terminal.
//!
//! The tool only checks the arguments and answers with one line: what was
//! shown and how much of it, then any warnings, such as values shown as
//! gaps. It has no side effects, so it is not wrapped by permissions and
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
//! visual out before its rows arrive. Only `kind` is required, so a later
//! `diagram` kind can take a `source` in place of `columns` and `rows`; the
//! checks each kind needs are made here, since providers enforce schemas
//! unevenly.

use bake_agent::{AgentTool, AgentToolResult, ToolReplay};
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

/// The kinds of visual the tool draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VisualKind {
    Bar,
    Line,
    Table,
}

impl VisualKind {
    /// The kind a `kind` argument names.
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "bar" => Some(Self::Bar),
            "line" => Some(Self::Line),
            "table" => Some(Self::Table),
            _ => None,
        }
    }

    /// What the result calls it: `bar chart`, `line chart`, `table`.
    fn noun(self) -> &'static str {
        match self {
            Self::Bar => "bar chart",
            Self::Line => "line chart",
            Self::Table => "table",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Bar => "bar",
            Self::Line => "line",
            Self::Table => "table",
        }
    }

    /// Whether the columns after the first are numeric series.
    fn charts(self) -> bool {
        matches!(self, Self::Bar | Self::Line)
    }
}

const DESCRIPTION: &str = "Show the user a bar chart, line chart, or table in the terminal. Use it for comparisons, trends, or tables the user should see at a glance. The user sees the drawing; you get a short confirmation, so do not repeat the data in prose.";

fn parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "kind": {
                "type": "string",
                "enum": ["bar", "line", "table"],
                "description": "bar: a bar per row and series; line: series over the rows in order; table: rows and columns."
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
        prompt_snippet: Some("Show the user a chart or table".into()),
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
    pub rows: usize,
    pub columns: usize,
    /// What the model should know about how the data is shown.
    pub warnings: Vec<String>,
}

impl Checked {
    /// The result's text: one line, then each warning on its own.
    pub fn text(&self) -> String {
        let title = if self.title.is_empty() {
            String::new()
        } else {
            format!(" \"{}\"", self.title)
        };
        let rows = count(self.rows, "row", "rows");
        let others = if self.kind.charts() {
            count(self.columns - 1, "series", "series")
        } else {
            count(self.columns, "column", "columns")
        };
        let mut text = format!(
            "Shown to the user: {}{title} ({rows}, {others}). The user can see it; do not repeat the data in prose.",
            self.kind.noun()
        );
        for warning in &self.warnings {
            text.push_str("\nWarning: ");
            text.push_str(warning);
        }
        text
    }

    fn details(&self) -> Value {
        json!({
            "kind": self.kind.name(),
            "rows": self.rows,
            "columns": self.columns,
            "warnings": self.warnings,
        })
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
/// `columns`, series values that are numbers or `null`, and the limits.
/// The error says what to fix, such as `rows[4] has 3 values; columns has
/// 2`.
pub fn check(args: &Value) -> Result<Checked, String> {
    let Some(map) = args.as_object() else {
        return Err("the arguments must be an object".into());
    };
    let kind = match map.get("kind") {
        None | Some(Value::Null) => return Err("kind is required: bar, line, or table".into()),
        Some(Value::String(name)) => VisualKind::parse(name).ok_or_else(|| {
            format!(
                "kind must be bar, line, or table, not {}",
                quoted(&json!(name))
            )
        })?,
        Some(other) => {
            return Err(format!(
                "kind must be bar, line, or table, not {}",
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
        rows: rows.len(),
        columns: columns.len(),
        warnings,
    })
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
            (json!({}), "kind is required: bar, line, or table"),
            (
                json!({ "kind": "pie" }),
                "kind must be bar, line, or table, not \"pie\"",
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
        // The frame comes before the data, so it streams first.
        let keys: Vec<&String> = tool.parameters["properties"]
            .as_object()
            .expect("properties")
            .keys()
            .collect();
        assert_eq!(keys, ["kind", "title", "columns", "y", "rows"]);
    }
}
