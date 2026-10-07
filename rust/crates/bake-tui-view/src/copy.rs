//! The preview's English product text, in one place.

pub const TITLE: &str = "Bake · Rust preview";
pub const INTRO: &[&str] = &[
    "Try the native composer and sample-agent views. Models and tools are not connected yet.",
    "",
    "Press Tab to inspect a sample agent. Esc brings you back to your draft.",
];
pub const HEADER_COMPOSER: &str = "Rust preview · model not connected";
pub const HEADER_LIST: &str = "Sample agents";
pub const HEADER_LIST_HINT: &str = "↑↓ select · Enter inspect · Esc back";
pub const HEADER_INSPECT_HINT: &str = "Esc back to draft";
pub const TAB_HINT: &str = "Tab sample agents";
/// The idle placeholder's parts. The preview has no commands or file
/// mentions to name, so it names its editing keys instead.
pub const PLACEHOLDER: &[&str] = &["Type a draft", "Alt+Enter newline", "Ctrl+Z undo"];
/// While a turn runs, Enter steers instead of sending.
pub const PLACEHOLDER_RUNNING: &[&str] = &["Enter steers the next step · Alt+↑ sends now"];
/// While history is compacted, Enter queues a prompt to run afterwards.
pub const PLACEHOLDER_COMPACTING: &[&str] = &["Compacting… Enter queues · Esc cancels"];
/// An inspection whose parent draft is empty.
pub const PLACEHOLDER_INSPECTING: &[&str] = &["Read-only · Esc returns to parent"];
/// Between a placeholder's parts.
pub const PLACEHOLDER_SEP: &str = " · ";
pub const HINT_SEND: &str = "Enter sends";
pub const HINT_INTERRUPT: &str = "Esc interrupts";
pub const DRAFT_KEPT: &str = "draft kept · Esc returns";
/// Phases a sample turn steps through, one every [`PHASE_SECONDS`].
pub const SAMPLE_PHASES: &[&str] = &["thinking", "writing", "running bash", "running subagent +2"];
/// The header's word while history is compacted, and the phases it steps through.
pub const COMPACTING: &str = "Compacting history";
pub const COMPACTING_PHASES: &[&str] = &["preparing", "summarizing", "saving"];
pub const PHASE_SECONDS: u64 = 4;
pub const ABOVE: &str = "above";
pub const BELOW: &str = "below";
pub const NO_MODEL: &str = "Model connection is not available in this preview. Your draft is kept.";
pub const READ_ONLY: &str =
    "Read-only inspection: input does not reach any agent or your draft. Esc returns.";
pub const LIST_KEYS: &str = "↑↓ select · Enter inspect · Esc returns to the draft.";
pub const DRAFT_LIMIT: &str = "Draft limit reached; the rest of the input was not added.";
pub const LIST_TITLE: &str = "Sample agents · fixed examples, nothing is running";
pub const INSPECT_PARENT: &str = "Parent: this preview's draft, unchanged while you inspect.";
pub const INSPECT_READ_ONLY: &str =
    "Read-only. Typing here never reaches an agent or the parent draft.";
pub const INSPECT_RETURN: &str = "Esc returns to the draft · Tab returns to the list.";
pub const STATE: &str = "static sample";
/// The status line's model field while no model is connected, and its hint.
pub const NO_MODEL_FIELD: &str = "no model";
pub const NO_MODEL_HINT: &str = "rust preview";
/// The preview names its quit key in the status line, having no other help.
pub const QUIT_KEY: &str = "Ctrl+C quits";
/// The header's word for a sample turn that ran to its end, and for one Esc stopped.
pub const COMPLETED: &str = "Completed";
pub const INTERRUPTED: &str = "Interrupted";
