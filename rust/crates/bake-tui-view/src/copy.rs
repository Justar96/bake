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
pub const PLACEHOLDER_APPROVAL: &[&str] = &["Approval pending"];
/// While history is compacted, Enter queues a prompt to run afterwards.
pub const PLACEHOLDER_COMPACTING: &[&str] = &["Compacting… Enter queues · Esc cancels"];
/// An inspection whose parent draft is empty.
pub const PLACEHOLDER_INSPECTING: &[&str] = &["Read-only · Esc returns to parent"];
/// Between a placeholder's parts.
pub const PLACEHOLDER_SEP: &str = " · ";
pub const HINT_SEND: &str = "Enter sends";
pub const HINT_INTERRUPT: &str = "Esc interrupts";
pub const HINT_APPROVAL: &str = "Esc cancels request";
pub const PLACEHOLDER_DIALOG: &[&str] = &["Answer the dialog above"];
pub const HINT_DIALOG: &str = "Esc cancels";
pub const PLACEHOLDER_PREPARING: &[&str] = &["Preparing…"];
pub const HINT_PREPARING: &str = "Preparing…";
/// The keys of a list dialog, with and without its save key, and their
/// short forms for a narrow sheet.
pub const DIALOG_LIST_KEYS: &str = "↑↓ choose · Enter select · Esc cancel";
pub const DIALOG_LIST_SAVE_KEYS: &str =
    "↑↓ choose · Enter select · Ctrl+S to set as default · Esc cancel";
pub const DIALOG_LIST_KEYS_SHORT: &str = "↑↓ · Enter · Esc";
pub const DIALOG_LIST_SAVE_KEYS_SHORT: &str = "↑↓ · Enter · ^S save · Esc";
/// The keys a sheet too narrow for the others still names.
pub const DIALOG_KEYS_TINY: &str = "↵ · Esc";
/// What an empty filter row says.
pub const DIALOG_FILTER: &str = "Type to filter";
pub const DIALOG_NO_MATCH: &str = "No matching choices";
/// What `/resume`'s empty search row says; its keys and state, `{order}`
/// one of the order names and `{named}` `on` or `off`; and what it lists
/// when nothing matches.
pub const SESSION_SEARCH_FILTER: &str = "Search · re:<regex> · \"phrase\"";
pub const SESSION_SEARCH_STATE: &str = "^S sort: {order} · ^N named: {named}";
pub const SESSION_SEARCH_RECENT: &str = "Recent";
pub const SESSION_SEARCH_RELEVANCE: &str = "Relevance";
pub const SESSION_SEARCH_NO_MATCH: &str = "No matching sessions";
pub const SESSION_SEARCH_NO_NAMED: &str = "No named sessions · ^N shows all";
/// Why a `re:` query matches nothing: none given, or `{reason}` the
/// pattern was refused.
pub const SESSION_SEARCH_EMPTY_REGEX: &str = "Empty regex: type a pattern after re:";
pub const SESSION_SEARCH_INVALID_REGEX: &str = "Invalid regex: {reason}";
/// The status of the item in effect now.
pub const DIALOG_CURRENT: &str = "Current";
/// After the count of rows a scrolled list hides above or below.
pub const DIALOG_MORE: &str = "more";
pub const DIALOG_FIELD_KEYS: &str = "Enter continue · Esc cancel";
pub const DIALOG_FIELD_KEYS_SHORT: &str = "Enter · Esc";
/// The keys of an information sheet, when it scrolls and when it fits.
pub const DIALOG_INFO_SCROLL_KEYS: &str = "↑↓ scroll · Enter or Esc close";
pub const DIALOG_INFO_KEYS: &str = "Enter or Esc close";
pub const DIALOG_WAITING: &str = "Working… · Esc cancels";
pub const DIALOG_PREPARING: &str = "Preparing… · Esc cancels";
pub const DRAFT_KEPT: &str = "draft kept · Esc returns";
/// Pi's branch-summary prompt after a `/tree` target is chosen
/// (`interactive-mode.ts` `showTreeSelector`): its title, its three
/// choices, and the custom-instructions editor's title, with the line
/// under it.
pub const BRANCH_SUMMARY_TITLE: &str = "Summarize branch?";
pub const BRANCH_SUMMARY_NONE: &str = "No summary";
pub const BRANCH_SUMMARY_SUMMARIZE: &str = "Summarize";
pub const BRANCH_SUMMARY_CUSTOM: &str = "Summarize with custom prompt";
pub const BRANCH_SUMMARY_INSTRUCTIONS: &str = "Custom summarization instructions";
pub const BRANCH_SUMMARY_INSTRUCTIONS_PROMPT: &str =
    "What the summary should focus on; Esc goes back to the choices";
/// The bar's phase while a branch is summarized, Pi's
/// `BranchSummaryStatusIndicator`; Esc cancels it.
pub const SUMMARIZING_BRANCH: &str = "summarizing branch";
/// The transcript row of a cancelled branch summary, Pi's status after
/// `abortBranchSummary`, before the tree reopens.
pub const BRANCH_SUMMARY_CANCELLED_ROW: &str = "Branch summarization cancelled";
/// The refusal of a prompt sent while `/tree` navigates.
pub const TREE_BEING_NAVIGATED: &str = "The session tree is being navigated; your draft is kept.";
/// A branch summary in the transcript, Pi's collapsed
/// `BranchSummaryMessageComponent` without its expand key.
pub const BRANCH_SUMMARY_ROW: &str = "[branch] Branch summary";
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
/// The titles of the cards whose tool names read poorly with only their
/// first letter raised; any other tool's card takes its name that way.
pub const TOOL_TITLES: &[(&str, &str)] = &[
    ("subagent", "Agent"),
    ("send_message", "Message"),
    ("interrupt_agent", "Interrupt"),
    ("list_agents", "Agents"),
    ("list_subagent_models", "Models"),
    ("job_output", "Job output"),
    ("job_list", "Jobs"),
    ("job_kill", "Stop job"),
    ("web_search", "Web search"),
    ("web_fetch", "Web fetch"),
];
/// Between the parts of a call's status, such as `exit 1 · earlier process`.
pub const STATUS_SEP: &str = " · ";
/// Call statuses: work handed to the background; an empty list; a message
/// handed to an agent; output waited for; a subagent that ran out of
/// tokens; a job whose end was not seen; a notice about a process before
/// this one; and a stop asked of a job that had already ended.
pub const BACKGROUND: &str = "background";
pub const NONE: &str = "none";
pub const DELIVERED: &str = "delivered";
pub const WAITING: &str = "waiting";
pub const TOKEN_LIMIT: &str = "token limit";
pub const JOB_LOST: &str = "outcome unknown";
pub const EARLIER_PROCESS: &str = "earlier process";
pub const ALREADY_FINISHED: &str = "already finished";
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
/// The file menu's words: each row's kind when the rows mix them, and its
/// states while discovery looks, finds nothing, or fails.
pub const FILE: &str = "File";
pub const DIRECTORY: &str = "Directory";
pub const FILES_LOADING: &str = "Finding paths…";
pub const NO_FILES: &str = "No matching paths";
pub const FILES_ERROR: &str = "File discovery failed";
/// A command submitted in the preview, which has no command service.
pub const NO_COMMANDS: &str = "Commands are not available in this preview";
/// The attachments panel's title, before the count, and its footer.
pub const ATTACHMENTS_TITLE: &str = "Staged attachments";
pub const ATTACHMENTS_HELP: &str = "Erase a placeholder to unstage its image";
/// The pending-input panel: its title, each item's target, and its footer,
/// as the TypeScript copy names them.
pub const PENDING: &str = "Pending input";
pub const NEXT_STEP: &str = "Next step";
pub const NEXT_TURN: &str = "Next turn";
pub const PENDING_HELP: &str = "Alt+↑ sends it now";
/// Alt+↑ sent the waiting prompts, or found none.
pub const PENDING_SENT: &str = "Sent queued input now";
pub const NO_PENDING: &str = "No queued input";
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
/// Enter with staged images while a live turn runs.
pub const IMAGES_WHILE_RUNNING: &str =
    "Wait for the turn to end before sending images. Your draft is kept.";
/// Ctrl+T or Ctrl+G in a live session: they open the preview's samples.
pub const PREVIEW_ONLY: &str = "Sample activity and sample agents are only in `bake-rs preview`.";
/// The runtime gave prompts back to the draft.
pub const RETURNED: &str = "Unsent prompts are back in your draft.";
/// `/tree` or `/fork` put the chosen message into the draft.
pub const RESTORED: &str = "The chosen message is in your draft; edit it and press Enter to send.";
/// The header's word for a live turn that ended with an error.
pub const FAILED_TURN: &str = "Failed";
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
/// Background shells: the standing row's key and count word, the list's
/// subtitle and keys, the shell view's keys, and each job's state words.
pub const JOBS_KEY: &str = "↓ manage";
pub const JOBS_RUNNING: &str = "shells running";
pub const JOBS_TITLE: &str = "Background shells";
pub const JOBS_SUBTITLE: &str = "They stop when Bake exits.";
pub const JOBS_LIST_KEYS: &str = "↑↓ select · Enter open · x stop · Esc back";
pub const JOBS_SHELL_KEYS: &str = "↑↓ PgUp/PgDn scroll · x stop · Esc list";
pub const JOB_RUNNING: &str = "running";
pub const JOB_STOPPING: &str = "stopping";
pub const JOB_EXIT: &str = "exit";
pub const JOB_DONE: &str = "done";
pub const JOB_STOPPED: &str = "stopped";
pub const JOB_FAILED: &str = "failed";
/// Before the size of output a shell no longer keeps.
pub const JOB_EARLIER: &str = "earlier";
pub const JOB_NO_OUTPUT: &str = "No output yet.";
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
/// The approval panel's words: its title, before the tool's name; the
/// heading over the call's arguments; its keys, as the TypeScript copy's
/// `approval` and `approve`, with `y` choosing and Enter confirming, and
/// the cancel key; the line once `y` has chosen; the scroll keys; and the
/// line shown until the request is armed and keys can answer it.
pub const APPROVAL_TITLE: &str = "Approval required";
pub const APPROVAL_ARGUMENTS: &str = "Arguments:";
pub const APPROVAL_KEYS: &str = "Y then Enter allows once · N rejects · Esc cancels";
pub const APPROVAL_CONFIRM: &str = "Allow this call once?";
pub const APPROVAL_CONFIRM_KEYS: &str = "Enter allows · N rejects · Esc cancels";
pub const APPROVAL_SCROLL: &str = "↑↓ PgUp/PgDn";
pub const APPROVAL_PREPARING: &str = "Preparing approval… · Esc cancels";
/// Why a key or paste did not answer the waiting approval.
pub const APPROVAL_KEYS_ONLY: &str =
    "To answer, press Y then Enter, or N, each on its own once the request shows; Esc cancels";
/// Why Enter did not allow the waiting approval.
pub const APPROVAL_CHOOSE_FIRST: &str = "Press Y first, then Enter, to allow this call once";

/// `/changelog`'s heading over the entries, Pi's `What's New`, and what it
/// says when the changelog has no released entries.
pub const CHANGELOG_TITLE: &str = "What's New";
pub const CHANGELOG_EMPTY: &str = "No changelog entries found.";

/// `/share`'s confirmation, which Pi's gist path does not have: its title,
/// what the gist holds and who can read it, and its two choices. Nothing is
/// uploaded unless the first is chosen.
pub const SHARE_CONFIRM_TITLE: &str = "Share this session as a secret GitHub gist?";
pub const SHARE_CONFIRM_NOTE: &str = "The gist holds this session as a web page: your messages, the model's answers, tool calls and their results (including file contents and command output read during the session), the system prompt, and the tool definitions. A secret gist is unlisted, but anyone with its link can read it.";
pub const SHARE_CONFIRM_YES: &str = "Yes, create the gist";
pub const SHARE_CONFIRM_NO: &str = "No";
/// Pi's `/share` notes and errors.
pub const SHARE_CANCELLED: &str = "Share cancelled";
pub const SHARE_GH_NOT_LOGGED_IN: &str = "GitHub CLI is not logged in. Run 'gh auth login' first.";
pub const SHARE_GH_MISSING: &str =
    "GitHub CLI (gh) is not installed. Install it from https://cli.github.com/";
pub const SHARE_EXPORT_FAILED: &str = "Failed to export session:";
pub const SHARE_GIST_FAILED: &str = "Failed to create gist:";
pub const SHARE_GIST_UNPARSED: &str = "Failed to parse gist ID from gh output";
pub const SHARE_GIST: &str = "Gist:";
pub const UNKNOWN_ERROR: &str = "Unknown error";

/// `/bug`'s first sheet: its title, what the report holds and where it
/// goes (`{repository}` names it), and the question its field answers.
/// Pi's report goes privately to its developers; Bake's is a public issue.
pub const BUG_TITLE: &str = "Report a bug";
pub const BUG_DISCLAIMER: &str = "This report becomes a public GitHub issue on {repository}. It includes your bake-rs version, operating system, the current model and provider configuration (without API keys), settings.json with secrets redacted, and provider error diagnostics from this session.";
pub const BUG_QUESTION: &str = "What went wrong? (optional)";
/// The transcript question, Pi's, with where a transcript goes in Bake.
pub const BUG_TRANSCRIPT_TITLE: &str = "Include the session transcript?";
pub const BUG_TRANSCRIPT_NOTE: &str = "The transcript contains your messages, model output, tool calls and their results, including file contents and command output read during this session. It goes only into the zip archive on this machine, never into the public issue.";
pub const BUG_TRANSCRIPT_YES: &str = "Yes, include the transcript";
pub const BUG_TRANSCRIPT_NO: &str = "No";
/// The last sheet before anything leaves the machine: its title, choices,
/// and the summary over them.
pub const BUG_DELIVERY_TITLE: &str = "Bug report";
pub const BUG_CREATE_ISSUE: &str = "Create GitHub issue";
pub const BUG_CREATE_ISSUE_WITHOUT_TRANSCRIPT: &str =
    "Create GitHub issue (without the transcript)";
pub const BUG_EXPORT_ZIP: &str = "Export as Zip";
pub const BUG_CANCEL: &str = "Cancel";
pub const BUG_ISSUE_LINK: &str = "Show a prefilled issue link";
/// Pi's summary lines; `{repository}` names the repository.
pub const BUG_DELIVERY_ISSUE_NOTE: &str = "Create GitHub issue files a public issue on {repository} with gh. Export writes a zip archive to the current directory instead.";
/// The fallback sheet after a failed issue, Pi's `Upload failed`.
pub const BUG_UPLOAD_FAILED_TITLE: &str = "Upload failed";
pub const BUG_UPLOAD_FAILED_QUESTION: &str = "Export the report as a zip archive instead?";
/// Pi's `/bug` notes and errors.
pub const BUG_CANCELLED: &str = "Bug report cancelled";
pub const BUG_BUILD_FAILED: &str = "Failed to build bug report:";
pub const BUG_WRITE_FAILED: &str = "Failed to write bug report:";
pub const BUG_ISSUE_CREATED: &str = "Bug report filed:";
pub const BUG_EXPORTED: &str = "Bug report exported to:";
pub const BUG_REPORT_ID: &str = "Report ID:";
pub const BUG_ISSUE_UNPARSED: &str = "Failed to read the issue URL from gh output";
pub const BUG_ISSUE_FAILED: &str = "Failed to create issue:";
pub const BUG_LINK: &str = "Open this link to file the report yourself:";
pub const BUG_NOT_RECORDED: &str = "The report was filed, but recording it in the session failed:";
