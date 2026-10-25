//! The rows of the `/tree` and `/fork` sheets.
//!
//! `/tree` adapts Pi v1.2.0
//! `packages/coding-agent/src/modes/interactive/components/tree-selector.ts`
//! (`TreeList`: `flattenTree`, `applyFilter` in its default mode,
//! `recalculateVisualStructure`, `findNearestVisibleIndex`, the row prefix
//! of `render`, `getEntryDisplayText`, and `formatToolCall`) to the shared
//! picker sheet: one row per visible entry, the branch holding the leaf
//! first, Pi's indentation, connectors, and gutters, `• ` on the path to
//! the leaf, and the entry's text. The row's value is the entry id. Pi's
//! other filter modes, folding, labels' editing and timestamps, copying,
//! and horizontal panning are not ported; the sheet's own filter searches
//! the rows.
//!
//! `/fork` adapts `components/user-message-selector.ts`: the user messages
//! oldest first, the latest selected.
//!
//! Untrusted session files are walked iteratively with a visited set, so a
//! parent cycle or a very deep tree neither loops nor overflows the stack.
//! Entries that no root reaches, as in a parent cycle, are listed as roots
//! of their own. Indentation stops growing at [`MAX_INDENT`] levels and a
//! long prefix keeps only its end, so a row always shows its text.

use std::collections::HashMap;

use bake_coding_agent::agent_session::ForkPoint;
use bake_coding_agent::session::{SessionTree, SessionTreeNode};
use bake_tui_view::dialog::ListItem;
use serde_json::Value;

/// Indentation levels beyond which the tree stops indenting.
pub const MAX_INDENT: usize = 64;

/// Prefix characters kept before a row's text; a longer prefix keeps its
/// end after `…`.
const MAX_PREFIX: usize = 30;

/// Characters of an entry's content a row shows, as Pi's `extractContent`.
const CONTENT_CHARS: usize = 200;

/// The `/tree` sheet's rows: the items, the row of the leaf (or its nearest
/// shown ancestor) to mark current, and the row to select first when no
/// leaf is shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeRows {
    pub items: Vec<ListItem>,
    pub current: Option<usize>,
    pub selected: Option<usize>,
}

#[derive(Clone)]
struct Gutter {
    position: usize,
    show: bool,
}

struct Visual {
    indent: usize,
    show_connector: bool,
    is_last: bool,
    gutters: Vec<Gutter>,
    virtual_root_child: bool,
}

fn text_of(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect(),
        _ => String::new(),
    }
}

/// Pi's `normalize` and `extractContent`: tabs and line breaks as spaces,
/// trimmed, at most [`CONTENT_CHARS`] characters (the cut before the trim,
/// as Pi cuts first).
fn short(text: &str) -> String {
    let cut: String = text.chars().take(CONTENT_CHARS).collect();
    one_line(&cut)
}

fn one_line(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c == '\n' || c == '\t' || c.is_control() {
                ' '
            } else {
                c
            }
        })
        .collect::<String>()
        .trim()
        .to_owned()
}

fn str_of<'a>(value: Option<&'a Value>, key: &str) -> Option<&'a str> {
    value
        .and_then(|value| value.get(key))
        .and_then(Value::as_str)
}

fn has_text(content: Option<&Value>) -> bool {
    match content {
        Some(Value::String(text)) => !text.trim().is_empty(),
        Some(Value::Array(blocks)) => blocks.iter().any(|block| {
            block.get("type").and_then(Value::as_str) == Some("text")
                && block
                    .get("text")
                    .and_then(Value::as_str)
                    .is_some_and(|text| !text.trim().is_empty())
        }),
        _ => false,
    }
}

/// Pi's default filter: usage entries, bookkeeping entries, and assistant
/// replies that only call tools are hidden; the leaf always shows.
fn visible(node: &SessionTreeNode, leaf: Option<&str>) -> bool {
    let entry = &node.entry;
    let kind = entry.entry_type();
    if kind == "usage" {
        return false;
    }
    if matches!(
        kind,
        "label"
            | "context_edit"
            | "custom"
            | "model_change"
            | "thinking_level_change"
            | "session_info"
    ) {
        return false;
    }
    if kind == "message" && leaf != Some(entry.id()) {
        let message = entry.get("message");
        if str_of(message, "role") == Some("assistant") {
            let stop = str_of(message, "stopReason");
            let error_or_aborted = stop.is_some_and(|stop| stop != "stop" && stop != "toolUse");
            if !has_text(message.and_then(|m| m.get("content"))) && !error_or_aborted {
                return false;
            }
        }
    }
    true
}

/// Pi's `formatToolCall`.
fn format_tool_call(name: &str, args: &Value) -> String {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_default();
    let shorten = |path: String| -> String {
        if !home.is_empty()
            && let Some(rest) = path.strip_prefix(&home)
        {
            return format!("~{rest}");
        }
        path
    };
    let arg = |key: &str| -> Option<String> {
        args.get(key).and_then(|value| match value {
            Value::Null => None,
            Value::String(text) if text.is_empty() => None,
            Value::String(text) => Some(text.clone()),
            other => Some(other.to_string()),
        })
    };
    let path = || shorten(arg("path").or_else(|| arg("file_path")).unwrap_or_default());
    let dir = || shorten(arg("path").unwrap_or_else(|| ".".into()));
    match name {
        "read" => {
            let mut display = path();
            let offset = args.get("offset").and_then(Value::as_i64);
            let limit = args.get("limit").and_then(Value::as_i64);
            if offset.is_some() || limit.is_some() {
                // Wider than the arguments, so untrusted values cannot
                // overflow; Pi omits an end of 0.
                let start = i128::from(offset.unwrap_or(1));
                display.push_str(&format!(":{start}"));
                if let Some(limit) = limit {
                    let end = start + i128::from(limit) - 1;
                    if end != 0 {
                        display.push_str(&format!("-{end}"));
                    }
                }
            }
            format!("[read: {display}]")
        }
        "write" => format!("[write: {}]", path()),
        "edit" => format!("[edit: {}]", path()),
        "bash" => {
            let raw = arg("command").unwrap_or_default();
            let command: String = one_line(&raw).chars().take(50).collect();
            let more = if raw.chars().count() > 50 { "..." } else { "" };
            format!("[bash: {command}{more}]")
        }
        "grep" => format!(
            "[grep: /{}/ in {}]",
            arg("pattern").unwrap_or_default(),
            dir()
        ),
        "find" => format!(
            "[find: {} in {}]",
            arg("pattern").unwrap_or_default(),
            dir()
        ),
        "ls" => format!("[ls: {}]", dir()),
        _ => {
            let json = args.to_string();
            let cut: String = json.chars().take(40).collect();
            let more = if json.chars().count() > 40 { "..." } else { "" };
            format!("[{name}: {cut}{more}]")
        }
    }
}

/// Pi's `getEntryDisplayText`, without colors.
fn display_text(node: &SessionTreeNode, tool_calls: &HashMap<String, (String, Value)>) -> String {
    let entry = &node.entry;
    match entry.entry_type() {
        "message" => {
            let message = entry.get("message");
            let content = message.and_then(|m| m.get("content"));
            match str_of(message, "role").unwrap_or("") {
                "user" => format!("user: {}", short(&text_of(content))),
                "assistant" => {
                    let text = short(&text_of(content));
                    if !text.is_empty() {
                        format!("assistant: {text}")
                    } else if str_of(message, "stopReason") == Some("aborted") {
                        "assistant: (aborted)".into()
                    } else if let Some(error) = str_of(message, "errorMessage") {
                        let error: String = one_line(error).chars().take(80).collect();
                        format!("assistant: {error}")
                    } else {
                        "assistant: (no content)".into()
                    }
                }
                "toolResult" => {
                    let call = str_of(message, "toolCallId").and_then(|id| tool_calls.get(id));
                    match call {
                        Some((name, args)) => format_tool_call(name, args),
                        None => format!("[{}]", str_of(message, "toolName").unwrap_or("tool")),
                    }
                }
                "bashExecution" => {
                    format!(
                        "[bash]: {}",
                        one_line(str_of(message, "command").unwrap_or(""))
                    )
                }
                role => format!("[{role}]"),
            }
        }
        "custom_message" => format!(
            "[{}]: {}",
            entry
                .get("customType")
                .and_then(Value::as_str)
                .unwrap_or(""),
            one_line(&text_of(entry.get("content")))
        ),
        "compaction" => {
            let tokens = entry
                .get("tokensBefore")
                .and_then(Value::as_f64)
                .filter(|tokens| tokens.is_finite())
                .unwrap_or(0.0);
            format!("[compaction: {}k tokens]", (tokens / 1000.0).round())
        }
        "branch_summary" => format!(
            "[branch summary]: {}",
            one_line(entry.get("summary").and_then(Value::as_str).unwrap_or(""))
        ),
        "model_change" => format!(
            "[model: {}]",
            entry.get("modelId").and_then(Value::as_str).unwrap_or("")
        ),
        "thinking_level_change" => format!(
            "[thinking: {}]",
            entry
                .get("thinkingLevel")
                .and_then(Value::as_str)
                .unwrap_or("")
        ),
        "custom" => format!(
            "[custom: {}]",
            entry
                .get("customType")
                .and_then(Value::as_str)
                .unwrap_or("")
        ),
        "session_info" => format!(
            "[title: {}]",
            entry
                .get("name")
                .and_then(Value::as_str)
                .filter(|name| !name.is_empty())
                .unwrap_or("empty")
        ),
        _ => String::new(),
    }
}

/// What the entry is, under the list while it is selected, with its id so
/// typing an id finds it.
fn about(node: &SessionTreeNode) -> String {
    let entry = &node.entry;
    let kind = match entry.entry_type() {
        "message" => match str_of(entry.get("message"), "role") {
            Some("user") => "User message; Enter puts it back in the draft to edit".to_owned(),
            Some(role) => format!("{role} message"),
            None => "message".to_owned(),
        },
        "custom_message" => "Custom message; Enter puts it back in the draft to edit".to_owned(),
        other => other.replace('_', " "),
    };
    format!("{kind} · {}", entry.id())
}

/// The `/tree` rows for `tree` with its leaf at `leaf`.
pub fn tree_rows(tree: &SessionTree, leaf: Option<&str>) -> TreeRows {
    let count = tree.nodes.len();
    let index_of: HashMap<&str, usize> = tree
        .nodes
        .iter()
        .enumerate()
        .map(|(index, node)| (node.entry.id(), index))
        .collect();
    // Pi's `buildActivePath`, bounded: the leaf and its ancestors.
    let mut active = vec![false; count];
    let mut at = leaf.and_then(|leaf| index_of.get(leaf).copied());
    while let Some(index) = at {
        if active[index] {
            break;
        }
        active[index] = true;
        at = tree.nodes[index]
            .entry
            .parent_id()
            .and_then(|parent| index_of.get(parent).copied());
    }
    // Out-of-range indexes, which no `SessionManager` tree holds, are
    // dropped rather than trusted.
    let active_first = |children: &[usize]| -> Vec<usize> {
        let valid = children.iter().copied().filter(|c| *c < count);
        let mut ordered: Vec<usize> = valid.clone().filter(|c| active[*c]).collect();
        ordered.extend(valid.filter(|c| !active[*c]));
        ordered
    };

    // Pi's `flattenTree` order: depth first, the branch holding the leaf
    // first. `walked_parent` is the node each was reached from, which is
    // acyclic even when the file's parent links are not.
    let mut order: Vec<usize> = Vec::with_capacity(count);
    let mut walked_parent: Vec<Option<usize>> = vec![None; count];
    let mut seen = vec![false; count];
    let mut starts = active_first(&tree.roots);
    starts.extend(0..count);
    for start in starts {
        if start >= count || seen[start] {
            continue;
        }
        let mut stack = vec![(start, None)];
        while let Some((index, parent)) = stack.pop() {
            if seen[index] {
                continue;
            }
            seen[index] = true;
            walked_parent[index] = parent;
            order.push(index);
            for child in active_first(&tree.nodes[index].children).into_iter().rev() {
                if child < count && !seen[child] {
                    stack.push((child, Some(index)));
                }
            }
        }
    }

    let mut tool_calls: HashMap<String, (String, Value)> = HashMap::new();
    for node in &tree.nodes {
        let message = node.entry.get("message");
        if node.entry.entry_type() != "message" || str_of(message, "role") != Some("assistant") {
            continue;
        }
        if let Some(Value::Array(blocks)) = message.and_then(|m| m.get("content")) {
            for block in blocks {
                if block.get("type").and_then(Value::as_str) == Some("toolCall")
                    && let (Some(id), Some(name)) = (
                        block.get("id").and_then(Value::as_str),
                        block.get("name").and_then(Value::as_str),
                    )
                {
                    let args = block.get("arguments").cloned().unwrap_or(Value::Null);
                    tool_calls.insert(id.to_owned(), (name.to_owned(), args));
                }
            }
        }
    }

    // Pi's `recalculateVisualStructure` over the visible rows.
    let shown: Vec<usize> = order
        .iter()
        .copied()
        .filter(|index| visible(&tree.nodes[*index], leaf))
        .collect();
    let mut is_shown = vec![false; count];
    for index in &shown {
        is_shown[*index] = true;
    }
    // Each node's nearest shown ancestor, once per node: `order` lists a
    // parent before its children, so a hidden parent's answer is ready.
    let mut nearest: Vec<Option<usize>> = vec![None; count];
    for index in &order {
        nearest[*index] = walked_parent[*index].and_then(|parent| {
            if is_shown[parent] {
                Some(parent)
            } else {
                nearest[parent]
            }
        });
    }
    let mut roots: Vec<usize> = Vec::new();
    let mut children: HashMap<usize, Vec<usize>> = HashMap::new();
    for index in &shown {
        match nearest[*index] {
            Some(parent) => children.entry(parent).or_default().push(*index),
            None => roots.push(*index),
        }
    }
    let multiple_roots = roots.len() > 1;
    let mut visuals: HashMap<usize, Visual> = HashMap::new();
    let mut rows: Vec<usize> = Vec::with_capacity(shown.len());
    let mut stack: Vec<(usize, Visual, bool)> = Vec::new();
    for (position, root) in roots.iter().enumerate().rev() {
        stack.push((
            *root,
            Visual {
                indent: usize::from(multiple_roots),
                show_connector: multiple_roots,
                is_last: position + 1 == roots.len(),
                gutters: Vec::new(),
                virtual_root_child: multiple_roots,
            },
            multiple_roots,
        ));
    }
    while let Some((index, visual, just_branched)) = stack.pop() {
        rows.push(index);
        let kids = children.get(&index).cloned().unwrap_or_default();
        let branches = kids.len() > 1;
        let grows =
            (branches || (just_branched && visual.indent > 0)) && visual.indent < MAX_INDENT;
        let child_indent = if grows {
            visual.indent + 1
        } else {
            visual.indent
        };
        let connector_drawn = visual.show_connector && !visual.virtual_root_child;
        let display = if multiple_roots {
            visual.indent.saturating_sub(1)
        } else {
            visual.indent
        };
        let mut child_gutters = visual.gutters.clone();
        if connector_drawn && visual.indent < MAX_INDENT {
            child_gutters.push(Gutter {
                position: display.saturating_sub(1),
                show: !visual.is_last,
            });
        }
        for (position, kid) in kids.iter().enumerate().rev() {
            stack.push((
                *kid,
                Visual {
                    indent: child_indent,
                    show_connector: branches,
                    is_last: position + 1 == kids.len(),
                    gutters: child_gutters.clone(),
                    virtual_root_child: false,
                },
                branches,
            ));
        }
        visuals.insert(index, visual);
    }

    let row_of: HashMap<usize, usize> = rows
        .iter()
        .enumerate()
        .map(|(row, index)| (*index, row))
        .collect();
    let items: Vec<ListItem> = rows
        .iter()
        .map(|index| {
            let node = &tree.nodes[*index];
            let visual = &visuals[index];
            let display = if multiple_roots {
                visual.indent.saturating_sub(1)
            } else {
                visual.indent
            };
            let connector = visual.show_connector && !visual.virtual_root_child;
            let mut prefix = String::new();
            for i in 0..display * 3 {
                let (level, column) = (i / 3, i % 3);
                let gutter = visual.gutters.iter().find(|g| g.position == level);
                prefix.push(match (gutter, column) {
                    (Some(gutter), 0) if gutter.show => '│',
                    (Some(_), _) => ' ',
                    (None, 0) if connector && level + 1 == display => {
                        if visual.is_last {
                            '└'
                        } else {
                            '├'
                        }
                    }
                    (None, 1) if connector && level + 1 == display => '─',
                    _ => ' ',
                });
            }
            let prefix_chars = prefix.chars().count();
            if prefix_chars > MAX_PREFIX {
                let tail: String = prefix
                    .chars()
                    .skip(prefix_chars - (MAX_PREFIX - 1))
                    .collect();
                prefix = format!("…{tail}");
            }
            let marker = if active[*index] { "• " } else { "" };
            let label = node
                .label
                .as_deref()
                .map(|label| format!("[{}] ", one_line(label)))
                .unwrap_or_default();
            ListItem {
                value: node.entry.id().to_owned(),
                label: format!("{prefix}{marker}{label}{}", display_text(node, &tool_calls)),
                about: Some(about(node)),
                ..ListItem::default()
            }
        })
        .collect();

    // Pi's `findNearestVisibleIndex` from the leaf, else the last row.
    let mut current = None;
    if let Some(index) = leaf.and_then(|leaf| index_of.get(leaf).copied()) {
        current = row_of
            .get(&index)
            .or_else(|| nearest[index].and_then(|shown| row_of.get(&shown)))
            .copied();
    }
    let selected = match current {
        Some(_) => None,
        None => items.len().checked_sub(1),
    };
    TreeRows {
        items,
        current,
        selected,
    }
}

/// The `/fork` rows: each user message on one line, oldest first, with its
/// place among them.
pub fn fork_rows(points: &[ForkPoint]) -> Vec<ListItem> {
    let total = points.len();
    points
        .iter()
        .enumerate()
        .map(|(index, point)| ListItem {
            value: point.entry_id.clone(),
            label: one_line(&point.text),
            detail: Some(format!("Message {} of {total}", index + 1)),
            about: Some(format!("user message · {}", point.entry_id)),
            ..ListItem::default()
        })
        .collect()
}

#[cfg(test)]
#[path = "tests/tree_rows.rs"]
mod tests;
