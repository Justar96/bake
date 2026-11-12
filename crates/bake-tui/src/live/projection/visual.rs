//! A `visualize` call's card: the visual its arguments describe, read as
//! the view draws it, while they stream as well as once they are complete.
//!
//! Arguments are untrusted and, while they stream, partial: the reader
//! takes what it can and never fails. While the call is still being
//! written ([`Args::Streaming`]), or was cut off by an abort or an error
//! ([`Args::Cut`]), the member being written is the arguments' last: its
//! array loses its last element, which may itself be cut, such as `12` of
//! `125`, and a `y` still being written gives no bounds. A diagram's
//! `source` still being written keeps only its complete lines, up to its
//! last newline, and the view draws the longest run of them that renders.
//! A visual whose arguments pass [`PREVIEW_BYTES`] while they stream is
//! drawn only once they are complete, so the drawing never costs more than
//! that per update.

use bake_coding_agent::tools::visualize::{MAX_COLUMNS, MAX_ROWS, MAX_SOURCE_BYTES, VisualKind};
use bake_tui_view::visual::{Cell, Kind, Number, Spec, Visual};

use super::*;

/// Arguments past this many bytes are drawn only once complete.
pub const PREVIEW_BYTES: usize = 32 * 1024;
/// Characters of any string the reader keeps; the view shows fewer.
const STRING_CHARS: usize = 512;

/// How much of a call's arguments has arrived.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Args {
    /// Strictly parsed and final.
    Complete,
    /// Still arriving.
    Streaming,
    /// The response stopped while they were arriving.
    Cut,
}

/// The card of a `visualize` call with `args`, as far as `read` says they
/// arrived, and `outcome`. A call that failed by the tool's error draws the
/// error instead of the visual; one interrupted keeps its drawing.
pub(super) fn visual_row(args: &Value, outcome: Outcome, read: Args) -> Row {
    let streaming = read == Args::Streaming;
    let too_large = streaming && size(args, PREVIEW_BYTES) > PREVIEW_BYTES;
    let mut spec = spec(args, read != Args::Complete);
    if too_large {
        spec.rows.clear();
        spec.source.clear();
    }
    let failed = outcome.state == CallState::Failed;
    Row::Visual(Visual {
        spec,
        state: outcome.state,
        streaming,
        too_large,
        summary: outcome.summary,
        error: if failed { outcome.output } else { Vec::new() },
    })
}

/// The visual `args` describe, leaving out the last element of the member
/// still being written when `provisional`.
pub fn spec(args: &Value, provisional: bool) -> Spec {
    let Value::Object(map) = args else {
        return Spec::default();
    };
    let open = provisional.then(|| map.keys().next_back()).flatten();
    let cut = |key: &str| open.is_some_and(|open| open == key);
    let text = |value: &Value| match value {
        Value::String(text) => Some(text.chars().take(STRING_CHARS).collect::<String>()),
        _ => None,
    };
    let kind = map
        .get("kind")
        .and_then(Value::as_str)
        .and_then(VisualKind::parse)
        .map(|kind| match kind {
            VisualKind::Bar => Kind::Bar,
            VisualKind::Line => Kind::Line,
            VisualKind::Table => Kind::Table,
            VisualKind::Diagram => Kind::Diagram,
        });
    let mut columns: Vec<String> = map
        .get("columns")
        .and_then(Value::as_array)
        .map(|columns| {
            columns
                .iter()
                .take(MAX_COLUMNS)
                .map(|name| text(name).unwrap_or_else(|| cell_text(name)))
                .collect()
        })
        .unwrap_or_default();
    if cut("columns") {
        columns.pop();
    }
    let y = map.get("y").and_then(Value::as_object);
    let bound = |key: &str| {
        y.filter(|_| !cut("y"))
            .and_then(|y| y.get(key))
            .and_then(Value::as_f64)
            .and_then(Number::new)
    };
    let mut rows: Vec<Vec<Cell>> = map
        .get("rows")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .take(MAX_ROWS + 1)
                .map(|row| match row {
                    Value::Array(cells) => cells.iter().take(MAX_COLUMNS).map(cell).collect(),
                    other => vec![cell(other)],
                })
                .collect()
        })
        .unwrap_or_default();
    if cut("rows") || rows.len() > MAX_ROWS {
        rows.pop();
    }
    let partial = cut("source");
    let mut source = map
        .get("source")
        .and_then(Value::as_str)
        .map(|source| {
            // Past the tool's limit the renderer draws nothing, so twice
            // that is enough to show as code.
            let mut end = source.len().min(2 * MAX_SOURCE_BYTES);
            while !source.is_char_boundary(end) {
                end -= 1;
            }
            source[..end].to_owned()
        })
        .unwrap_or_default();
    if partial {
        source.truncate(source.rfind('\n').map_or(0, |at| at + 1));
    }
    Spec {
        kind,
        title: map.get("title").and_then(text).unwrap_or_default(),
        columns,
        min: bound("min"),
        max: bound("max"),
        unit: y
            .and_then(|y| y.get("unit"))
            .and_then(text)
            .unwrap_or_default(),
        rows,
        source,
        partial,
    }
}

/// A row's value as the view reads it: a string, a finite number, or
/// empty; a flag reads as its word.
fn cell(value: &Value) -> Cell {
    match value {
        Value::String(text) => Cell::Text(text.chars().take(STRING_CHARS).collect()),
        Value::Number(number) => number
            .as_f64()
            .and_then(Number::new)
            .map_or(Cell::Empty, Cell::Number),
        Value::Bool(flag) => Cell::Text(flag.to_string()),
        Value::Null | Value::Array(_) | Value::Object(_) => Cell::Empty,
    }
}

/// A value that is not a string, as a column's name.
fn cell_text(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Array(_) | Value::Object(_) => String::new(),
        other => other.to_string(),
    }
}

/// About how many bytes `value` takes as JSON, counting no further than
/// just past `limit`.
fn size(value: &Value, limit: usize) -> usize {
    let mut total = 0;
    let mut stack = vec![value];
    while let Some(value) = stack.pop() {
        total += match value {
            Value::Null | Value::Bool(_) => 5,
            Value::Number(_) => 8,
            Value::String(text) => text.len() + 2,
            Value::Array(_) | Value::Object(_) => 2,
        };
        match value {
            Value::Array(items) => {
                for item in items {
                    if total > limit {
                        break;
                    }
                    total += 1;
                    stack.push(item);
                }
            }
            Value::Object(map) => {
                for (key, item) in map {
                    if total > limit {
                        break;
                    }
                    total += key.len() + 3;
                    stack.push(item);
                }
            }
            _ => {}
        }
        if total > limit {
            break;
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn the_member_being_written_loses_its_last_element() {
        let args = json!({
            "kind": "bar",
            "columns": ["crate", "s"],
            "y": { "unit": "s" },
            "rows": [["a", 1], ["b", 12]],
        });
        assert_eq!(spec(&args, false).rows.len(), 2);
        let streamed = spec(&args, true);
        assert_eq!(
            streamed.rows,
            [vec![
                Cell::Text("a".into()),
                Cell::Number(Number::new(1.0).unwrap())
            ]]
        );
        assert_eq!(streamed.unit, "s");
        // Columns still arriving lose their last name.
        let columns = json!({ "kind": "bar", "columns": ["crate", "de"] });
        assert_eq!(spec(&columns, true).columns, ["crate"]);
        // A bound still arriving is not used.
        let y = json!({ "kind": "bar", "columns": ["a", "b"], "y": { "max": 1 } });
        assert_eq!(spec(&y, true).max, None);
        assert_eq!(spec(&y, false).max, Number::new(1.0));
    }

    #[test]
    fn a_source_being_written_keeps_its_complete_lines() {
        let args = json!({
            "kind": "diagram",
            "title": "Flow",
            "source": "flowchart LR\n    A --> B\n    B --",
        });
        let whole = spec(&args, false);
        assert_eq!(whole.kind, Some(Kind::Diagram));
        assert_eq!(whole.source, "flowchart LR\n    A --> B\n    B --");
        assert!(!whole.partial);
        let streamed = spec(&args, true);
        assert_eq!(streamed.source, "flowchart LR\n    A --> B\n");
        assert!(streamed.partial);
        // No newline yet: nothing.
        let first = json!({ "kind": "diagram", "source": "flowch" });
        assert_eq!(spec(&first, true).source, "");
        // A source followed by another member is complete.
        let before = json!({ "kind": "diagram", "source": "flowchart LR\n    A", "title": "T" });
        let read = spec(&before, true);
        assert_eq!(
            (read.source.as_str(), read.partial),
            ("flowchart LR\n    A", false)
        );
        // A source far past the tool's limit is cut on a character.
        let long =
            json!({ "kind": "diagram", "source": format!("a{}", "é".repeat(MAX_SOURCE_BYTES)) });
        assert_eq!(spec(&long, false).source.len(), MAX_SOURCE_BYTES * 2 - 1);
    }

    #[test]
    fn size_stops_counting_past_its_limit() {
        let rows: Vec<Value> = (0..10_000).map(|i| json!(["row", i])).collect();
        let args = json!({ "rows": rows });
        let counted = size(&args, 100);
        assert!(counted > 100 && counted < 200, "{counted}");
        assert!(size(&json!({ "kind": "bar" }), PREVIEW_BYTES) < 32);
    }
}
