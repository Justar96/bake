//! Bake's property tests: any source and width is total, and a drawing
//! fits its width and holds no control characters, so it can never become
//! a terminal escape sequence.

use proptest::prelude::*;
use unicode_width::UnicodeWidthStr;

use crate::{RenderError, render_spans};

/// Pieces of Mermaid, valid and not, that random sources are built from.
const PIECES: &[&str] = &[
    "flowchart TD",
    "flowchart LR",
    "graph BT",
    "graph RL",
    "sequenceDiagram",
    "stateDiagram-v2",
    "classDiagram",
    "erDiagram",
    "\n",
    "; ",
    " ",
    "A",
    "B",
    "C",
    "Long_name",
    "[Work]",
    "{Done?}",
    "([Start])",
    "[\"quoted ; [x]\"]",
    "[请求]",
    "-->",
    "---",
    "-.->",
    "<-->",
    "-- label -->",
    "-->|yes|",
    " & ",
    "->>",
    "-->>",
    "-x",
    ": message",
    "participant P",
    "actor U as Buyer",
    "Note over A,B: memo",
    "loop retry",
    "alt ok",
    "else no",
    "end",
    "[*] --> S",
    "S --> [*]",
    "state \"Busy\" as S",
    "class Order {",
    "+int id",
    "}",
    "<|--",
    "*--",
    "\"1..*\"",
    "||--o{",
    "string name PK",
    "%% comment",
    "%%{init}%%",
    "<br/>",
    "&amp;",
    "\u{1b}[31m",
    "\t",
    "\u{0}",
    "e\u{301}",
    "🚀",
    "direction LR",
];

/// Mostly valid diagrams, so the drawing itself is exercised: random edges
/// between a few nodes, or random messages between a few participants,
/// with labels of any printable text.
fn diagrams() -> impl Strategy<Value = String> {
    let label = "[a-zA-Z0-9 ?!.,:()<>=+*/'请求界🚀é-]{1,12}";
    let node = prop::sample::select(&["A", "B", "C", "D", "E"][..]);
    let direction = prop::sample::select(&["TD", "BT", "LR", "RL"][..]);
    let edge = prop::sample::select(&["-->", "---", "-.->", "<-->", "-.-"][..]);
    let arrow = prop::sample::select(&["->>", "-->>", "->", "-->", "-x", "--x"][..]);
    prop_oneof![
        (
            direction,
            prop::collection::vec((node.clone(), edge, label, node.clone()), 1..10),
        )
            .prop_map(|(direction, edges)| {
                let mut source = format!("flowchart {direction}\n");
                for (from, edge, label, to) in edges {
                    source.push_str(&format!("{from}[{label}] {edge}|{label}| {to}\n"));
                }
                source
            }),
        prop::collection::vec((node.clone(), arrow, label, node), 1..12).prop_map(|messages| {
            let mut source = "sequenceDiagram\n".to_owned();
            for (from, arrow, label, to) in messages {
                source.push_str(&format!("{from}{arrow}{to}: {label}\n"));
            }
            source
        }),
    ]
}

fn sources() -> impl Strategy<Value = String> {
    prop_oneof![
        diagrams(),
        prop::collection::vec(prop::sample::select(PIECES), 0..40)
            .prop_map(|pieces| pieces.concat()),
        any::<String>(),
        "[ -~\n]{0,200}",
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2048))]

    #[test]
    fn any_source_is_total_and_fits_its_width(source in sources(), width in 0usize..160) {
        match render_spans(&source, width) {
            Ok(lines) => {
                prop_assert!(!lines.is_empty());
                for line in &lines {
                    let text: String = line.iter().map(|span| span.text.as_str()).collect();
                    prop_assert!(text.width() <= width, "{text:?} is wider than {width}");
                    prop_assert!(
                        !text.chars().any(|c| c.is_control()),
                        "{text:?} holds a control character"
                    );
                    prop_assert!(line.iter().all(|span| !span.text.is_empty()));
                }
            }
            Err(RenderError::Unsupported | RenderError::Limit | RenderError::TooWide) => {}
        }
    }

    #[test]
    fn every_prefix_of_a_diagram_is_total(cut in 0usize..2048, width in 0usize..200) {
        // A streamed fence is drawn from prefixes of its source.
        let source = "sequenceDiagram\n    participant A as API\n    loop retry\n        A->>B: ask 请求\n        B-->>A: answer\n    end\n    Note over A,B: done\nflowchart LR\n    A[Start] -->|go| B{ok?} & C([End])";
        let end = source.char_indices().map(|(at, _)| at).nth(cut).unwrap_or(source.len());
        if let Ok(lines) = render_spans(&source[..end], width) {
            for line in &lines {
                let text: String = line.iter().map(|span| span.text.as_str()).collect();
                prop_assert!(text.width() <= width);
                prop_assert!(!text.chars().any(|c| c.is_control()));
            }
        }
    }
}
