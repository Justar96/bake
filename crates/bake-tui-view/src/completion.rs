//! The slash-command menu: a sample catalog, the matches for the draft at the
//! caret, and the drafts a choice makes.
//!
//! The preview has no command service, so its catalog is a fixed sample of
//! commands, with their names, hints, and descriptions; none of them runs.
//! It samples only commands a live session also runs. A live session lists [`LIVE_CATALOG`], the
//! commands it runs, followed by its loaded skills (`/skill:<name>`) and
//! prompt templates (`/<name>`).

use std::borrow::Cow;
use std::collections::HashSet;

use crate::runtime::{DynamicCommand, DynamicCommandKind};

/// Longest dynamic command name kept (in Unicode scalar values).
pub const MAX_DYNAMIC_NAME: usize = 64;
/// Longest dynamic command argument hint kept (in Unicode scalar values).
pub const MAX_DYNAMIC_HINT: usize = 80;
/// Longest dynamic command description kept (in Unicode scalar values).
pub const MAX_DYNAMIC_DESCRIPTION: usize = 240;

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

/// The preview's sample of the commands a live session runs.
pub const CATALOG: &[Command] = &[
    command("compact", None, "Compact older conversation history"),
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

/// The commands a live `bake` session runs.
pub const LIVE_CATALOG: &[Command] = &[
    command("model", Some("[search]"), "Choose the model"),
    command("thinking", Some("[level]"), "Choose the thinking level"),
    command(
        "permission",
        Some("[preset]"),
        "Switch the sandbox and approval preset",
    ),
    command(
        "settings",
        None,
        "Change compaction, queue, /tree, and status line settings",
    ),
    command("login", None, "Sign in to CLIProxyAPI"),
    command("logout", None, "Sign out of CLIProxyAPI"),
    command("refresh-models", None, "Refresh the CLIProxyAPI model list"),
    command(
        "session",
        Some("[context]"),
        "Show this session's details or context",
    ),
    command("name", Some("[name]"), "Name this session"),
    command("new", None, "Start a new session"),
    command("resume", None, "Resume a saved session from this directory"),
    command("tree", None, "Navigate the session tree (switch branches)"),
    command("fork", None, "Fork a new session from an earlier message"),
    command(
        "clone",
        None,
        "Duplicate the current session at the current position",
    ),
    command(
        "compact",
        Some("[instructions]"),
        "Manually compact the session context",
    ),
    command("changelog", None, "Show changelog entries"),
    command("share", None, "Share session as a secret GitHub gist"),
    command(
        "bug",
        Some("[description]"),
        "Report a bug to the Bake developers",
    ),
    command(
        "telecom",
        Some("[msg <agent> <text> | broadcast <text>]"),
        "Show or message the Bake agents in this repository",
    ),
    command(
        "spawn",
        Some("<task>"),
        "Start a headless Bake agent on a task",
    ),
];

/// Commands listed ahead of the rest, in this order.
const FREQUENT: &[&str] = &["model", "resume", "new", "clear"];

/// Rows the menu lists before it counts the rest.
pub const MENU_ROWS: usize = 8;

/// One entry in the slash menu: a built-in command, a loaded skill
/// (`/skill:<name>`), or a prompt template (`/<name>`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: Cow<'static, str>,
    /// The argument placeholder: `<…>` marks required input, `[…]` optional.
    pub hint: Option<Cow<'static, str>>,
    pub description: Cow<'static, str>,
    /// Whether Enter fills in `/<name> ` for arguments instead of submitting
    /// immediately: true for built-in commands with a required `<…>` hint,
    /// for skills, and for templates with a required `<…>` hint.
    pub takes_arguments: bool,
}

impl Entry {
    /// Builds a menu entry from a static built-in [`Command`].
    pub fn from_command(command: &Command) -> Self {
        Self {
            name: Cow::Borrowed(command.name),
            hint: command.hint.map(Cow::Borrowed),
            description: Cow::Borrowed(command.description),
            takes_arguments: requires_input(command),
        }
    }

    /// Builds a sanitized menu entry from a session-defined
    /// [`DynamicCommand`], or `None` when its sanitized name is empty.
    pub fn from_dynamic(command: &DynamicCommand) -> Option<Self> {
        let raw_name = sanitize_name(&command.name, MAX_DYNAMIC_NAME);
        if raw_name.is_empty() {
            return None;
        }
        let name = match command.kind {
            DynamicCommandKind::Skill => format!("skill:{raw_name}"),
            DynamicCommandKind::Template => raw_name,
        };
        let hint = command
            .hint
            .as_deref()
            .map(|h| sanitize_inline(h, MAX_DYNAMIC_HINT))
            .filter(|h| !h.is_empty());
        let description = sanitize_inline(&command.description, MAX_DYNAMIC_DESCRIPTION);
        let takes_arguments = command.kind == DynamicCommandKind::Skill
            || hint
                .as_deref()
                .is_some_and(|h| h.trim_start().starts_with('<'));
        Some(Self {
            name: Cow::Owned(name),
            hint: hint.map(Cow::Owned),
            description: Cow::Owned(description),
            takes_arguments,
        })
    }
}

/// Strips control characters, whitespace, and `/` from an untrusted command
/// name and bounds its length.
pub fn sanitize_name(text: &str, max_chars: usize) -> String {
    text.chars()
        .filter(|c| !c.is_control() && !c.is_whitespace() && *c != '/')
        .take(max_chars)
        .collect()
}

/// Collapses untrusted text to one line with control characters removed,
/// runs of whitespace collapsed to one ASCII space, trimmed, and bounded.
pub fn sanitize_inline(text: &str, max_chars: usize) -> String {
    let mut out = String::new();
    let mut count = 0;
    for c in text.chars() {
        if c.is_whitespace() {
            if !out.is_empty() && !out.ends_with(' ') {
                if count >= max_chars {
                    break;
                }
                out.push(' ');
                count += 1;
            }
        } else if !c.is_control() {
            if count >= max_chars {
                break;
            }
            out.push(c);
            count += 1;
        }
    }
    let trimmed = out.trim_end().len();
    out.truncate(trimmed);
    out
}

fn rank(command: &Command) -> usize {
    FREQUENT
        .iter()
        .position(|&name| name == command.name)
        .unwrap_or(FREQUENT.len())
}

/// The leading slash token up to `caret`, when `draft[..caret]` is `/`
/// followed by non-whitespace, non-control, non-`/` characters.
fn slash_token(draft: &str, caret: usize) -> Option<&str> {
    let token = draft.get(..caret)?.strip_prefix('/')?;
    if !token
        .chars()
        .all(|c| !c.is_whitespace() && !c.is_control() && c != '/')
    {
        return None;
    }
    Some(token)
}

/// The commands that match a leading slash token, when the draft up to the
/// caret is a single slash token without `/`. Matching is by prefix,
/// case-insensitively; the frequent commands come first and the rest keep
/// the catalog's order. `None` outside the slash menu.
pub fn matches(draft: &str, caret: usize, catalog: &[Command]) -> Option<Vec<Command>> {
    let token = slash_token(draft, caret)?;
    let prefix = token.to_lowercase();
    let mut found: Vec<Command> = catalog
        .iter()
        .filter(|command| command.name.to_lowercase().starts_with(&prefix))
        .copied()
        .collect();
    found.sort_by_key(rank);
    Some(found)
}

/// Whether every character of `needle` appears in `haystack` in order.
fn is_subsequence(needle: &str, haystack: &str) -> bool {
    let mut chars = haystack.chars();
    needle.chars().all(|want| chars.any(|have| have == want))
}

/// Match tier for a dynamic entry against a lowercased slash `prefix`:
/// `0` exact, `1` prefix, `2` fuzzy subsequence, or `None` for no match.
///
/// Skills match both with `skill:` (`/sk`, `/skill:`, `/skill:rev`,
/// `/skill:rvw`) and by their bare name (`/rev`, `/rvw`, `/review`).
fn dynamic_match_tier(entry: &Entry, kind: DynamicCommandKind, prefix: &str) -> Option<u8> {
    let full = entry.name.to_lowercase();
    if prefix.is_empty() {
        return Some(1);
    }
    match kind {
        DynamicCommandKind::Skill => {
            let bare = full.strip_prefix("skill:").unwrap_or(&full);
            if let Some(after) = prefix.strip_prefix("skill:") {
                if !after.is_empty() && bare == after {
                    Some(0)
                } else if bare.starts_with(after) {
                    Some(1)
                } else if is_subsequence(after, bare) {
                    Some(2)
                } else {
                    None
                }
            } else if bare == prefix || full == prefix {
                Some(0)
            } else if full.starts_with(prefix) || bare.starts_with(prefix) {
                Some(1)
            } else if is_subsequence(prefix, bare) {
                Some(2)
            } else {
                None
            }
        }
        DynamicCommandKind::Template => {
            if full == prefix {
                Some(0)
            } else if full.starts_with(prefix) {
                Some(1)
            } else if is_subsequence(prefix, &full) {
                Some(2)
            } else {
                None
            }
        }
    }
}

/// The built-in commands and session-defined skills/templates that match a
/// leading slash token. Built-ins come first (frequent first, then catalog
/// order), followed by matching dynamic entries ranked exact, prefix, then
/// fuzzy subsequence while preserving their given order within each tier.
/// Skills match both `/skill:<name>` and their bare `<name>` (e.g. `/rev`,
/// `/rvw`, `/review`). Dynamic entries whose name collides with a built-in or
/// an earlier dynamic entry are skipped. `None` outside the slash menu.
pub fn matches_with(
    draft: &str,
    caret: usize,
    catalog: &[Command],
    dynamic: &[DynamicCommand],
) -> Option<Vec<Entry>> {
    let token = slash_token(draft, caret)?;
    let prefix = token.to_lowercase();
    let mut built_in: Vec<Command> = catalog
        .iter()
        .filter(|command| command.name.to_lowercase().starts_with(&prefix))
        .copied()
        .collect();
    built_in.sort_by_key(rank);
    let mut found: Vec<Entry> = built_in.iter().map(Entry::from_command).collect();
    let mut seen: HashSet<String> = catalog.iter().map(|c| c.name.to_lowercase()).collect();
    let mut dynamic_matches: Vec<(u8, Entry)> = Vec::new();
    for item in dynamic {
        let Some(entry) = Entry::from_dynamic(item) else {
            continue;
        };
        let lower = entry.name.to_lowercase();
        if !seen.insert(lower) {
            continue;
        }
        if let Some(tier) = dynamic_match_tier(&entry, item.kind, &prefix) {
            dynamic_matches.push((tier, entry));
        }
    }
    dynamic_matches.sort_by_key(|(tier, _)| *tier);
    found.extend(dynamic_matches.into_iter().map(|(_, entry)| entry));
    Some(found)
}

/// Whether a command cannot run without an argument: its hint opens with a
/// required placeholder.
pub fn requires_input(command: &Command) -> bool {
    command
        .hint
        .is_some_and(|hint| hint.trim_start().starts_with('<'))
}

fn complete_name(draft: &str, name: &str) -> (String, usize) {
    let end = draft.find(char::is_whitespace).unwrap_or(draft.len());
    let suffix = &draft[end..];
    let head = format!("/{name}");
    let (separator, space_len) = if suffix.starts_with(char::is_whitespace) {
        ("", suffix.chars().next().map_or(0, char::len_utf8))
    } else {
        (" ", 1)
    };
    let caret = head.len() + space_len;
    (format!("{head}{separator}{suffix}"), caret)
}

/// The draft a choice makes and the caret after it: the leading token
/// replaced by `/name`, then a space unless whitespace already follows, and
/// the caret after the space.
pub fn complete(draft: &str, command: &Command) -> (String, usize) {
    complete_name(draft, command.name)
}

/// [`complete`] for a slash-menu [`Entry`].
pub fn complete_entry(draft: &str, entry: &Entry) -> (String, usize) {
    complete_name(draft, entry.name.as_ref())
}

/// The command whose arguments are being typed once the menu has closed: the
/// one named before the first space or tab, when it has a hint.
pub fn usage(draft: &str, catalog: &[Command]) -> Option<Command> {
    let rest = draft.strip_prefix('/')?;
    let end = rest.find([' ', '\t'])?;
    let name = &rest[..end];
    catalog
        .iter()
        .find(|command| command.name == name && command.hint.is_some())
        .copied()
}

/// [`usage`] across both built-in commands and session-defined dynamic
/// commands.
pub fn usage_with(draft: &str, catalog: &[Command], dynamic: &[DynamicCommand]) -> Option<Entry> {
    let rest = draft.strip_prefix('/')?;
    let end = rest.find([' ', '\t'])?;
    let name = &rest[..end];
    if let Some(command) = catalog
        .iter()
        .find(|command| command.name == name && command.hint.is_some())
    {
        return Some(Entry::from_command(command));
    }
    if catalog.iter().any(|command| command.name == name) {
        return None;
    }
    dynamic
        .iter()
        .filter_map(Entry::from_dynamic)
        .find(|entry| entry.name == name && entry.hint.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matches_preview(draft: &str, caret: usize) -> Option<Vec<Command>> {
        matches(draft, caret, CATALOG)
    }

    fn usage_preview(draft: &str) -> Option<Command> {
        usage(draft, CATALOG)
    }

    #[test]
    fn a_live_session_lists_the_commands_it_runs() {
        let live: Vec<&str> = matches("/", 1, LIVE_CATALOG)
            .unwrap()
            .iter()
            .map(|c| c.name)
            .collect();
        assert_eq!(live[0], "model");
        assert_eq!(live.len(), LIVE_CATALOG.len());
        assert!(live.contains(&"refresh-models"));
        assert!(live.contains(&"resume") && live.contains(&"new"));
        assert!(live.contains(&"tree") && live.contains(&"fork"));
        assert!(live.contains(&"compact") && live.contains(&"clone"));
        // `/compact`'s instructions are optional, so Enter on it submits.
        let compact = LIVE_CATALOG.iter().find(|c| c.name == "compact").unwrap();
        assert!(!requires_input(compact));
        // `/session`'s `context` subcommand is optional, so Enter submits `/session`
        // and `/session ` shows the usage hint.
        let session = LIVE_CATALOG.iter().find(|c| c.name == "session").unwrap();
        assert!(!requires_input(session));
        assert_eq!(usage("/session ", LIVE_CATALOG), Some(*session));
    }

    fn names(found: Option<Vec<Command>>) -> Option<Vec<&'static str>> {
        found.map(|found| found.iter().map(|c| c.name).collect())
    }

    #[test]
    fn a_leading_slash_token_lists_its_prefix_matches_frequent_first() {
        let all = names(matches_preview("/", 1)).unwrap();
        assert_eq!(
            all,
            [
                "model", "resume", "new", "compact", "login", "logout", "settings", "thinking"
            ]
        );
        assert_eq!(all.len(), CATALOG.len());
        assert_eq!(
            names(matches_preview("/lo", 3)),
            Some(vec!["login", "logout"])
        );
        assert_eq!(
            names(matches_preview("/LO", 3)),
            Some(vec!["login", "logout"])
        );
        assert_eq!(names(matches_preview("/zz", 3)), Some(vec![]));
        // Only the draft up to the caret counts, and only as one token.
        assert_eq!(names(matches_preview("/model x", 6)), Some(vec!["model"]));
        assert_eq!(matches_preview("/model x", 8), None);
        assert_eq!(matches_preview("ask /model", 10), None);
        assert_eq!(matches_preview("", 0), None);
    }

    /// Each command the preview samples is one a live session runs.
    #[test]
    fn the_preview_lists_only_commands_a_live_session_runs() {
        for command in CATALOG {
            assert!(
                LIVE_CATALOG.iter().any(|live| live.name == command.name),
                "/{} is not run",
                command.name
            );
        }
    }

    #[test]
    fn a_choice_fills_in_its_name_and_a_space() {
        let thinking = CATALOG.iter().find(|c| c.name == "thinking").unwrap();
        assert_eq!(complete("/th", thinking), ("/thinking ".into(), 10));
        // What followed the token stays, with no second space.
        assert_eq!(
            complete("/th high", thinking),
            ("/thinking high".into(), 10)
        );
        assert!(!requires_input(thinking));
        // No catalog command takes a required argument now; a `<…>` hint
        // is one, and a leading space before it is ignored.
        let required = command("stage", Some(" <path>"), "Stage a file");
        assert!(requires_input(&required));
        assert_eq!(complete("/st", &required), ("/stage ".into(), 7));
        assert!(!requires_input(&command("bare", None, "Takes nothing")));
    }

    #[test]
    fn a_typed_command_with_a_hint_shows_its_usage() {
        assert_eq!(usage_preview("/model ").map(|c| c.name), Some("model"));
        assert_eq!(usage_preview("/model x/y").map(|c| c.name), Some("model"));
        // Without a separator the menu is still open; `new` takes nothing.
        assert_eq!(usage_preview("/model"), None);
        assert_eq!(usage_preview("/new "), None);
        assert_eq!(usage_preview("/nope "), None);
    }

    fn skill(name: &str, description: &str) -> DynamicCommand {
        DynamicCommand {
            name: name.into(),
            hint: None,
            description: description.into(),
            kind: DynamicCommandKind::Skill,
        }
    }

    fn template(name: &str, hint: Option<&str>, description: &str) -> DynamicCommand {
        DynamicCommand {
            name: name.into(),
            hint: hint.map(str::to_owned),
            description: description.into(),
            kind: DynamicCommandKind::Template,
        }
    }

    fn entry_names(found: Option<Vec<Entry>>) -> Option<Vec<String>> {
        found.map(|found| found.into_iter().map(|e| e.name.into_owned()).collect())
    }

    #[test]
    fn dynamic_skills_and_templates_list_after_builtins_and_fuzzy_match() {
        let dynamic = vec![
            skill("review", "Review code changes"),
            skill("release-notes", "Write release notes"),
            template("deploy", Some("<env>"), "Deploy to an environment"),
            // Collides with built-in `/model`: skipped.
            template("model", None, "Shadowed"),
        ];
        // Leading `/` keeps built-in order first, then skills and templates.
        let all = entry_names(matches_with("/", 1, LIVE_CATALOG, &dynamic)).unwrap();
        assert_eq!(all.len(), LIVE_CATALOG.len() + 3);
        assert_eq!(all[0], "model");
        assert_eq!(
            &all[LIVE_CATALOG.len()..],
            ["skill:review", "skill:release-notes", "deploy"]
        );

        // `/sk`, `/skill:`, `/skill:rev`, `/SKILL:REV` find skills.
        assert_eq!(
            entry_names(matches_with("/sk", 3, LIVE_CATALOG, &dynamic)),
            Some(vec!["skill:review".into(), "skill:release-notes".into()])
        );
        assert_eq!(
            entry_names(matches_with("/skill:", 7, LIVE_CATALOG, &dynamic)),
            Some(vec!["skill:review".into(), "skill:release-notes".into()])
        );
        assert_eq!(
            entry_names(matches_with("/skill:rev", 10, LIVE_CATALOG, &dynamic)),
            Some(vec!["skill:review".into()])
        );
        assert_eq!(
            entry_names(matches_with("/SKILL:REV", 10, LIVE_CATALOG, &dynamic)),
            Some(vec!["skill:review".into()])
        );

        // Bare skill name prefix and fuzzy subsequence matching:
        // `/rev`, `/rvw`, `/review` all list `/skill:review`, after any
        // matching built-in commands.
        assert_eq!(
            entry_names(matches_with("/rev", 4, LIVE_CATALOG, &dynamic)),
            Some(vec!["skill:review".into()])
        );
        assert_eq!(
            entry_names(matches_with("/rvw", 4, LIVE_CATALOG, &dynamic)),
            Some(vec!["skill:review".into()])
        );
        assert_eq!(
            entry_names(matches_with("/review", 7, LIVE_CATALOG, &dynamic)),
            Some(vec!["skill:review".into()])
        );
        // `/re` matches built-ins `resume` and `refresh-models` first, then
        // skills starting with `re` (`skill:review`, `skill:release-notes`).
        assert_eq!(
            entry_names(matches_with("/re", 3, LIVE_CATALOG, &dynamic)),
            Some(vec![
                "resume".into(),
                "refresh-models".into(),
                "skill:review".into(),
                "skill:release-notes".into(),
            ])
        );
        // `/rn` fuzzy-matches `release-notes`.
        assert_eq!(
            entry_names(matches_with("/rn", 3, LIVE_CATALOG, &dynamic)),
            Some(vec!["skill:release-notes".into()])
        );
        // `/sv` does not match `review` across the `skill:` prefix.
        assert_eq!(
            entry_names(matches_with("/sv", 3, LIVE_CATALOG, &dynamic)),
            Some(vec![])
        );

        // Mid-draft `/skill:` and filesystem paths do not open the menu.
        assert_eq!(
            matches_with("ask /skill:rev", 14, LIVE_CATALOG, &dynamic),
            None
        );
        assert_eq!(
            matches_with("/home/me/notes.txt", 18, LIVE_CATALOG, &dynamic),
            None
        );

        // Choosing a skill inserts `/skill:<name> ` with trailing space.
        let found = matches_with("/rvw", 4, LIVE_CATALOG, &dynamic).unwrap();
        assert!(found[0].takes_arguments);
        assert_eq!(
            complete_entry("/rvw", &found[0]),
            ("/skill:review ".into(), 14)
        );
        // Usage shows for templates with a hint.
        let deploy = usage_with("/deploy ", LIVE_CATALOG, &dynamic).expect("template usage");
        assert_eq!(deploy.name, "deploy");
        assert_eq!(deploy.hint.as_deref(), Some("<env>"));
    }

    #[test]
    fn untrusted_dynamic_commands_are_sanitized_and_bounded() {
        let long_name = "日本語".repeat(40);
        let long_desc = format!("line 1\r\n\x1b[31mred\x07\t{}", "x".repeat(400));
        let entry = Entry::from_dynamic(&DynamicCommand {
            name: format!("  {long_name}\x1b/bad\n"),
            hint: Some("<arg\x07\nname>".into()),
            description: long_desc,
            kind: DynamicCommandKind::Skill,
        })
        .expect("non-empty");
        assert!(
            !entry
                .name
                .chars()
                .any(|c| c.is_control() || c.is_whitespace() || c == '/')
        );
        assert_eq!(
            entry.name.strip_prefix("skill:").unwrap().chars().count(),
            MAX_DYNAMIC_NAME
        );
        assert!(!entry.description.chars().any(char::is_control));
        assert!(!entry.description.contains('\n'));
        assert!(entry.description.chars().count() <= MAX_DYNAMIC_DESCRIPTION);
        assert_eq!(entry.hint.as_deref(), Some("<arg name>"));
        // All-control/whitespace name is dropped.
        assert!(Entry::from_dynamic(&skill(" \x1b\n/ ", "empty")).is_none());
    }
}
