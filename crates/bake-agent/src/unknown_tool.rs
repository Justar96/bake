//! The error a call to a tool the model was not given reads: the tools it
//! has, and the one it meant when exactly one matches but for case,
//! separators, or a namespace prefix, so the next call can be right.
//!
//! Bake's own; Pi answers `Tool {name} not found` alone.

/// The most tool names the message lists.
pub const MAX_LISTED_TOOLS: usize = 40;

/// `name` in the form the comparison uses: lowercase, with `-`, `_`, and
/// `.` alike.
fn comparable(name: &str) -> String {
    name.chars()
        .flat_map(char::to_lowercase)
        .map(|c| if matches!(c, '-' | '.') { '_' } else { c })
        .collect()
}

/// The one name in `available` that `requested` equals in the comparable
/// form, `None` when none or several do.
fn only_match<'a>(requested: &str, available: &[&'a str]) -> Option<&'a str> {
    let requested = comparable(requested);
    let mut matching = available
        .iter()
        .filter(|name| comparable(name) == requested);
    let first = matching.next()?;
    matching.next().is_none().then_some(*first)
}

/// `requested` without a namespace prefix: what follows its last `.`, `:`,
/// or `__`; `None` when it has none or nothing follows.
fn without_prefix(requested: &str) -> Option<&str> {
    let end = [
        requested.rfind('.').map(|at| at + 1),
        requested.rfind(':').map(|at| at + 1),
        requested.rfind("__").map(|at| at + 2),
    ]
    .into_iter()
    .flatten()
    .max()?;
    Some(&requested[end..]).filter(|rest| !rest.is_empty())
}

/// The tool `requested` most likely means: the one name in `available`
/// it equals after lowercasing and treating `-`, `_`, and `.` alike, else
/// the one its name without a namespace prefix equals so. Never a guess by
/// similarity.
pub fn suggested_tool<'a>(requested: &str, available: &[&'a str]) -> Option<&'a str> {
    only_match(requested, available)
        .or_else(|| without_prefix(requested).and_then(|rest| only_match(rest, available)))
        .filter(|name| *name != requested)
}

/// The error for a call to `requested`, which is not among `available`,
/// the tools callable where it was made, in their order: the suggestion,
/// if any, then at most [`MAX_LISTED_TOOLS`] names.
pub fn unknown_tool_message(requested: &str, available: &[&str]) -> String {
    let mut message = format!("Tool {requested} not found.");
    if let Some(suggested) = suggested_tool(requested, available) {
        message.push_str(&format!(" Did you mean {suggested}?"));
    }
    if available.is_empty() {
        message.push_str(" No tools are available.");
        return message;
    }
    let listed = available[..available.len().min(MAX_LISTED_TOOLS)].join(", ");
    message.push_str(&format!(" Available tools: {listed}"));
    if available.len() > MAX_LISTED_TOOLS {
        message.push_str(&format!(
            ", and {} more",
            available.len() - MAX_LISTED_TOOLS
        ));
    }
    message.push('.');
    message
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOOLS: [&str; 4] = ["read", "bash", "web_search", "codemode"];

    #[test]
    fn the_message_lists_the_tools_in_order() {
        assert_eq!(
            unknown_tool_message("grep", &TOOLS),
            "Tool grep not found. Available tools: read, bash, web_search, codemode."
        );
        assert_eq!(
            unknown_tool_message("x", &[]),
            "Tool x not found. No tools are available."
        );
        let many: Vec<String> = (0..45).map(|index| format!("t{index}")).collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        let message = unknown_tool_message("x", &many);
        assert!(message.ends_with(", t38, t39, and 5 more."), "{message}");
        assert_eq!(message.matches(", ").count(), 40);
    }

    #[test]
    fn a_suggestion_needs_exactly_one_match() {
        for (requested, expected) in [
            ("Read", Some("read")),
            ("BASH", Some("bash")),
            ("web-search", Some("web_search")),
            ("web.search", Some("web_search")),
            ("functions.read", Some("read")),
            ("default_api:read", Some("read")),
            ("mcp__srv__read", Some("read")),
            ("functions.Web-Search", Some("web_search")),
            // No guess by similarity, and none without a match.
            ("reed", None),
            ("readfile", None),
            ("grep", None),
            ("functions.", None),
            ("", None),
        ] {
            assert_eq!(suggested_tool(requested, &TOOLS), expected, "{requested}");
        }
        // Two names equal in the comparable form: no suggestion.
        assert_eq!(suggested_tool("Read", &["read", "READ"]), None);
        // A whole-name match wins over the prefix rule.
        assert_eq!(suggested_tool("a.b", &["a_b", "b"]), Some("a_b"));
        assert_eq!(
            unknown_tool_message("functions.read", &TOOLS),
            "Tool functions.read not found. Did you mean read? Available tools: read, bash, web_search, codemode."
        );
    }
}
