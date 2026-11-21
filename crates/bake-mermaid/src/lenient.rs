//! Bake's own: Mermaid as models and people write it, reduced to the
//! subset the family parsers read, so a diagram with decoration the
//! terminal cannot show still draws instead of falling back to source.
//!
//! [`reduce`] works on whole lines and never fails: what it does not know
//! it leaves for the parsers to accept or refuse. It leaves out
//!
//! - front matter (`---` to `---`) and `%%{ ... }%%` directives;
//! - styling and interaction: `style`, `classDef`, `linkStyle`, `click`,
//!   `:::class` suffixes, `accTitle`, and `accDescr` in every family, and
//!   `class A,B name` in flowcharts and state diagrams;
//! - grouping: a flowchart's `subgraph ... end` and its `direction`, a
//!   state diagram's `state X { ... }`, and a class diagram's
//!   `namespace X { ... }` keep what they hold and lose their frame;
//! - a sequence's `activate`, `deactivate`, `autonumber`, `title`,
//!   `destroy`, `link`, and `links` lines, and the frames of `rect` and
//!   `box` blocks; `create participant` reads as `participant`;
//! - a state diagram's notes, `--` region separators, `hide` lines, and
//!   `<<choice>>`, `<<fork>>`, and `<<join>>` declarations, whose states
//!   draw as plain ones;
//! - a class diagram's notes and `<<annotation>>` lines; generic types
//!   written `List~T~` read as `List[T]`.
//!
//! Outside flowcharts, whose node labels break lines themselves, `<br>`
//! reads as a space.

/// The family of a header line, as far as [`reduce`] tells them apart.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Family {
    Flowchart,
    Sequence,
    State,
    Class,
    Entity,
    Other,
}

/// `source` without what the terminal does not draw. See the module's
/// documentation.
pub(super) fn reduce(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut lines = source.lines().peekable();
    // Front matter opens the source and ends at its second `---`.
    while lines.peek().is_some_and(|line| line.trim().is_empty()) {
        lines.next();
    }
    if lines.peek().is_some_and(|line| line.trim() == "---") {
        let rest: Vec<&str> = lines.clone().skip(1).collect();
        if let Some(close) = rest.iter().position(|line| line.trim() == "---") {
            lines.nth(close + 1);
        }
    }
    let mut family = None;
    let mut directive = false;
    // Sequence blocks open: whether each one's frame is kept.
    let mut blocks: Vec<bool> = Vec::new();
    // Braces open in a state or class diagram: whether each is kept.
    let mut braces: Vec<bool> = Vec::new();
    let mut note = false;
    let mut directed = false;
    for line in lines {
        let text = line.trim();
        if directive {
            directive = !text.contains("}%%");
            continue;
        }
        if text.starts_with("%%{") {
            directive = !text.contains("}%%");
            continue;
        }
        let Some(family) = family else {
            if !text.is_empty() && !text.starts_with("%%") {
                family = Some(family_of(text));
            }
            push(&mut out, line);
            continue;
        };
        if text.is_empty() || text.starts_with("%%") || family == Family::Other {
            push(&mut out, line);
            continue;
        }
        let statement = text.trim_end_matches(';').trim_end();
        let word = statement
            .split(|ch: char| ch.is_whitespace() || ch == ':')
            .next()
            .unwrap_or_default();
        let after = statement[word.len()..].trim_start();
        if matches!(word, "style" | "classDef" | "linkStyle" | "click") && !after.is_empty()
            || matches!(word, "accTitle" | "accDescr") && after.starts_with(':')
        {
            continue;
        }
        match family {
            Family::Flowchart => {
                if matches!(word, "subgraph" | "direction" | "class") || statement == "end" {
                    continue;
                }
                push(&mut out, &without_classes(line));
            }
            Family::Sequence => {
                if matches!(
                    word,
                    "activate"
                        | "deactivate"
                        | "autonumber"
                        | "title"
                        | "destroy"
                        | "link"
                        | "links"
                ) {
                    continue;
                }
                if statement == "end" {
                    if blocks.pop() == Some(false) {
                        continue;
                    }
                } else if matches!(word, "rect" | "box") {
                    blocks.push(false);
                    continue;
                } else if matches!(word, "loop" | "alt" | "opt" | "critical" | "break" | "par") {
                    blocks.push(true);
                }
                let line = statement
                    .strip_prefix("create ")
                    .map_or(line, |rest| rest.trim_start());
                push(&mut out, &without_breaks(line));
            }
            Family::State => {
                if note {
                    note = statement != "end note";
                    continue;
                }
                if word == "note" {
                    // A note with its text on the line is one line; else
                    // it runs to `end note`.
                    note = !statement.contains(':');
                    continue;
                }
                if statement == "--" || word == "hide" || word == "class" {
                    continue;
                }
                if statement == "}" && braces.pop().is_some() {
                    continue;
                }
                if word == "direction" {
                    if directed || !braces.is_empty() {
                        continue;
                    }
                    directed = true;
                }
                if word == "state" && statement.contains("<<") {
                    continue;
                }
                let mut kept = without_breaks(&without_classes(line));
                if word == "state" && statement.ends_with('{') {
                    braces.push(false);
                    let opened = kept.trim_end().trim_end_matches(';').trim_end();
                    let opened = opened.strip_suffix('{').unwrap_or(opened).trim_end();
                    // `state X {` names a state its transitions name too;
                    // `state "label" as X {` keeps its label.
                    if !opened.contains('"') {
                        continue;
                    }
                    kept = opened.to_owned();
                }
                push(&mut out, &kept);
            }
            Family::Class => {
                let in_body = braces.last() == Some(&true);
                if statement == "}" {
                    // A brace this did not see open is the parser's to read.
                    if braces.pop() != Some(false) {
                        push(&mut out, line);
                    }
                    continue;
                }
                if statement.starts_with("<<") {
                    continue;
                }
                if !in_body {
                    if matches!(word, "note" | "callback" | "link" | "cssClass") {
                        continue;
                    }
                    if word == "namespace" {
                        if statement.ends_with('{') {
                            braces.push(false);
                        }
                        continue;
                    }
                }
                let mut kept = without_breaks(&without_classes(line));
                if !in_body && word == "class" {
                    // `class Box~T~ <<interface>> {`: the name alone.
                    kept = without_annotation(&without_generics(&kept, true));
                    if kept.trim_end().ends_with('{') {
                        braces.push(true);
                    }
                } else {
                    kept = without_generics(&kept, false);
                }
                push(&mut out, &kept);
            }
            Family::Entity => push(&mut out, &without_breaks(line)),
            Family::Other => push(&mut out, line),
        }
    }
    out
}

fn push(out: &mut String, line: &str) {
    if !out.is_empty() {
        out.push('\n');
    }
    out.push_str(line);
}

fn family_of(header: &str) -> Family {
    let word = header
        .split(|ch: char| ch.is_whitespace() || ch == ';')
        .next()
        .unwrap_or_default();
    match word {
        "flowchart" | "graph" => Family::Flowchart,
        "sequenceDiagram" => Family::Sequence,
        "stateDiagram" | "stateDiagram-v2" => Family::State,
        "classDiagram" => Family::Class,
        "erDiagram" => Family::Entity,
        _ => Family::Other,
    }
}

/// `line` without its `:::class` suffixes, outside quotes.
fn without_classes(line: &str) -> String {
    if !line.contains(":::") {
        return line.to_owned();
    }
    let mut out = String::with_capacity(line.len());
    let mut quoted = false;
    let mut rest = line;
    while let Some(ch) = rest.chars().next() {
        if ch == '"' {
            quoted = !quoted;
        }
        if !quoted && rest.starts_with(":::") {
            let name = rest[3..]
                .bytes()
                .take_while(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
                .count();
            // A class follows what it styles; `: ::: text` is a label.
            if name > 0
                && !out.ends_with(|ch: char| ch.is_whitespace() || ch == ':')
                && !out.is_empty()
            {
                rest = &rest[3 + name..];
                continue;
            }
            out.push_str(":::");
            rest = &rest[3..];
            continue;
        }
        out.push(ch);
        rest = &rest[ch.len_utf8()..];
    }
    out
}

/// `line` with each `<br>` as a space.
fn without_breaks(line: &str) -> String {
    if !line.contains("<br") {
        return line.to_owned();
    }
    line.replace("<br />", " ")
        .replace("<br/>", " ")
        .replace("<br>", " ")
}

/// `line` with each `~T~` pair as `[T]`, or left out when `drop`.
fn without_generics(line: &str, drop: bool) -> String {
    if !line.contains('~') {
        return line.to_owned();
    }
    let mut out = String::with_capacity(line.len());
    let mut open = false;
    for ch in line.chars() {
        if ch == '~' {
            if !drop {
                out.push(if open { ']' } else { '[' });
            }
            open = !open;
        } else if !(drop && open) {
            out.push(ch);
        }
    }
    out
}

/// A class declaration without its `<<annotation>>`.
fn without_annotation(line: &str) -> String {
    let (Some(start), Some(end)) = (line.find("<<"), line.find(">>")) else {
        return line.to_owned();
    };
    if end < start {
        return line.to_owned();
    }
    format!("{}{}", &line[..start], &line[end + 2..])
}

#[cfg(test)]
mod tests {
    use super::reduce;
    use crate::{RenderError, outline, render};

    /// Each decorated source draws exactly as its plain form does.
    #[test]
    fn decoration_is_left_out_and_the_rest_draws() {
        for (decorated, plain) in [
            (
                "---\ntitle: Flow\n---\n%%{init: {'theme': 'dark'}}%%\ngraph TD\n    subgraph api [API layer]\n        direction LR\n        A((Start)):::hot ==> B[(Store)]\n    end\n    B -- saved --- C{{Hex}}\n    C --o D>Flag]\n    D -.-x E(Round)\n    E ---> A\n    classDef hot fill:#f00\n    class A,B hot\n    style A fill:#f9f,stroke:#333;\n    linkStyle 0 stroke:red\n    click A href \"https://example.com\"",
                "graph TD\n    A([Start]) --> B[Store]\n    B -- saved --- C[Hex]\n    C --> D[Flag]\n    D -.-> E([Round])\n    E --> A",
            ),
            (
                "%%{\n  init: {}\n}%%\nflowchart LR\n    A[/In/] == go ==> B[\\Out\\]\n    B <==> C[[Sub]]\n    C -..-> D(((Twice)))",
                "flowchart LR\n    A[In] -- go --> B[Out]\n    B <--> C[Sub]\n    C -.-> D([Twice])",
            ),
            (
                "sequenceDiagram\n    autonumber\n    title Login\n    participant U as User\n    box Backend\n    participant S\n    end\n    U->>+S: sign in\n    activate S\n    rect rgb(0, 0, 0)\n    S-)U: event<br/>sent\n    end\n    Note right of S: checks\n    S-->>-U: ok\n    deactivate S\n    create participant T\n    U<<->>T: both\n    destroy T",
                "sequenceDiagram\n    participant U as User\n    participant S\n    U->>S: sign in\n    S->>U: event sent\n    Note over S: checks\n    S-->>U: ok\n    participant T\n    U->>T: both",
            ),
            (
                "stateDiagram-v2\n    direction LR\n    [*] --> Idle\n    state \"Working hard\" as Busy {\n        direction TB\n        Run --> Wait\n        --\n        Other --> Run\n    }\n    state Pick <<choice>>\n    state Group {\n        In --> Out\n    }\n    Idle --> Busy: go\n    Busy --> Pick\n    note right of Idle: resting\n    note left of Busy\n        long\n    end note\n    Pick --> [*]\n    hide empty description\n    classDef done fill:#0f0\n    class Idle done",
                "stateDiagram-v2\n    direction LR\n    [*] --> Idle\n    state \"Working hard\" as Busy\n    Run --> Wait\n    Other --> Run\n    In --> Out\n    Idle --> Busy: go\n    Busy --> Pick\n    Pick --> [*]",
            ),
            (
                "classDiagram\n    namespace shapes {\n    class Shape~T~ {\n        <<interface>>\n        +List~T~ items\n        +area() float\n    }\n    class Circle:::round\n    }\n    <<abstract>> Circle\n    Shape <|.. Circle\n    note for Circle \"round\"\n    click Circle href \"x\"",
                "classDiagram\n    class Shape {\n        +List[T] items\n        +area() float\n    }\n    class Circle\n    Shape <|.. Circle",
            ),
        ] {
            let drawn = render(decorated, 200);
            assert!(drawn.is_ok(), "{decorated}\n{drawn:?}");
            assert_eq!(drawn, render(plain, 200), "{decorated}");
            assert_eq!(outline(decorated), outline(plain), "{decorated}");
        }
    }

    #[test]
    fn par_and_critical_draw_their_branches() {
        let drawn = render(
            "sequenceDiagram\n    par one\n    A->>B: x\n    and two\n    A->>B: y\n    end\n    critical lock\n    A->>B: z\n    option busy\n    B->>A: no\n    end\n    alt\n    A->>B: a\n    else\n    A->>B: b\n    else again\n    A->>B: c\n    end",
            200,
        )
        .expect("draws");
        for label in [
            "┌─par one─",
            "├─and two─",
            "┌─critical lock─",
            "├─option busy─",
            "┌─alt─",
            "├─else─",
            "├─else again─",
        ] {
            assert!(drawn.contains(label), "{label}\n{drawn}");
        }
        // A branch word outside its own block is still refused.
        for source in [
            "sequenceDiagram\n    loop x\n    and y\n    end",
            "sequenceDiagram\n    par x\n    else y\n    end",
            "sequenceDiagram\n    A->>B: x\n    end",
        ] {
            assert_eq!(
                render(source, 200),
                Err(RenderError::Unsupported),
                "{source}"
            );
        }
    }

    // What looks like decoration inside content stays content.
    #[test]
    fn content_is_kept() {
        for source in [
            "flowchart TD\n    A[\"a:::b\"] --> B",
            "classDiagram\n    class A\n    A: ::: literal",
            "erDiagram\n    accTitle {\n    int id\n    }",
            "sequenceDiagram\n    A->>B: end",
            "pie title Pets\n    style x",
        ] {
            assert_eq!(reduce(source), source);
        }
        // An unclosed front matter is not one.
        assert_eq!(reduce("---\nflowchart TD\nA"), "---\nflowchart TD\nA");
        assert_eq!(reduce(""), "");
    }
}
