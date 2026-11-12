// Adapted from OpenAI Codex codex-rs/mermaid/src/lib.rs and README.md, tag
// rust-v0.167.2 (ce21072c90d32c40af7a55b60b2fb35c84a43a6a), under the Apache
// License, Version 2.0; see this crate's NOTICE.
// Changed by Bake contributors: the crate documentation below gathers the
// upstream README's grammar, layout, and limits, and the tests include
// Bake's property tests.

//! Bounded, terminal-native Mermaid diagrams as text: Bake's port of
//! OpenAI Codex's `codex-mermaid`.
//!
//! Supports flowcharts, sequences, states, classes, and entity relationships.
//! Graph edges own separate lanes and endpoint positions. Crossings never join routes. Unsupported
//! syntax and outputs exceeding the caller's width return errors, leaving source fallback to
//! the caller. This crate performs no I/O and does not depend on a Mermaid implementation.
//!
//! # Supported subsets
//!
//! - `flowchart`, `graph`: TD/TB (default), BT, LR, RL; rectangle `[label]`,
//!   decision `{label}`, and stadium `([label])` nodes; solid `-->`/`---`/`<-->`
//!   and dashed `-.->`/`-.-`/`<-.->` edges; pipe labels and spaced directed
//!   infix labels (`-- text -->`, `-. text .->`); `&` fan-out/fan-in, chains,
//!   branches, merges, loops.
//! - `sequenceDiagram`: implicit participants, `participant`/`actor`, aliases,
//!   `->`, `->>`, `-->`, `-->>`, `-x`, `--x`, self-messages,
//!   `Note over A[,B]`, nested `loop`/`alt`/`opt`/`critical`/`break`, one
//!   labeled `else` per `alt`.
//! - `stateDiagram-v2`, `stateDiagram`: flat states, `state "label" as ID`,
//!   descriptions, directed transitions with optional labels, initial/final
//!   `[*]`, direction declarations.
//! - `classDiagram`: `class ID`, multiline member bodies, `ID : member`,
//!   solid/dashed links, association, inheritance, composition, aggregation,
//!   dependency, realization, quoted endpoint cardinalities, relationship
//!   labels, direction declarations.
//! - `erDiagram`: entities, multiline attribute bodies,
//!   `type name [PK, FK, UK] ["comment"]`, all four endpoint cardinalities,
//!   identifying/non-identifying relationships, relationship labels,
//!   direction declarations.
//!
//! Identifiers are ASCII letters followed by letters, digits, or underscores.
//! Text supports ordinary Unicode and CJK. Full-line `%%` comments and
//! semicolon-separated statements are supported outside flowchart labels,
//! quoted tokens, and class bodies. Flowchart node and edge labels may be
//! enclosed in double quotes, and node labels support `<br>`, `<br/>`, and
//! `<br />` as line breaks. These are explicit subsets, not complete Mermaid
//! compatibility: compound states, flowchart subgraphs, other shapes,
//! sequence activation and parallel fragments, styling, front matter,
//! directives, other HTML, escapes, combining or zero-width characters, and
//! ligatures with non-additive widths return errors. The library never
//! returns a partial diagram.
//!
//! # Layout and notation
//!
//! Graph nodes appear in declaration or first-reference order, in the
//! requested direction. Each edge gets its own lane and endpoint positions.
//! Crossings use `╪` and never join routes. Decisions use `◇` inside a box;
//! stadiums use rounded corners. Class links use `◁`/`△` for inheritance and
//! realization, `◆` for composition, and `◇` for aggregation; endpoint
//! cardinalities appear in parentheses. Sequence messages keep their order:
//! `->>`/`-->>` end in `▶`/`◀`, `->`/`-->` have no arrowhead, and `-x`/`--x`
//! end in `x`. Lifelines a message crosses show `┼`. States use separate
//! `● initial` and `◎ final` nodes.
//!
//! # Limits
//!
//! Inputs are limited to 16 KiB. Graphs allow 16 nodes, 24 edges (after
//! expanding `&` groups), and 16 members per node; each `&` group allows 24
//! node references. Sequences allow 8 participants, 64 events (including
//! fragment boundaries), and 4 fragment levels. Labels are limited to 40
//! display cells per line and identifiers to 40 ASCII bytes. Canvases are
//! capped at 65,536 cells, independent of the caller's maximum width.
//!
//! [`render_spans`] returns lines of semantic [`Role::Node`], [`Role::Edge`],
//! and [`Role::Text`] spans. Callers apply their own theme; the crate never
//! emits ANSI escapes.

mod draw;
mod output;
mod parse;
mod relations;
mod sequence;
mod state;
mod syntax;

pub use output::Role;
pub use output::Span;
use std::fmt;

const MAX_SOURCE: usize = 16 * 1024;
const MAX_NODES: usize = 16;
const MAX_EDGES: usize = 24;
const MAX_LABEL: usize = 40;
const MAX_CELLS: usize = 64 * 1024;

/// A diagram cannot be faithfully represented by this bounded prototype.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderError {
    /// Syntax or text is outside the explicitly supported subset.
    Unsupported,
    /// A source, diagram, label, or canvas limit was exceeded.
    Limit,
    /// The complete diagram exceeds the supplied display width.
    TooWide,
}

impl fmt::Display for RenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Unsupported => "unsupported Mermaid syntax or label",
            Self::Limit => "diagram exceeds prototype limits",
            Self::TooWide => "diagram exceeds available display width",
        })
    }
}

impl std::error::Error for RenderError {}

/// Render a bounded subset of Mermaid as plain Unicode text.
///
/// Supports flowcharts, sequence diagrams, flat state diagrams, class diagrams, and ER diagrams.
/// See the crate README for each grammar and its limits. Unknown syntax, unsafe terminal text,
/// and diagrams exceeding `max_width` return errors; no partial result is returned.
pub fn render(source: &str, max_width: usize) -> Result<String, RenderError> {
    Ok(render_spans(source, max_width)?
        .into_iter()
        .map(|line| line.into_iter().map(|span| span.text).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n"))
}

/// Render the same bounded diagram as lines of semantic spans for caller-provided styling.
pub fn render_spans(source: &str, max_width: usize) -> Result<Vec<Vec<Span>>, RenderError> {
    if source.len() > MAX_SOURCE {
        return Err(RenderError::Limit);
    }
    let statements = syntax::statements(source)?;
    let (header, body) = statements.split_first().ok_or(RenderError::Unsupported)?;
    match *header {
        "sequenceDiagram" => sequence::render(body, max_width),
        "stateDiagram-v2" | "stateDiagram" | "classDiagram" | "erDiagram" => {
            draw::render(&relations::parse(header, body)?, max_width)
        }
        _ => draw::render(&parse::parse(header, body)?, max_width),
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Direction {
    #[default]
    Down,
    Up,
    Right,
    Left,
}

impl Direction {
    fn parse(text: &str) -> Result<Self, RenderError> {
        match text {
            "TD" | "TB" => Ok(Self::Down),
            "BT" => Ok(Self::Up),
            "LR" => Ok(Self::Right),
            "RL" => Ok(Self::Left),
            _ => Err(RenderError::Unsupported),
        }
    }
}

/// Node shapes supported by the terminal renderer.
#[derive(Debug, PartialEq, Eq)]
enum Shape {
    /// A rectangular node, such as `A[Work]`.
    Rectangle,
    /// A decision node (`A{Done?}`), rendered with a `◇` marker inside a box.
    Decision,
    /// A stadium node (`A([Start])`), rendered as a box with rounded corners.
    Stadium,
}

#[derive(Debug, PartialEq, Eq)]
struct Node {
    id: String,
    label: String,
    shape: Shape,
    declared: bool,
    members: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
struct Edge {
    from: usize,
    to: usize,
    label: String,
    target_label: String,
    source_tip: char,
    target_tip: char,
    dashed: bool,
}

impl Edge {
    fn directed(from: usize, to: usize, label: String) -> Self {
        Self {
            from,
            to,
            label,
            target_label: String::new(),
            source_tip: '─',
            target_tip: '◄',
            dashed: false,
        }
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
struct Graph {
    direction: Direction,
    nodes: Vec<Node>,
    edges: Vec<Edge>,
}

impl Graph {
    fn node(&mut self, id: &str) -> Result<usize, RenderError> {
        if let Some(index) = self.nodes.iter().position(|node| node.id == id) {
            return Ok(index);
        }
        if self.nodes.len() == MAX_NODES {
            return Err(RenderError::Limit);
        }
        self.nodes.push(Node {
            id: id.to_owned(),
            label: id.to_owned(),
            shape: Shape::Rectangle,
            declared: false,
            members: Vec::new(),
        });
        Ok(self.nodes.len() - 1)
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
