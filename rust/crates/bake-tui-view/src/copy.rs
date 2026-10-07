//! The preview's English product text, in one place.

pub const TITLE: &str = "Bake · Rust preview";
pub const INTRO: &[&str] = &[
    "Try the native composer and sample-agent views. Models and tools are not connected yet.",
    "",
    "Press Tab to inspect a sample agent. Esc brings you back to your draft.",
];
pub const HEADER_LIST_HINT: &str = "↑↓ select · Enter inspect · Esc back";
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
/// The call a sample turn runs, and its note once the turn completes or Esc
/// stops it.
pub const SAMPLE_COMMAND: &str = "bun run build";
pub const SAMPLE_DONE: &str = "exit 0";
pub const INTERRUPTED_NOTE: &str = "interrupted";
pub const COMPACTING_PHASES: &[&str] = &["preparing", "summarizing", "saving"];
pub const PHASE_SECONDS: u64 = 4;
pub const ABOVE: &str = "above";
/// A result preview's count of the lines it leaves out: `⋯ 4 more lines`.
pub const MORE_LINES: &str = "more lines";
/// A diff's count of the lines between its hunks: `⋯ 40 unmodified lines`.
pub const UNMODIFIED: &str = "unmodified lines";
/// A call's line count when it has no summary of its own: `1 line`, `2 lines`.
pub const LINE: &str = "line";
pub const LINES: &str = "lines";
/// Code mode: the label `run_code` calls draw under, and the words of its
/// call count, `13 calls · 1 failed`, and of its folded calls.
pub const SCRIPT: &str = "Script";
pub const CALL: &str = "call";
pub const CALLS: &str = "calls";
pub const FAILED: &str = "failed";
pub const MORE_CALLS: &str = "more calls";
/// The transcript's hint row: its keys while following output, and the way
/// back while reading history.
pub const HINT_FOLLOWING: &[(&str, &str)] = &[("PgUp", "scroll"), ("Ctrl+↑", "prompts")];
pub const HINT_LATEST: &str = "↓ Latest · Ctrl+End";
pub const BELOW: &str = "below";
pub const NO_MODEL: &str = "Model connection is not available in this preview. Your draft is kept.";
pub const READ_ONLY: &str =
    "Read-only inspection: input does not reach any agent or your draft. Esc returns.";
pub const LIST_KEYS: &str = "↑↓ select · Enter inspect · Esc returns to the draft.";
pub const DRAFT_LIMIT: &str = "Draft limit reached; the rest of the input was not added.";
/// The agents: the standing row's label, the list's subtitle, and what
/// inspection says about input and the draft.
pub const AGENTS: &str = "Agents";
pub const AGENTS_SUMMARY: &str = "samples · none running";
pub const LIST_SUBTITLE: &str = "Fixed examples · nothing is running";
pub const INSPECT_READ_ONLY: &str = "Read only · typing never reaches an agent or your draft";
pub const INSPECT_PARENT: &str = "Kept unchanged while you inspect";
/// The inspection ledger's labels, and the sample agents' state.
pub const LEDGER_ID: &str = "Id";
pub const LEDGER_ROLE: &str = "Role";
pub const LEDGER_STATE: &str = "State";
pub const LEDGER_INPUT: &str = "Input";
pub const LEDGER_DRAFT: &str = "Draft";
pub const LEDGER_STATE_VALUE: &str = "Sample · not running";
pub const INSPECT_KEYS: &str = "Tab agents · Esc draft";
/// The status line's model field while no model is selected, and the dim
/// label before the context reading.
pub const NO_MODEL_FIELD: &str = "no model";
pub const CONTEXT: &str = "ctx";
/// The header's word for a sample turn that ran to its end, and for one Esc stopped.
pub const COMPLETED: &str = "Completed";
pub const INTERRUPTED: &str = "Interrupted";
