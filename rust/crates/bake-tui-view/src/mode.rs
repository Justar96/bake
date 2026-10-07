//! The composer's modes. The editor, wrapping, and window are the same in
//! every mode; only the prompt's tone, the empty draft's placeholder, and the
//! bottom edge's hint change. Hints ride the box's edge, so a mode change
//! never rewraps the draft or moves the caret.

use unicode_width::UnicodeWidthStr;

use crate::copy;

/// Narrower than this, the composer shows only a placeholder's first part and
/// no mode hint; the TypeScript layout's `HINT_MIN_COLUMNS`. A hint cut to fit
/// stops being help, and the key it names still works unnamed.
pub const HINT_MIN_COLUMNS: u16 = 60;

/// What the composer is for at the moment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Enter starts a turn.
    Idle,
    /// A turn runs; Enter steers its next step.
    Running,
    /// History is being compacted; Enter queues a prompt.
    Compacting,
    /// A child is inspected; the parent draft is kept and shown dim.
    Inspecting,
}

/// How the composer reads in one mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModeView {
    /// Whether the prompt and draft are drawn dim instead of in the accent.
    pub dim: bool,
    /// The empty draft's placeholder, in parts; see [`placeholder`].
    pub placeholder: &'static [&'static str],
    /// The bottom edge's label when no hidden-row count takes it.
    pub hint: Option<&'static str>,
}

/// The composer's tone, placeholder, and hint in `mode` on a terminal
/// `columns` wide; `drafting` is whether the draft has any text.
pub fn view(mode: Mode, drafting: bool, columns: u16) -> ModeView {
    let wide = columns >= HINT_MIN_COLUMNS;
    match mode {
        Mode::Idle => ModeView {
            dim: false,
            placeholder: copy::PLACEHOLDER,
            hint: (wide && drafting).then_some(copy::HINT_SEND),
        },
        Mode::Running => ModeView {
            dim: false,
            placeholder: copy::PLACEHOLDER_RUNNING,
            hint: wide.then_some(copy::HINT_INTERRUPT),
        },
        Mode::Compacting => ModeView {
            dim: false,
            placeholder: copy::PLACEHOLDER_COMPACTING,
            hint: None,
        },
        // Inspection always says where input goes and that the draft is safe.
        Mode::Inspecting => ModeView {
            dim: true,
            placeholder: copy::PLACEHOLDER_INSPECTING,
            hint: Some(copy::DRAFT_KEPT),
        },
    }
}

/// The placeholder's parts that fit in `room` cells, joined. The first part
/// always shows, clipped by the caller if it must be; later parts drop whole,
/// and all of them drop on a terminal narrower than [`HINT_MIN_COLUMNS`].
pub fn placeholder(parts: &[&str], columns: u16, room: usize) -> String {
    let mut text = parts.first().copied().unwrap_or_default().to_owned();
    if columns < HINT_MIN_COLUMNS {
        return text;
    }
    for part in parts.iter().skip(1) {
        if text.width() + copy::PLACEHOLDER_SEP.width() + part.width() > room {
            break;
        }
        text.push_str(copy::PLACEHOLDER_SEP);
        text.push_str(part);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_mode_reads_as_the_modes_table_says() {
        let cases = [
            (Mode::Idle, false, (false, "Type a draft", None)),
            (
                Mode::Idle,
                true,
                (false, "Type a draft", Some("Enter sends")),
            ),
            (
                Mode::Running,
                false,
                (
                    false,
                    "Enter steers the next step · Alt+↑ sends now",
                    Some("Esc interrupts"),
                ),
            ),
            (
                Mode::Compacting,
                true,
                (false, "Compacting… Enter queues · Esc cancels", None),
            ),
            (
                Mode::Inspecting,
                false,
                (
                    true,
                    "Read-only · Esc returns to parent",
                    Some("draft kept · Esc returns"),
                ),
            ),
        ];
        for (mode, drafting, (dim, first, hint)) in cases {
            let v = view(mode, drafting, 80);
            assert_eq!(
                (v.dim, v.placeholder[0], v.hint),
                (dim, first, hint),
                "{mode:?}"
            );
        }
    }

    #[test]
    fn narrow_terminals_drop_mode_hints_but_keep_the_inspection_label() {
        for mode in [Mode::Idle, Mode::Running, Mode::Compacting] {
            assert_eq!(
                view(mode, true, HINT_MIN_COLUMNS - 1).hint,
                None,
                "{mode:?}"
            );
        }
        assert_eq!(
            view(Mode::Running, false, HINT_MIN_COLUMNS).hint,
            Some("Esc interrupts")
        );
        assert_eq!(
            view(Mode::Inspecting, true, 12).hint,
            Some("draft kept · Esc returns")
        );
    }

    #[test]
    fn placeholder_parts_after_the_first_drop_whole() {
        let parts = copy::PLACEHOLDER;
        assert_eq!(
            placeholder(parts, 80, 74),
            "Type a draft · Alt+Enter newline · Ctrl+Z undo"
        );
        assert_eq!(
            placeholder(parts, 80, 40),
            "Type a draft · Alt+Enter newline"
        );
        assert_eq!(placeholder(parts, 80, 20), "Type a draft");
        assert_eq!(placeholder(parts, 80, 4), "Type a draft");
        // Below the hint width only the first part shows, room or not.
        assert_eq!(
            placeholder(parts, HINT_MIN_COLUMNS - 1, 200),
            "Type a draft"
        );
        assert_eq!(placeholder(&[], 80, 10), "");
    }
}
