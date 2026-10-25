//! The terminal's own status indicators for a live session: the window title,
//! a progress indicator while a turn runs, and a desktop notification when
//! a turn ends or asks for approval while the terminal is out of focus.
//! Ghostty shows all three; the terminal loop writes them between frames,
//! and the terminal owner records what restoration must undo.
//!
//! The title is Pi v1.4.0's (`4a080252f06caf63a525d7873c5d94d3c23887e0`,
//! `coding-agent/src/modes/interactive/interactive-mode.ts`
//! `updateTerminalTitle`), `bake - <session name> - <directory>`, set with
//! OSC 0 as `tui/src/terminal.ts` `setTitle` sets it. Bake leads it with
//! `◐ ` while a turn works and `? ` while an approval waits, so a tab shows
//! what its session needs, and first saves the terminal's own title on the
//! xterm title stack (CSI 22;0 t), which restoration pops where the
//! terminal keeps one. Herdr's `terminal_title_stripped` drops a leading
//! `◐` it recognizes as activity, so its sidebar can show the bare title.
//!
//! Progress is Pi's `setProgress` (OSC 9;4): indeterminate while a turn
//! runs, sent again every second ([`KEEPALIVE`]) because Ghostty drops an
//! indicator after about 15 seconds without one. Bake adds the protocol's
//! paused state while a running turn waits for an approval, and its error
//! state for [`ERROR_HOLD`] after a turn fails. Pi's
//! `terminal.showTerminalProgress` turns it on or off; unset, only Ghostty
//! shows it, since a terminal that reads every OSC 9 as a notification
//! would announce `4;3`.
//!
//! Notifications are Bake's own: OSC 777 `notify`, which Ghostty shows as a
//! desktop notification, when a turn completes or fails and once for each
//! approval request, only after the terminal has reported (DECSET 1004)
//! that it lost focus. `terminal.notifications` turns them on or off;
//! unset, only Ghostty shows them. Inside Herdr, which notifies about its
//! panes itself, the terminal names itself `herdr`, so they stay off.
//!
//! Every text an indicator carries is one line without control characters
//! ([`one_line`]), so a session name read from a file cannot end a sequence
//! early or start one of its own.

use std::path::Path;
use std::time::Duration;

use bake_coding_agent::settings::Settings;
use bake_tui_view::runtime::RuntimeUpdate;
use bake_tui_view::state::Outcome;

use crate::lifecycle::{Activity, Lifecycle, one_line};

/// How often progress is sent again while it shows, Pi's keep-alive.
const KEEPALIVE: Duration = Duration::from_secs(1);
/// How long a failed turn's error indicator shows.
const ERROR_HOLD: Duration = Duration::from_secs(10);
/// The most characters a title keeps.
const TITLE: usize = 120;
/// The most characters a notification's title or body keeps.
const NOTE: usize = 120;

/// Saves the terminal's window and icon titles on its title stack.
pub(crate) const PUSH_TITLE: &str = "\x1b[22;0t";
/// Restores the titles [`PUSH_TITLE`] saved.
pub(crate) const POP_TITLE: &str = "\x1b[23;0t";
/// Removes the progress indicator.
pub(crate) const CLEAR_PROGRESS: &str = "\x1b]9;4;0\x07";

/// Which optional indicators the terminal gets. The title is always set.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Capabilities {
    pub(crate) progress: bool,
    pub(crate) notifications: bool,
}

impl Capabilities {
    /// The settings' choices, or where unset, whether the terminal is
    /// Ghostty.
    pub(crate) fn resolve(env: impl Fn(&str) -> Option<String>, settings: &Settings) -> Self {
        let ghostty = ghostty(&env);
        Self {
            progress: settings.show_terminal_progress.unwrap_or(ghostty),
            notifications: settings.terminal_notifications.unwrap_or(ghostty),
        }
    }
}

/// Whether the terminal is Ghostty: it names itself in `TERM_PROGRAM`, or,
/// where that is not passed on, as over SSH, its `TERM` is Ghostty's own.
/// A multiplexer such as tmux or Herdr names itself instead.
fn ghostty(env: &impl Fn(&str) -> Option<String>) -> bool {
    match env("TERM_PROGRAM").filter(|program| !program.is_empty()) {
        Some(program) => program == "ghostty",
        None => env("TERM").as_deref() == Some("xterm-ghostty"),
    }
}

/// The title's directory: the last component of `cwd`, or all of it at
/// the root.
pub(crate) fn directory(cwd: &Path) -> String {
    cwd.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| cwd.to_string_lossy().into_owned())
}

/// An OSC 9;4 state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Progress {
    #[default]
    Clear,
    Busy,
    Paused,
    Error,
}

impl Progress {
    fn sequence(self) -> &'static str {
        match self {
            Self::Clear => CLEAR_PROGRESS,
            Self::Busy => "\x1b]9;4;3\x07",
            Self::Paused => "\x1b]9;4;4;100\x07",
            Self::Error => "\x1b]9;4;2;100\x07",
        }
    }

    /// Whether it is sent again to keep showing. The error state goes
    /// away on its own sooner than Ghostty would drop it.
    fn kept_alive(self) -> bool {
        matches!(self, Self::Busy | Self::Paused)
    }
}

/// What one flush writes, and what restoration must then undo.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Output {
    pub(crate) sequence: String,
    /// The terminal's own title is now on its title stack.
    pub(crate) pushed_title: bool,
    /// Whether a progress indicator shows, when this flush changed that.
    pub(crate) progress: Option<bool>,
}

/// The indicators' state: what the runtime's updates say, and what the
/// terminal was last sent.
#[derive(Debug)]
pub(crate) struct Indicators {
    capabilities: Capabilities,
    /// The title's directory ([`directory`]).
    dir: String,
    /// The session's display name.
    name: Option<String>,
    lifecycle: Lifecycle,
    /// False only once the terminal has reported losing focus: a terminal
    /// that never reports is taken to be in front.
    focused: bool,
    /// The approval request announced last, by generation.
    announced: Option<u64>,
    /// Until when a failed turn's error indicator shows.
    failed_until: Option<Duration>,
    /// Notification bodies waiting for the next flush.
    notes: Vec<String>,
    title: Option<String>,
    pushed: bool,
    progress: Progress,
    progress_at: Duration,
}

impl Indicators {
    pub(crate) fn new(capabilities: Capabilities, dir: String) -> Self {
        Self {
            capabilities,
            dir,
            name: None,
            lifecycle: Lifecycle::default(),
            focused: true,
            announced: None,
            failed_until: None,
            notes: Vec::new(),
            title: None,
            pushed: false,
            progress: Progress::Clear,
            progress_at: Duration::ZERO,
        }
    }

    /// Whether the terminal should report focus changes: only
    /// notifications read them.
    pub(crate) fn wants_focus(&self) -> bool {
        self.capabilities.notifications
    }

    /// The terminal reported gaining or losing focus.
    pub(crate) fn focus(&mut self, focused: bool) {
        self.focused = focused;
    }

    /// Follows one runtime update, `now` on the loop's clock.
    pub(crate) fn observe(&mut self, update: &RuntimeUpdate, now: Duration) {
        match update {
            RuntimeUpdate::Status(status) => self.name.clone_from(&status.name),
            RuntimeUpdate::TurnStarted { .. } | RuntimeUpdate::Replaced(_) => {
                self.failed_until = None;
            }
            RuntimeUpdate::TurnEnded(outcome) => match outcome {
                Outcome::Completed => self.notify("Ready for your next prompt"),
                Outcome::Failed => {
                    self.failed_until = Some(now + ERROR_HOLD);
                    self.notify("The turn ended with an error");
                }
                // Esc stopped it, so someone is there.
                Outcome::Interrupted => {}
            },
            RuntimeUpdate::Approvals(queue) => {
                if let Some(front) = queue.first()
                    && self.announced != Some(front.generation)
                {
                    self.announced = Some(front.generation);
                    self.notify(&format!("{} needs approval", front.tool));
                }
            }
            _ => {}
        }
        self.lifecycle.observe(update);
    }

    fn notify(&mut self, body: &str) {
        if self.capabilities.notifications && !self.focused {
            self.notes.push(one_line(body, NOTE).replace(';', ","));
        }
    }

    fn title(&self) -> String {
        let marker = match self.lifecycle.activity() {
            Activity::Working => "◐ ",
            Activity::Blocked => "? ",
            Activity::Idle => "",
        };
        let title = match &self.name {
            Some(name) => format!("{marker}bake - {name} - {}", self.dir),
            None => format!("{marker}bake - {}", self.dir),
        };
        one_line(&title, TITLE)
    }

    /// A notification's title, which OSC 777 ends at its first `;`.
    fn note_title(&self) -> String {
        let title = format!("bake - {}", self.name.as_deref().unwrap_or(&self.dir));
        one_line(&title, NOTE).replace(';', ",")
    }

    fn wanted_progress(&self, now: Duration) -> Progress {
        if self.lifecycle.running() {
            match self.lifecycle.activity() {
                Activity::Blocked => Progress::Paused,
                _ => Progress::Busy,
            }
        } else if self.failed_until.is_some_and(|until| now < until) {
            Progress::Error
        } else {
            Progress::Clear
        }
    }

    /// What has changed since the last flush, and progress's keep-alive
    /// when due; `None` when nothing is to be written.
    pub(crate) fn flush(&mut self, now: Duration) -> Option<Output> {
        let mut sequence = String::new();
        let mut pushed_title = false;
        let title = self.title();
        if self.title.as_ref() != Some(&title) {
            if !self.pushed {
                sequence.push_str(PUSH_TITLE);
                self.pushed = true;
                pushed_title = true;
            }
            sequence.push_str(&format!("\x1b]0;{title}\x07"));
            self.title = Some(title);
        }
        let mut progress = None;
        if self.capabilities.progress {
            let wanted = self.wanted_progress(now);
            let due = wanted.kept_alive() && now >= self.progress_at + KEEPALIVE;
            if wanted != self.progress || due {
                sequence.push_str(wanted.sequence());
                if wanted != self.progress {
                    progress = Some(wanted != Progress::Clear);
                }
                self.progress = wanted;
                self.progress_at = now;
            }
        }
        if !self.notes.is_empty() {
            let title = self.note_title();
            for body in self.notes.drain(..) {
                sequence.push_str(&format!("\x1b]777;notify;{title};{body}\x07"));
            }
        }
        (!sequence.is_empty()).then_some(Output {
            sequence,
            pushed_title,
            progress,
        })
    }

    /// How long until a flush is due without an update: progress's
    /// keep-alive, or a failed turn's indicator going away.
    pub(crate) fn next_due(&self, now: Duration) -> Option<Duration> {
        let keep = self
            .progress
            .kept_alive()
            .then(|| (self.progress_at + KEEPALIVE).saturating_sub(now));
        let fail = (self.progress == Progress::Error)
            .then_some(self.failed_until)
            .flatten()
            .map(|until| until.saturating_sub(now));
        keep.into_iter().chain(fail).min()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bake_tui_view::runtime::{Approval, SessionStatus};

    const BUSY: &str = "\x1b]9;4;3\x07";
    const PAUSED: &str = "\x1b]9;4;4;100\x07";
    const ERROR: &str = "\x1b]9;4;2;100\x07";

    fn indicators(progress: bool, notifications: bool) -> Indicators {
        Indicators::new(
            Capabilities {
                progress,
                notifications,
            },
            "bake".into(),
        )
    }

    fn at(ms: u64) -> Duration {
        Duration::from_millis(ms)
    }

    fn turn() -> RuntimeUpdate {
        RuntimeUpdate::TurnStarted { seed: "s".into() }
    }

    fn named(name: &str) -> RuntimeUpdate {
        RuntimeUpdate::Status(SessionStatus {
            name: Some(name.into()),
            ..SessionStatus::default()
        })
    }

    fn approval(generation: u64, tool: &str) -> RuntimeUpdate {
        RuntimeUpdate::Approvals(vec![Approval {
            generation,
            tool: tool.into(),
            call_id: "1".into(),
            parent_call_id: None,
            reason: "why".into(),
            arguments: "what".into(),
        }])
    }

    fn written(indicators: &mut Indicators, now: Duration) -> String {
        indicators
            .flush(now)
            .map(|output| output.sequence)
            .unwrap_or_default()
    }

    // Pi v1.4.0 `tui/test/terminal.test.ts`: "writes a valid OSC 9;4
    // clear sequence".
    #[test]
    fn the_clear_sequence_is_a_complete_osc_9_4() {
        assert_eq!(CLEAR_PROGRESS, "\x1b]9;4;0\x07");
    }

    #[test]
    fn ghostty_gets_progress_and_notifications_unless_settings_say_otherwise() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| (*value).to_owned())
            }
        };
        let defaults = Settings::default();
        let on = Capabilities {
            progress: true,
            notifications: true,
        };
        for ghostty in [
            &[("TERM_PROGRAM", "ghostty"), ("TERM", "xterm-ghostty")][..],
            &[("TERM_PROGRAM", "ghostty"), ("TERM", "xterm-256color")],
            // Over SSH, only TERM comes along.
            &[("TERM_PROGRAM", ""), ("TERM", "xterm-ghostty")],
            &[("TERM", "xterm-ghostty")],
        ] {
            assert_eq!(Capabilities::resolve(env(ghostty), &defaults), on);
        }
        for other in [
            &[("TERM_PROGRAM", "herdr"), ("TERM", "xterm-256color")][..],
            // tmux names itself, whatever TERM it was given.
            &[("TERM_PROGRAM", "tmux"), ("TERM", "xterm-ghostty")],
            &[("TERM_PROGRAM", "iTerm.app")],
            &[],
        ] {
            assert_eq!(
                Capabilities::resolve(env(other), &defaults),
                Capabilities::default()
            );
        }
        let chosen = Settings {
            show_terminal_progress: Some(true),
            terminal_notifications: Some(false),
            ..Settings::default()
        };
        assert_eq!(
            Capabilities::resolve(env(&[("TERM_PROGRAM", "WezTerm")]), &chosen),
            Capabilities {
                progress: true,
                notifications: false
            }
        );
        let off = Settings {
            show_terminal_progress: Some(false),
            terminal_notifications: Some(false),
            ..Settings::default()
        };
        assert_eq!(
            Capabilities::resolve(env(&[("TERM_PROGRAM", "ghostty")]), &off),
            Capabilities::default()
        );
    }

    #[test]
    fn the_title_names_the_session_and_marks_what_it_needs() {
        let mut s = indicators(false, false);
        let first = s.flush(at(0)).unwrap();
        assert_eq!(
            first,
            Output {
                sequence: format!("{PUSH_TITLE}\x1b]0;bake - bake\x07"),
                pushed_title: true,
                progress: None,
            }
        );
        assert_eq!(s.flush(at(0)), None);
        s.observe(&named("fix auth"), at(0));
        assert_eq!(written(&mut s, at(0)), "\x1b]0;bake - fix auth - bake\x07");
        s.observe(&turn(), at(0));
        assert_eq!(
            written(&mut s, at(0)),
            "\x1b]0;◐ bake - fix auth - bake\x07"
        );
        s.observe(&approval(1, "bash"), at(0));
        assert_eq!(
            written(&mut s, at(0)),
            "\x1b]0;? bake - fix auth - bake\x07"
        );
        s.observe(&RuntimeUpdate::Approvals(vec![]), at(0));
        s.observe(&RuntimeUpdate::TurnEnded(Outcome::Completed), at(0));
        // Only the last state is written; the stack is pushed once.
        let output = s.flush(at(0)).unwrap();
        assert_eq!(output.sequence, "\x1b]0;bake - fix auth - bake\x07");
        assert!(!output.pushed_title);
        // A name from a session file cannot write sequences of its own.
        s.observe(&named("x\x07\x1b]0;evil\x1b\\\u{9c}y"), at(0));
        assert_eq!(
            written(&mut s, at(0)),
            "\x1b]0;bake - x ]0;evil \\ y - bake\x07"
        );
    }

    #[test]
    fn progress_follows_turns_and_is_kept_alive() {
        let mut s = indicators(true, false);
        s.flush(at(0));
        assert_eq!(s.next_due(at(0)), None);
        s.observe(&turn(), at(100));
        let started = s.flush(at(100)).unwrap();
        assert_eq!(started.sequence, format!("\x1b]0;◐ bake - bake\x07{BUSY}"));
        assert_eq!(started.progress, Some(true));
        assert_eq!(s.flush(at(600)), None);
        assert_eq!(s.next_due(at(600)), Some(at(500)));
        // Ghostty drops an indicator nobody refreshes.
        let alive = s.flush(at(1100)).unwrap();
        assert_eq!((alive.sequence.as_str(), alive.progress), (BUSY, None));
        s.observe(&approval(1, "bash"), at(1200));
        assert!(written(&mut s, at(1200)).ends_with(PAUSED));
        assert_eq!(written(&mut s, at(2200)), PAUSED);
        s.observe(&RuntimeUpdate::Approvals(vec![]), at(2300));
        assert!(written(&mut s, at(2300)).ends_with(BUSY));
        s.observe(&RuntimeUpdate::TurnEnded(Outcome::Completed), at(2400));
        let ended = s.flush(at(2400)).unwrap();
        assert!(ended.sequence.ends_with(CLEAR_PROGRESS));
        assert_eq!(ended.progress, Some(false));
        assert_eq!(s.next_due(at(2400)), None);
        assert_eq!(s.flush(at(9000)), None);
    }

    #[test]
    fn a_failed_turn_shows_an_error_for_a_while() {
        let mut s = indicators(true, false);
        s.observe(&turn(), at(0));
        s.flush(at(0));
        s.observe(&RuntimeUpdate::TurnEnded(Outcome::Failed), at(500));
        let failed = s.flush(at(500)).unwrap();
        assert!(failed.sequence.ends_with(ERROR));
        assert_eq!(failed.progress, Some(true));
        assert_eq!(s.next_due(at(500)), Some(ERROR_HOLD));
        assert_eq!(s.flush(at(5000)), None);
        let cleared = s.flush(at(500) + ERROR_HOLD).unwrap();
        assert_eq!(
            (cleared.sequence.as_str(), cleared.progress),
            (CLEAR_PROGRESS, Some(false))
        );
        // A new turn replaces the error at once.
        s.observe(&RuntimeUpdate::TurnEnded(Outcome::Failed), at(20_000));
        s.flush(at(20_000));
        s.observe(&turn(), at(20_100));
        assert!(written(&mut s, at(20_100)).ends_with(BUSY));
    }

    #[test]
    fn without_progress_only_the_title_is_written() {
        let mut s = indicators(false, false);
        s.observe(&turn(), at(0));
        assert_eq!(
            written(&mut s, at(0)),
            format!("{PUSH_TITLE}\x1b]0;◐ bake - bake\x07")
        );
        assert_eq!(s.flush(at(5000)), None);
        assert_eq!(s.next_due(at(5000)), None);
    }

    #[test]
    fn notifications_wait_for_the_terminal_to_lose_focus() {
        let mut s = indicators(false, true);
        assert!(s.wants_focus());
        s.flush(at(0));
        s.observe(&turn(), at(0));
        s.observe(&approval(1, "bash"), at(0));
        s.observe(&RuntimeUpdate::Approvals(vec![]), at(0));
        s.observe(&RuntimeUpdate::TurnEnded(Outcome::Completed), at(0));
        assert!(!written(&mut s, at(0)).contains("777"));
        s.focus(false);
        s.observe(&named("auth; part 2"), at(0));
        s.observe(&turn(), at(0));
        s.observe(&approval(2, "edit"), at(0));
        // The same request, still displayed, is announced once.
        s.observe(&approval(2, "edit"), at(0));
        s.observe(&RuntimeUpdate::Approvals(vec![]), at(0));
        s.observe(&RuntimeUpdate::TurnEnded(Outcome::Failed), at(0));
        s.observe(&turn(), at(0));
        s.observe(&RuntimeUpdate::TurnEnded(Outcome::Interrupted), at(0));
        s.observe(&turn(), at(0));
        s.observe(&RuntimeUpdate::TurnEnded(Outcome::Completed), at(0));
        let sequence = written(&mut s, at(0));
        let notes: Vec<&str> = sequence
            .split("\x1b]777;notify;")
            .skip(1)
            .map(|note| note.trim_end_matches('\x07'))
            .collect();
        assert_eq!(
            notes,
            [
                "bake - auth, part 2;edit needs approval",
                "bake - auth, part 2;The turn ended with an error",
                "bake - auth, part 2;Ready for your next prompt",
            ]
        );
        s.focus(true);
        s.observe(&turn(), at(0));
        s.observe(&RuntimeUpdate::TurnEnded(Outcome::Completed), at(0));
        assert!(!written(&mut s, at(0)).contains("777"));
    }

    #[test]
    fn notifications_that_are_off_never_queue() {
        let mut s = indicators(true, false);
        assert!(!s.wants_focus());
        s.focus(false);
        s.observe(&turn(), at(0));
        s.observe(&RuntimeUpdate::TurnEnded(Outcome::Completed), at(0));
        assert!(!written(&mut s, at(0)).contains("777"));
    }

    #[test]
    fn the_directory_is_the_last_component_or_the_root() {
        assert_eq!(directory(Path::new("/home/me/bake")), "bake");
        assert_eq!(directory(Path::new("/")), "/");
    }

    /// `sequence` without the framing the indicators write, which leaves the
    /// texts they carry.
    fn texts(sequence: &str) -> String {
        let mut rest = sequence.to_owned();
        for frame in [
            PUSH_TITLE,
            BUSY,
            PAUSED,
            ERROR,
            CLEAR_PROGRESS,
            "\x1b]0;",
            "\x1b]777;notify;",
            "\x07",
        ] {
            rest = rest.replace(frame, "\n");
        }
        rest
    }

    proptest::proptest! {
        #![proptest_config(proptest::test_runner::Config::with_cases(256))]
        #[test]
        fn carried_text_never_holds_a_control_character(
            name in proptest::prelude::any::<String>(),
            tool in proptest::prelude::any::<String>(),
        ) {
            let mut s = indicators(true, true);
            s.focus(false);
            s.observe(&named(&name), at(0));
            s.observe(&turn(), at(0));
            s.observe(&approval(1, &tool), at(0));
            let sequence = written(&mut s, at(0));
            let texts = texts(&sequence);
            proptest::prop_assert!(
                !texts.chars().any(|c| c != '\n' && c.is_control()),
                "{:?}", sequence
            );
            // A notification's title and body stay two fields.
            for note in sequence.split("\x1b]777;notify;").skip(1) {
                proptest::prop_assert_eq!(note.matches(';').count(), 1);
            }
        }
    }
}
