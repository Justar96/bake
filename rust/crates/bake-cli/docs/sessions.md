# Sessions

bake-rs saves each conversation as a session file in Pi's session format, so a later run can continue it. A session records its messages, its model and thinking level, compactions, and its permission.

## Where sessions live

Sessions live in `sessions/` in the Bake home, in one directory per working directory: `/home/me/project` becomes `--home-me-project--`. Each file is named after its start time and id, such as `2026-01-02T03-04-05-678Z_<id>.jsonl`. `sessionDir` in `settings.json` names another directory to use instead.

## Choosing a session

- With no session flag, a run starts a new session.
- `-c`, `--continue` continues the most recently changed session for the working directory, or starts one when there is none.
- `--session <path|id>` opens a session. An argument that contains `/` or `\` or ends in `.jsonl` is a path, relative to the working directory; a path to a file that does not exist starts a new session saved there. Anything else is a session id or the start of one, looked up among the working directory's sessions. An id that belongs to another project's session is an error naming `--fork`, and one that matches nothing prints `No session found matching '<id>'`.
- `--session-id <id>` opens the project session with that exact id, or warns and starts a new session with that id. It cannot be combined with `--session` or `--continue`.
- `--fork <path|id>` copies a session file, including one from another project, into a new session in this working directory whose header records the source as its parent. Combine it with `--session-id` to choose the new session's id; it cannot be combined with `--session`, `--continue`, or `--no-session`.
- `--session-dir <dir>` stores and looks up sessions in `<dir>` instead of the Bake home's `sessions/` directory.
- `-n`, `--name <name>` records a display name entry in the session.
- `--no-session` keeps the session in memory and saves nothing.

A resumed session keeps its model, thinking level, and permission unless a flag changes them.

## Recovering a selected run

`--recoverable` journals one print-mode prompt and reports its run id and session file before execution. For example, `bake-rs -p --recoverable --max-model-calls 20 "Fix the failing test"` gives the run one allowance shared by its model requests, retries, and summaries. Recovery is opt-in and requires a saved session.

After an interrupted process, use `bake-rs -p --session /path/to/session.jsonl --resume-run <run-id>`. Select the existing file explicitly; the command takes no new prompt, piped input, file attachment, or replacement budget. It keeps already-spent model calls, recorded tool results, and the selected branch. A completed run returns its recorded outcome without calling the model again. Current tools and permission policy apply; pending approvals are not restored.

A dispatched tool without a recorded result needs reconciliation, so Bake refuses continuation rather than guessing whether its effect happened. Inspect the affected files or external service before starting new work. An interrupted run with `--run-timeout-ms` also refuses continuation because this implementation cannot prove monotonic clock continuity across processes. A recorded final answer or terminal outcome can still be read.

Text mode writes the run id and session path to standard error and prints the selected run's answer, even when the session has later conversation. JSON mode emits `recoverable_run_started` with `runId` before execution and `recoverable_run_result` with `runId`, `outcome`, and `finalMessage` when it returns. `finalMessage` is null when the selected run has no assistant response. `finished_turn` describes execution; it does not independently verify that the task succeeded. These flags do not recover ordinary runs that were started without `--recoverable`.

## In an interactive session

- `/session` shows the session's name, file, id, and message and token counts. It changes nothing.
- `/name <name>` records a display name, with line breaks turned into spaces; `/name` alone asks for a name, starting from the current one. A name that would be blank is refused.
- `/new` starts a new session in a new file, keeping the current model and thinking level.
- `/resume` lists the working directory's saved sessions, newest first, by name or first message, with the running one marked `Current`. Typing searches them (see [Searching sessions](#searching-sessions)). Enter opens the chosen session with its own transcript, model, thinking level, and permission; Esc keeps the running session.
- `/tree` shows the session's entries as a tree, the branch you are on first and marked with `•`, the current point marked `Current`. Settings entries and replies that only call tools are hidden; typing filters the rows. Enter on an earlier entry asks "Summarize branch?": No summary, Summarize, or Summarize with custom prompt, which asks for the summary's focus; Esc goes back a step. The session then continues from that entry in the same file: on a user message, it goes back to just before it and puts its text in your draft to edit and send; on any other entry, it continues after it. Without a summary nothing is sent, and the navigation itself writes nothing: the next entry written, such as your next prompt, starts a new branch there, and the branch you left stays in the file. With one, the model summarizes the entries of the branch you leave, back to where the two branches meet, while the bar shows `summarizing branch`; Esc cancels it, writes nothing, and reopens the tree at your choice. The summary is written where the session continues, replays as `[branch] Branch summary`, and is what the model sees of the branch you left. `branchSummary.reserveTokens` in `settings.json` sets the room kept for the summary request (16384 tokens), and `branchSummary.skipPrompt: true` navigates without asking. The model, thinking level, and permission stay as they are.
- `/fork` lists the session's user messages, the latest selected. Enter starts a new session in a new file, in the same session directory, holding the conversation up to just before the chosen message, and puts that message in your draft. The new file names the running session's file as its parent, and the running session's file is left unchanged. A fork that holds earlier messages uses the model, thinking level, and permission they were recorded with.
- `/clone` starts a new session in a new file, in the same session directory, holding the whole conversation up to the current point, and leaves your draft empty. Like `/fork`, the new file names the running session's file as its parent, the running session's file is left unchanged, and the clone uses the model, thinking level, and permission its history records. A saved session that has not written its file yet, before its first message, cannot be cloned.

A turn that is running refuses `/new`, `/resume`, `/tree`, `/fork`, `/clone`, and `/compact`, and a failed or cancelled command keeps the running session where it was; the draft is kept in each case. The new session is set up before the running one is closed. `/resume` shows only sessions recorded for this working directory, has no threaded order, path display, rename, or delete, and cannot go to a session in another directory. With `--no-session`, `/new`, `/fork`, and `/clone` start unsaved sessions and `/resume` is refused. A fork cancelled after it was set up may leave its new file behind. `/tree` does not offer Pi's label editing, filter modes, or folding, a branch summary cannot be expanded in the transcript, and the custom summary prompt is one line.

### Compacting

`/compact` summarizes the conversation before the most recent messages, so later prompts send the summary instead of the older history. Text after it, as in `/compact keep the API decisions`, is passed to the summary request as extra instructions. While the summary is written, the bar shows `compacting` and Esc cancels it; prompts and commands you send meanwhile are refused and stay in your draft. When it finishes, the transcript starts over with the messages it kept and `Compacted from N tokens` below them, and the session file gains one compaction entry; earlier entries stay in the file. It works whether or not automatic compaction (`compaction.enabled`) is on, and `compaction.keepRecentTokens` sets how much recent history is kept.

`/compact` runs only while the session is idle: a running turn, waiting prompts, or another compaction refuse it with the draft kept. Pi instead stops a running turn first; in bake-rs, Esc stops the turn, then `/compact` compacts. A session that is too small, or that ends with a compaction already, is refused, as is a session with no model. A cancel, or a summary request that fails, writes nothing, puts `/compact` back in your draft, and leaves the conversation as it was. An Esc that arrives after the compaction was saved is reported with the result. If the compaction cannot be written to the session file, the session stops, as it does after any failed write: nothing more is written, and later prompts are refused. The summary prompts and the compaction entry follow Pi's.

### Searching sessions

`/resume` searches each session's id, name, working directory, and the text of every message you or the model wrote in it, including on branches you left. Tool output and thinking are not searched.

| Type | Matches |
|---|---|
| `word other` | Sessions holding every word's letters in order, ignoring case |
| `"a phrase"` | Sessions holding those words together, whatever whitespace separates them |
| `re:pattern` | Sessions matching the regular expression, ignoring case |

Ctrl+S switches between newest first and best match first, and Ctrl+N shows only named sessions; the line under the title shows both. Regular expressions do not support look-around or backreferences, and a pattern may be at most 8 KiB. A pattern that cannot be used lists nothing, the line under the title says why, and Enter opens nothing. Searching, switching the order or filter, and Esc never change a session file or send a request.

### Sharing a session and reporting a bug

Both commands run the GitHub CLI, `gh`, found on `PATH`, and need it signed in (`gh auth login`); without it they say so and send nothing. They run only while the session is idle, and Esc on their sheet cancels them, even while `gh` runs.

`/share` turns the saved session into one self-contained web page: every entry of the session file, including branches you left, the system prompt, and the tool definitions, viewable in a browser. A sheet then says what the page holds and asks whether to upload it; `No` is selected first, and anything but `Yes` uploads nothing. On `Yes`, `gh gist create --public=false` uploads it as a secret gist, which is unlisted but readable by anyone with its link, and the transcript shows `Gist: <url>`. The page is written to a private temporary directory that is removed afterwards. A session that is not saved (`--no-session`) or has no messages yet is refused.

`/bug [description]` asks what went wrong, starting from the description you typed, on a sheet that says the report becomes a public GitHub issue on `Justar96/bake`. It then asks whether to include the session transcript and, last, whether to create the issue, export a zip archive, or cancel; nothing is sent or written before that last choice. The report holds your description, the bake-rs version and user agent, the operating system and terminal, the names (never the values) of `BAKE_` and `DSH_` environment variables, the model and its provider without keys, `settings.json` with every secret-looking value replaced by `<redacted>`, and the failed or aborted model responses of this session without their content. The issue shows the description with the report folded below it; the transcript is never part of the issue, only of the zip archive, `bake-bug-report-<id>.zip` in the working directory. If the issue cannot be created, you can export the archive instead or get a link that opens GitHub's new-issue form filled in. A filed or exported report is recorded in the session file as a `bake.bug-report` entry, which is never sent to the model.

## The format

A session file is JSON Lines: a header line (`type` `session`, `version` 3, `id`, `timestamp`, `cwd`), then one entry per line, each with an `id` and a `parentId` naming the entry it follows, so the entries form a tree. Entries are appended and never rewritten, except that a file in Pi's older versions 1 and 2 is upgraded when it is opened. Lines that are damaged or cut short are skipped.

bake-rs syncs each append to disk, and a rewrite (an upgrade, or a branched session) goes through a temporary file renamed over the original, so a crash never leaves a half-written file. A line cut short by a crash stays skipped, and the next entry starts on a new line.

Only one run writes a session at a time. A run that writes a session holds a lock on a file beside it, `<session>.jsonl.lock`, while it runs; the lock is released when the run ends, even when it is killed. Another run that tries to write the same session, with `--continue` or `--session`, waits up to a second and then exits with status 1 and `Session file is in use by another writer: <path>`, before sending anything. Listing and finding sessions take no lock. The `.lock` files are left in place. Bake 0.3 Session logs are a different format, which bake-rs does not read.
