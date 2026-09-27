# Changelog

Notable changes to Bake. `/changelog` in the terminal prints the section for the running version, so each release heading must match the root `package.json` version.

## [Unreleased]

- Closing the task, subagent, or goal view no longer lifts the prompt to the middle of the screen: the rows the view took stay blank above the prompt until new output fills them.
- An Escape that arrives together with the next key, as on a slow terminal, still closes an open view, and the typed characters go to the prompt.
- The subagents row counts the children and how many are working instead of listing their names, which the Ctrl+G view still shows. With tasks, a goal, and subagents all showing, the goal on the header drops its objective as well as its round count.

## [0.1.6] - 2026-09-27

- A background job that finishes after its turn wakes the agent with one completion notice instead of two.
- When `update_goal` rejects a field the chosen action does not use, the error names the field and the empty value to send instead, so the agent stops retrying the same call.
- Resuming a long session replays its history sooner and allocates less than half the memory while drawing it.

## [0.1.5] - 2026-09-27

- The rows around the prompt stay compact when tasks, a goal, and subagents are all showing: the goal drops its round count, and the status line shows context as `ctx ~N%` instead of token totals. Context turns yellow from 70% and red from 90%.
- Subagents have their own dim row above the status line, each child named in its own colour; Down selects it and Enter opens the picker.
- Ctrl+T, Ctrl+G, and Ctrl+O each open and close their own view, and Tab and Shift-Tab move between open views.
- The access mode is shown when a session opens, in the welcome block or after a resumed session's heading, instead of on the status line.
- `/resume` lists sessions by last use, starts on the most recently used one, and names sessions without a title "Untitled session" beside their short id.
- Profiles can apply several bundle patches in order, and `--dump-config-schema` prints JSON Schema for the composed configuration so editors can validate and complete it.
- Plugin settings marked as live change without restarting the plugin, and the agent can inspect the live configuration of each plugin.
- Tools can be added and removed during a conversation; sessions that record such a change need this version or newer to open.
- Sessions from releases that used Agent Teams or model selection still open.
- Long tool previews no longer split an emoji or other surrogate pair, session-log uploads are bounded, and a request whose optional extensions cannot be sent falls back to the base request.
- A shell command without wider sandbox access may give an empty reason.

## [0.1.4] - 2026-09-26

- Each tool call opens with an icon for its kind: `$` a command, `≡` a read, `✎` an edit, `⌕` a search, `↳` a subagent started, `→` a message sent to one, and more; a batch of calls carries the icon on its header only.
- Ctrl+T cycles the task, subagent, and goal views and closes after the last; Tab and Shift-Tab move between them, and a tab strip names every view.
- The task view adds a progress bar, counts by state, and numbered tasks; the goal view adds a rounds bar and labels its objective and reason.
- Questions from the agent read more clearly: numbered options with aligned descriptions, `[✓]` boxes for multi-select, a step indicator across several questions, an Other row that stays in view with a hint, and a note when Enter has nothing to submit.
- The subagent view opens in place of the picker: Up and Down choose a child and Enter opens its session. `/agents <id>` opens a child directly.
- Long sessions no longer run out of memory. Bake now loads React's production build, which does not record a performance entry for every render; commands Bake runs still see your own `NODE_ENV`.

## [0.1.3] - 2026-09-26

- `/update` installs the newest release from inside the terminal, with download progress; the status line then asks for a restart.
- A new release is noticed within the hour, a failed check is retried after ten minutes, and an open terminal keeps checking.
- `bake update` shows download progress.
- `--resume <id>` resumes a Session in both the terminal and headless profiles; `--session-id` remains an alias.
- Subagents fold into one status-line field with their count and how many are working; Down selects it and Enter opens the child picker.
- The goal moves back to the header row. Up past the oldest history entry selects it and restores your unsent draft, and Enter or Ctrl+O opens a scrollable view of the full goal.
- The task list folds into one row above the header with its progress and the current task; Ctrl+T, or Enter on the selected row, opens the full checklist.
- Large tool results, long reasoning, and parents with many subagents no longer slow the terminal: a 20,000-line read previews in milliseconds instead of seconds.

## [0.1.2] - 2026-09-25

- Slash commands complete their arguments: `/goal`, `/plan`, and `/permission` offer their choices, `/login` offers sign-in targets, and `/model` offers models.
- Enter on a command that needs input fills it in instead of running it, and its usage line stays visible while you type the arguments.
- An unknown slash command keeps your draft and suggests the nearest command.
- A new session starts on CLIProxyAPI when it is set up and no DeepSeek key is, and the "no credential" notice appears only when no provider is configured.
- `/help` lists commands in columns, and `/help <command>` explains one.
- Usage errors from `/compact`, `/feedback`, `/permission`, and `/plan` show the expected form, with the reason on the next line.
- `/feedback` now says where the session history goes: shared through telemetry, or kept in the local session log when telemetry is off.
- The active goal has its own block above the header, and the header's spinner lines up with the tool-call markers.
- The README covers installing, updating, and uninstalling Bake on macOS, Linux, and Windows.

## [0.1.1] - 2026-09-25

- `bake update` installs the newest release in place, and `bake update --check` only reports; any failure leaves the install as it was. Installs of 0.1.0 run the installer once more to get it.
- Releases are signed: the installers and `bake update` refuse a manifest the Bake release key did not sign. Every release is also published on GitHub Releases.
- The status line names a newer release when one is available.
- Installs and updates show a small progress line in interactive terminals; `BAKE_NO_ANIMATION=1` turns it off.
- A header row above the composer shows what the current turn is doing and the active goal.
- Final answers end with a dim tokens-per-second line.
- Shift+Tab cycles the reasoning effort.
- The live area fits the terminal's size, and a long batch of tool calls keeps its heading, folding older calls into a count.
- Tool calls show a `●` heading with a `⎿` result line.
- Models whose streams report empty token usage no longer reset the context meter to 0%.

## [0.1.0]

- Bake is an independent terminal coding agent with a Bun-managed workspace, a Node runtime, and `~/.bake` as its default home.
- A fresh session opens with a welcome block showing the Bake version, a short notice, and example commands.
- `/changelog` shows what changed in the running version.
- The terminal follows the newest line, prints answers as they stream, and merges each action's call and result into one block.
- Tool cards, a task checklist, and a subagent panel sit above the composer.
- Session discovery skips damaged compressed headers instead of hiding healthy sessions.
