//! The preview's English product text, in one place.

pub const TITLE: &str = "Bake · Rust preview";
pub const INTRO: &[&str] = &[
    "Try the native composer and sample-agent views. Models and tools are not connected yet.",
    "",
    "Press Ctrl+G to inspect a sample agent. Esc brings you back to your draft.",
];
pub const HEADER_LIST_HINT: &str = "↑↓ select · Enter inspect · Esc back";
/// The idle placeholder's parts. The preview has no commands or file
/// mentions to name, so it names its editing keys instead.
pub const PLACEHOLDER: &[&str] = &["Type a draft", "Alt+Enter newline", "Ctrl+- undo"];
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
/// The build a sample turn's script runs last, and its note once the turn
/// completes or Esc stops it.
pub const SAMPLE_COMMAND: &str = "bun run build";
/// What the script a sample turn runs is for.
pub const SAMPLE_SCRIPT: &str = "Read every manifest, then build";
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
/// Code mode: the label `run_code` calls draw under; the words of its call
/// count, `13 calls · 1 failed` or `13 calls · 4 running`, and of a call
/// site's tally, `11 done · 1 failed`; and the count of failures a site
/// leaves unlisted.
pub const SCRIPT: &str = "Codemode";
pub const CALL: &str = "call";
pub const CALLS: &str = "calls";
pub const DONE: &str = "done";
pub const FAILED: &str = "failed";
pub const RUNNING: &str = "running";
pub const MORE_FAILED: &str = "more failed";
/// What a program hands back to the model, named as the program wrote it:
/// the lines it logged, the value it returned, or how it failed.
pub const CONSOLE: &str = "console";
pub const RETURN: &str = "return";
pub const ERROR: &str = "error";
/// The transcript's hint row: its keys while following output, and the way
/// back while reading history.
pub const HINT_FOLLOWING: &[(&str, &str)] = &[("Wheel/PgUp", "scroll"), ("Ctrl+↑", "prompts")];
/// While reading history, how much lies below: `12 lines below`.
pub const LINES_BELOW: &str = "lines below";
/// The slash menu's words: its key on the box's edge, the count of matches
/// it leaves out, and its empty state.
pub const TAB_COMPLETES: &str = "Tab completes";
pub const MORE_MATCHES: &str = "more, keep typing to narrow";
pub const NO_COMPLETIONS: &str = "No matching commands";
/// A command submitted in the preview, which has no command service.
pub const NO_COMMANDS: &str = "Commands are not available in this preview";
/// The attachments panel's title, before the count, and its footer.
pub const ATTACHMENTS_TITLE: &str = "Staged attachments";
pub const ATTACHMENTS_HELP: &str = "Erase a placeholder to unstage its image";
/// Ctrl+V, or an empty paste, found no image on the clipboard.
pub const NO_CLIPBOARD_IMAGE: &str = "No image on the clipboard";
/// Said in the scroll indicator's row once a selection reaches the clipboard,
/// or does not.
pub const COPIED: &str = "Copied";
pub const COPY_FAILED: &str = "Copy failed";
/// The reading indicator's words: what arrived below, and the key back.
pub const NEW_OUTPUT: &str = "New output";
pub const LATEST_KEY: &str = "Ctrl+End";
pub const BELOW: &str = "below";
/// Enter while the Ctrl+T sample runs: it stands in for a turn and takes no prompt.
pub const SAMPLE_ONLY: &str =
    "The sample activity takes no prompt. Esc stops it; your draft is kept.";
/// Enter with staged images: the fixture runtime takes text alone.
pub const NO_IMAGES: &str = "Images are not sent in this preview. Your draft is kept.";
/// The phase a runtime turn shows until the runtime names one.
pub const FIRST_PHASE: &str = "thinking";
pub const READ_ONLY: &str =
    "Read-only inspection: input does not reach any agent or your draft. Esc returns.";
pub const DRAFT_LIMIT: &str = "Draft limit reached; the rest of the input was not added.";
/// The agents: the standing row's label, the list's subtitle, and what
/// inspection says about input and the draft.
pub const AGENTS: &str = "Agents";
/// The key that opens the agent list, at the right of the agents row.
pub const AGENTS_KEY: &str = "Ctrl+G";
/// Shown while a first Ctrl+C is armed.
pub const QUIT: &str = "Press Ctrl-C again to quit";
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
