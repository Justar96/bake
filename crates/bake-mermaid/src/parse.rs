// Adapted from OpenAI Codex codex-rs/mermaid/src/parse.rs, tag rust-v0.167.2
// (ce21072c90d32c40af7a55b60b2fb35c84a43a6a), under the Apache License,
// Version 2.0; see this crate's NOTICE.
// Changed by Bake contributors: other node shapes draw as a box or a stadium,
// and thick, long, and circle- or cross-tipped edges as the plain edge of the
// same direction, where upstream refuses them.
//! Strict parser for a small flowchart grammar; every non-comment byte must be consumed.
//! Node groups expand into explicit edges within the graph edge cap.

use super::Direction;
use super::Edge;
use super::Graph;
use super::MAX_EDGES;
use super::MAX_LABEL;
use super::RenderError;
use super::Shape;
use super::syntax::delimited_label;
use unicode_width::UnicodeWidthChar;
use unicode_width::UnicodeWidthStr;

pub(super) fn parse(header: &str, body: &[&str]) -> Result<Graph, RenderError> {
    let tokens = header.split_whitespace().collect::<Vec<_>>();
    let direction = match tokens.as_slice() {
        ["flowchart" | "graph"] => Direction::Down,
        ["flowchart" | "graph", direction] => Direction::parse(direction)?,
        _ => return Err(RenderError::Unsupported),
    };
    let mut graph = Graph {
        direction,
        ..Graph::default()
    };
    for statement in body {
        let mut rest = *statement;
        let mut from = nodes(&mut rest, &mut graph)?;
        while !rest.trim_start().is_empty() {
            rest = rest.trim_start();
            let mut label = String::new();
            let (source_tip, target_tip, dashed) = if let Some((after, source, target, dashed)) =
                link(rest)
            {
                rest = after.trim_start();
                if let Some(after) = rest.strip_prefix('|') {
                    let (text, remaining) = delimited_label(after, "|")?;
                    label = flowchart_label(text)?.to_owned();
                    check_label(&label)?;
                    rest = remaining;
                }
                (source, target, dashed)
            } else {
                let (after, stem, dashed) = if let Some(after) = rest.strip_prefix("--") {
                    (after, "--", false)
                } else if let Some(after) = rest.strip_prefix("==") {
                    (after, "==", false)
                } else if let Some(after) = rest.strip_prefix("-.") {
                    (after, ".-", true)
                } else {
                    return Err(RenderError::Unsupported);
                };
                // Spaced labels keep endpoint markers out of the text. Stop at the first
                // closing stem instead of swallowing an unsupported edge and its target.
                let (text, remaining) = after.split_once(stem).ok_or(RenderError::Unsupported)?;
                if !text.starts_with(char::is_whitespace) || !text.ends_with(char::is_whitespace) {
                    return Err(RenderError::Unsupported);
                }
                // A longer closing stem is the same edge; without a tip it is a line.
                let remaining = remaining.trim_start_matches(['-', '=']);
                let target_tip = match remaining.strip_prefix(['>', 'o', 'x']) {
                    Some(after) => {
                        rest = after;
                        '◄'
                    }
                    None => {
                        rest = remaining;
                        '─'
                    }
                };
                label = flowchart_label(text.trim())?.to_owned();
                check_label(&label)?;
                ('─', target_tip, dashed)
            };
            let to = nodes(&mut rest, &mut graph)?;
            // Check the Cartesian expansion before allocating edges, including repeated IDs.
            if from.len() * to.len() > MAX_EDGES - graph.edges.len() {
                return Err(RenderError::Limit);
            }
            for &from in &from {
                for &to in &to {
                    graph.edges.push(Edge {
                        from,
                        to,
                        label: label.clone(),
                        target_label: String::new(),
                        source_tip,
                        target_tip,
                        dashed,
                    });
                }
            }
            from = to;
        }
    }
    if graph.nodes.is_empty() {
        return Err(RenderError::Unsupported);
    }
    Ok(graph)
}

fn nodes(rest: &mut &str, graph: &mut Graph) -> Result<Vec<usize>, RenderError> {
    let mut nodes = vec![node(rest, graph)?];
    while let Some(after) = rest.trim_start().strip_prefix('&') {
        if nodes.len() == MAX_EDGES {
            return Err(RenderError::Limit);
        }
        *rest = after;
        nodes.push(node(rest, graph)?);
    }
    Ok(nodes)
}

fn node(rest: &mut &str, graph: &mut Graph) -> Result<usize, RenderError> {
    let id = identifier(rest)?;
    // Reserved constructs must not be interpreted as ordinary node declarations.
    if matches!(
        id,
        "end" | "subgraph" | "direction" | "style" | "class" | "classDef" | "linkStyle" | "click"
    ) {
        return Err(RenderError::Unsupported);
    }
    // Longer shape delimiters come first, so they never become punctuation inside a
    // simpler node. Shapes the terminal has no glyphs for draw as the nearest it has.
    let declaration = [
        ("(((", ")))", Shape::Stadium),
        ("((", "))", Shape::Stadium),
        ("([", "])", Shape::Stadium),
        ("[(", ")]", Shape::Rectangle),
        ("[[", "]]", Shape::Rectangle),
        ("[/", "/]", Shape::Rectangle),
        ("[/", "\\]", Shape::Rectangle),
        ("[\\", "\\]", Shape::Rectangle),
        ("[\\", "/]", Shape::Rectangle),
        ("{{", "}}", Shape::Rectangle),
        ("(", ")", Shape::Stadium),
        (">", "]", Shape::Rectangle),
        ("[", "]", Shape::Rectangle),
        ("{", "}", Shape::Decision),
    ]
    .into_iter()
    .find(|(open, _, _)| rest.starts_with(open));
    let index = graph.node(id)?;
    if let Some((open, close, shape)) = declaration {
        let (label, remaining) = delimited_label(&rest[open.len()..], close)?;
        let label = flowchart_label(label)?
            .replace("<br>", "\n")
            .replace("<br/>", "\n")
            .replace("<br />", "\n");
        if label.trim().is_empty() {
            return Err(RenderError::Unsupported);
        }
        for line in label.split('\n').filter(|line| !line.is_empty()) {
            check_label(line)?;
        }
        *rest = remaining;
        let node = &mut graph.nodes[index];
        if node.declared && (node.label != label || node.shape != shape) {
            return Err(RenderError::Unsupported);
        }
        node.label = label;
        node.shape = shape;
        node.declared = true;
    }
    Ok(index)
}

/// The edge `rest` opens with: what follows it, its source and target tips, and whether it is
/// dashed. Thick (`==>`) and longer (`--->`) edges are the plain edge of the same direction,
/// and circle and cross tips are arrows. `--`, `==`, and `-.` alone open a spaced label and
/// are left to the caller.
fn link(rest: &str) -> Option<(&str, char, char, bool)> {
    let (source, body) = match rest.strip_prefix('<') {
        Some(body) => ('◄', body),
        None => ('─', rest),
    };
    let stem = body.chars().next().filter(|ch| matches!(ch, '-' | '='))?;
    let mut after = body.trim_start_matches(stem);
    let mut length = body.len() - after.len();
    let mut dashed = false;
    if stem == '-' && length == 1 {
        let dots = after.trim_start_matches('.');
        if dots.len() == after.len() {
            return None;
        }
        after = dots.strip_prefix('-')?;
        dashed = true;
        length = 3;
    }
    if length < 2 {
        return None;
    }
    let (target, after) = match after.strip_prefix(['>', 'o', 'x']) {
        Some(after) => ('◄', after),
        None => ('─', after),
    };
    // `--` with no tip opens a spaced label; `<--` with none is no edge.
    if target == '─' && (length < 3 || source == '◄') {
        return None;
    }
    Some((after, source, target, dashed))
}

fn flowchart_label(label: &str) -> Result<&str, RenderError> {
    // Mermaid Markdown strings require rendering beyond ordinary quoted labels.
    if label.starts_with("\"`") {
        return Err(RenderError::Unsupported);
    }
    let label = if let Some(quoted) = label.strip_prefix('"') {
        quoted.strip_suffix('"').ok_or(RenderError::Unsupported)?
    } else {
        if label.contains(['[', ']', '{', '}', '|']) {
            return Err(RenderError::Unsupported);
        }
        label
    };
    if label.contains('"') {
        return Err(RenderError::Unsupported);
    }
    Ok(label)
}

pub(super) fn check_label(label: &str) -> Result<(), RenderError> {
    // Markup needs deliberate decoding/layout; printable comparison operators are plain text.
    let markup = label.match_indices('<').any(|(index, _)| {
        let after = &label[index + 1..];
        after.starts_with(|ch: char| ch.is_ascii_alphabetic() || matches!(ch, '/' | '!' | '?'))
    });
    if markup
        || label.trim().is_empty()
        || label.chars().any(|ch| {
            ch.is_control()
                || matches!(ch, '┌' | '┐' | '└' | '┘' | '├' | '┤' | '╪' | '◄')
                || UnicodeWidthChar::width(ch).is_none_or(|width| width == 0)
        })
    {
        return Err(RenderError::Unsupported);
    }
    // Labels are drawn one Unicode scalar at a time. Reject ligatures whose string width differs
    // from those scalar widths rather than misaligning borders or underallocating the canvas.
    if label
        .chars()
        .filter_map(UnicodeWidthChar::width)
        .sum::<usize>()
        != label.width()
    {
        return Err(RenderError::Unsupported);
    }
    if UnicodeWidthStr::width(label) > MAX_LABEL {
        return Err(RenderError::Limit);
    }
    Ok(())
}

pub(super) fn identifier<'a>(rest: &mut &'a str) -> Result<&'a str, RenderError> {
    *rest = rest.trim_start();
    let len = rest
        .bytes()
        .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_')
        .count();
    let id = &rest[..len];
    if id.is_empty() || !id.as_bytes()[0].is_ascii_alphabetic() || id.len() > MAX_LABEL {
        return Err(RenderError::Unsupported);
    }
    *rest = &rest[len..];
    Ok(id)
}
