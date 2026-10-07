//! The slash-command menu: a sample catalog, the matches for the draft at the
//! caret, and the drafts a choice makes.
//!
//! Ports the slash half of the TypeScript `completion.ts`. The preview has no
//! command service, so the catalog is a fixed sample of the runtime's own
//! commands, with their names, hints, and descriptions; none of them runs.

/// One command in the sample catalog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Command {
    pub name: &'static str,
    /// The argument placeholder: `<…>` marks required input, `[…]` optional.
    pub hint: Option<&'static str>,
    pub description: &'static str,
}

const fn command(
    name: &'static str,
    hint: Option<&'static str>,
    description: &'static str,
) -> Command {
    Command {
        name,
        hint,
        description,
    }
}

/// The runtime's commands as its command plugins describe them.
pub const CATALOG: &[Command] = &[
    command("agents", None, "List delegated agents"),
    command("attach", Some("<path>"), "Stage a file for the next prompt"),
    command("clear", None, "Clear the conversation"),
    command("compact", None, "Compact older conversation history"),
    command(
        "goal",
        Some("[objective|clear]"),
        "Set or view the goal for a long-running task",
    ),
    command("help", Some("[command]"), "List available commands"),
    command("login", Some("[target]"), "Sign in or set up CLIProxyAPI"),
    command(
        "logout",
        Some("[target]"),
        "Remove a stored key or the CLIProxyAPI route",
    ),
    command(
        "model",
        Some("[provider/model [effort]]"),
        "Choose model and reasoning effort",
    ),
    command("new", None, "New session"),
    command("resume", None, "Browse sessions or start a new one"),
    command("settings", None, "Change terminal and session settings"),
    command(
        "thinking",
        Some("[effort|default]"),
        "Change reasoning effort, also during a turn",
    ),
];

/// Commands listed ahead of the rest, in this order, as the oracle ranks them.
const FREQUENT: &[&str] = &["model", "resume", "new", "clear"];

/// Rows the menu lists before it counts the rest: the oracle's
/// `completionLimit` default.
pub const MENU_ROWS: usize = 8;

fn rank(command: &Command) -> usize {
    FREQUENT
        .iter()
        .position(|&name| name == command.name)
        .unwrap_or(FREQUENT.len())
}

/// The commands that match a leading slash token, when the draft up to the
/// caret is exactly one: `/` and letters, digits, `_`, or `-`. Matching is
/// by prefix, case-insensitively; the frequent commands come first and the
/// rest keep the catalog's order. `None` outside the slash menu.
pub fn matches(draft: &str, caret: usize) -> Option<Vec<Command>> {
    let token = draft[..caret].strip_prefix('/')?;
    if !token
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return None;
    }
    let prefix = token.to_ascii_lowercase();
    let mut found: Vec<Command> = CATALOG
        .iter()
        .filter(|command| command.name.starts_with(&prefix))
        .copied()
        .collect();
    found.sort_by_key(rank);
    Some(found)
}

/// Whether a command cannot run without an argument: its hint opens with a
/// required placeholder.
pub fn requires_input(command: &Command) -> bool {
    command
        .hint
        .is_some_and(|hint| hint.trim_start().starts_with('<'))
}

/// The draft a choice makes and the caret after it: the leading token
/// replaced by `/name`, then a space unless whitespace already follows, and
/// the caret after the space.
pub fn complete(draft: &str, command: &Command) -> (String, usize) {
    let end = draft.find(char::is_whitespace).unwrap_or(draft.len());
    let suffix = &draft[end..];
    let head = format!("/{}", command.name);
    let separator = if suffix.starts_with(char::is_whitespace) {
        ""
    } else {
        " "
    };
    let caret = head.len() + 1;
    (format!("{head}{separator}{suffix}"), caret)
}

/// The command whose arguments are being typed once the menu has closed: the
/// one named before the first space or tab, when it has a hint.
pub fn usage(draft: &str) -> Option<Command> {
    let rest = draft.strip_prefix('/')?;
    let end = rest.find([' ', '\t'])?;
    let name = &rest[..end];
    CATALOG
        .iter()
        .find(|command| command.name == name && command.hint.is_some())
        .copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(found: Option<Vec<Command>>) -> Option<Vec<&'static str>> {
        found.map(|found| found.iter().map(|c| c.name).collect())
    }

    #[test]
    fn a_leading_slash_token_lists_its_prefix_matches_frequent_first() {
        let all = names(matches("/", 1)).unwrap();
        assert_eq!(&all[..4], ["model", "resume", "new", "clear"]);
        assert_eq!(all.len(), CATALOG.len());
        assert_eq!(names(matches("/lo", 3)), Some(vec!["login", "logout"]));
        assert_eq!(names(matches("/LO", 3)), Some(vec!["login", "logout"]));
        assert_eq!(names(matches("/zz", 3)), Some(vec![]));
        // Only the draft up to the caret counts, and only as one token.
        assert_eq!(names(matches("/model x", 6)), Some(vec!["model"]));
        assert_eq!(matches("/model x", 8), None);
        assert_eq!(matches("ask /model", 10), None);
        assert_eq!(matches("", 0), None);
    }

    #[test]
    fn a_choice_fills_in_its_name_and_a_space() {
        let goal = CATALOG.iter().find(|c| c.name == "goal").unwrap();
        assert_eq!(complete("/go", goal), ("/goal ".into(), 6));
        // What followed the token stays, with no second space.
        assert_eq!(complete("/go ship it", goal), ("/goal ship it".into(), 6));
        assert!(requires_input(
            CATALOG.iter().find(|c| c.name == "attach").unwrap()
        ));
        assert!(!requires_input(goal));
    }

    #[test]
    fn a_typed_command_with_a_hint_shows_its_usage() {
        assert_eq!(usage("/goal ").map(|c| c.name), Some("goal"));
        assert_eq!(usage("/goal ship").map(|c| c.name), Some("goal"));
        // Without a separator the menu is still open; `new` takes nothing.
        assert_eq!(usage("/goal"), None);
        assert_eq!(usage("/new "), None);
        assert_eq!(usage("/nope "), None);
    }
}
