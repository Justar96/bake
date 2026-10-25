//! The session's lifecycle as status integrations report it: `working`
//! while a turn runs, `blocked` while an approval request waits for a
//! person, and `idle` otherwise, with one line saying what it waits for or
//! does. Herdr ([`crate::herdr`]) and the terminal's own indicators
//! ([`crate::indicators`]) follow the runtime's updates through it, so they
//! agree with each other and with the view.
//!
//! Dialogs do not block: every one answers a command the person ran, so
//! they are already looking at it, and Herdr's guide reserves `blocked` for
//! the agent needing a decision, as its Pi integration reports it.
//!
//! The line carries only what Bake itself names: the bar's phase and the
//! tool an approval is for. Prompts, answers, and tool arguments never
//! reach it.

use bake_tui_view::runtime::RuntimeUpdate;

/// The most characters a line from [`Lifecycle::detail`] or
/// [`one_line`] keeps.
pub(crate) const LINE: usize = 80;

/// What the session is doing, as status integrations name it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Activity {
    Idle,
    Working,
    Blocked,
}

impl Activity {
    /// Herdr's state name.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Working => "working",
            Self::Blocked => "blocked",
        }
    }
}

/// The lifecycle the runtime's updates describe.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Lifecycle {
    running: bool,
    /// The tool of the approval request the view displays.
    approval: Option<String>,
    /// What the running turn is doing, as the bar says it.
    phase: Option<String>,
}

impl Lifecycle {
    /// Applies `update`. Returns whether it is one the lifecycle follows;
    /// others change nothing.
    pub(crate) fn observe(&mut self, update: &RuntimeUpdate) -> bool {
        match update {
            RuntimeUpdate::TurnStarted { .. } => {
                self.running = true;
                self.phase = None;
            }
            RuntimeUpdate::Phase(phase) => self.phase = Some(phase.clone()),
            RuntimeUpdate::TurnEnded(_) => {
                self.running = false;
                self.phase = None;
            }
            RuntimeUpdate::Approvals(queue) => {
                self.approval = queue.first().map(|approval| approval.tool.clone());
            }
            RuntimeUpdate::Replaced(_) => *self = Self::default(),
            _ => return false,
        }
        true
    }

    /// Whether a turn, or a command shown as one, is running.
    pub(crate) fn running(&self) -> bool {
        self.running
    }

    /// A decision waits before work resumes: a displayed approval request
    /// blocks even a running turn.
    pub(crate) fn activity(&self) -> Activity {
        if self.approval.is_some() {
            Activity::Blocked
        } else if self.running {
            Activity::Working
        } else {
            Activity::Idle
        }
    }

    /// One line on what a blocked session waits for or what a working one
    /// does; `None` when idle or when a turn has not said yet.
    pub(crate) fn detail(&self) -> Option<String> {
        let line = match self.activity() {
            Activity::Blocked => format!("{} needs approval", self.approval.as_deref()?),
            Activity::Working => self.phase.clone()?,
            Activity::Idle => return None,
        };
        Some(one_line(&line, LINE)).filter(|line| !line.is_empty())
    }
}

/// `text` as one line of at most `max` characters: every control
/// character, line breaks and escapes included, and every other space
/// becomes an ASCII space, runs of spaces become one, and the ends are
/// trimmed. What reaches a terminal or another program's arguments can then
/// carry no sequence of its own.
pub(crate) fn one_line(text: &str, max: usize) -> String {
    let mut line = String::new();
    for c in text.chars() {
        let c = if c.is_control() || c.is_whitespace() {
            ' '
        } else {
            c
        };
        if c == ' ' && (line.is_empty() || line.ends_with(' ')) {
            continue;
        }
        line.push(c);
    }
    let mut line: String = line.trim_end().chars().take(max).collect();
    line.truncate(line.trim_end().len());
    line
}

#[cfg(test)]
mod tests {
    use super::*;
    use bake_tui_view::dialog::{DialogKind, DialogRequest, InfoDialog};
    use bake_tui_view::runtime::Approval;
    use bake_tui_view::state::Outcome;

    fn approval(tool: &str) -> Approval {
        Approval {
            generation: 1,
            tool: tool.into(),
            call_id: "1".into(),
            parent_call_id: None,
            reason: "secret reason".into(),
            arguments: "rm -rf secret".into(),
        }
    }

    fn dialog(title: &str) -> RuntimeUpdate {
        RuntimeUpdate::Dialog(Some(DialogRequest {
            id: 1,
            kind: DialogKind::Info(InfoDialog {
                title: title.into(),
                lines: vec![],
            }),
        }))
    }

    #[test]
    fn approvals_block_before_work_and_name_only_the_tool() {
        let mut lifecycle = Lifecycle::default();
        assert_eq!(lifecycle.activity(), Activity::Idle);
        assert!(lifecycle.observe(&RuntimeUpdate::TurnStarted { seed: "s".into() }));
        assert_eq!(lifecycle.activity(), Activity::Working);
        assert_eq!(lifecycle.detail(), None);
        lifecycle.observe(&RuntimeUpdate::Phase("running bash".into()));
        assert_eq!(lifecycle.detail().as_deref(), Some("running bash"));
        lifecycle.observe(&RuntimeUpdate::Approvals(vec![approval("bash")]));
        assert_eq!(lifecycle.activity(), Activity::Blocked);
        // The tool alone: no reason or arguments.
        assert_eq!(lifecycle.detail().as_deref(), Some("bash needs approval"));
        lifecycle.observe(&RuntimeUpdate::Approvals(vec![]));
        assert_eq!(lifecycle.detail().as_deref(), Some("running bash"));
        lifecycle.observe(&RuntimeUpdate::TurnEnded(Outcome::Completed));
        assert_eq!(lifecycle.activity(), Activity::Idle);
        assert_eq!(lifecycle.detail(), None);
        assert!(!lifecycle.observe(&RuntimeUpdate::Live(vec![])));
    }

    #[test]
    fn a_dialog_the_person_opened_changes_nothing() {
        let mut lifecycle = Lifecycle::default();
        assert!(!lifecycle.observe(&dialog("Model")));
        assert_eq!(lifecycle.activity(), Activity::Idle);
        lifecycle.observe(&RuntimeUpdate::TurnStarted { seed: "s".into() });
        let working = lifecycle.clone();
        assert!(!lifecycle.observe(&dialog("Sign in")));
        assert!(!lifecycle.observe(&RuntimeUpdate::Dialog(None)));
        assert_eq!(lifecycle, working);
    }

    #[test]
    fn a_replaced_session_starts_idle() {
        let mut lifecycle = Lifecycle::default();
        lifecycle.observe(&RuntimeUpdate::TurnStarted { seed: "s".into() });
        lifecycle.observe(&RuntimeUpdate::Approvals(vec![approval("edit")]));
        lifecycle.observe(&RuntimeUpdate::Replaced(vec![]));
        assert_eq!(lifecycle, Lifecycle::default());
    }

    #[test]
    fn lines_lose_control_characters_and_keep_their_bound() {
        assert_eq!(one_line("  a\x1b]0;x\x07b\r\n\tc  ", 80), "a ]0;x b c");
        assert_eq!(one_line("\u{9b}31m", 80), "31m");
        assert_eq!(one_line("abcdef", 3), "abc");
        assert_eq!(one_line("ab  cd", 3), "ab");
        assert_eq!(one_line("\n\n", 3), "");
        assert_eq!(one_line("\u{2000}a\u{a0}\u{3000}b\u{2029}", 80), "a b");
    }

    proptest::proptest! {
        #![proptest_config(proptest::test_runner::Config::with_cases(256))]
        #[test]
        fn a_line_is_bounded_and_carries_no_control_character(
            text in proptest::prelude::any::<String>(),
            max in 0usize..100,
        ) {
            let line = one_line(&text, max);
            proptest::prop_assert!(line.chars().count() <= max);
            proptest::prop_assert!(!line.chars().any(char::is_control));
            proptest::prop_assert_eq!(line.trim(), line.as_str());
            proptest::prop_assert!(!line.contains("  "));
        }
    }
}
