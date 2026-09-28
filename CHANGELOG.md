# Changelog

Notable changes to Bake. `/changelog` in the terminal prints the section for the running version, so each release heading must match the root `package.json` version.

## [Unreleased]

- Every request to a pi-ai route names its session in `x-deepseek-harness-session-id`, so a proxy that balances several upstream accounts, such as CliRelay with `session-sticky` routing, keeps a session on one account. Prompt caches belong to one account, so a session that hopped between them re-sent its whole context at full price on most steps.
- `/login cliproxyapi` writes two defaults for multi-account proxies: a retry delay of up to 60 seconds, and session affinity (`x-session-affinity`) on its Claude and Chat Completions models.
- A CLIProxyAPI route saved by an earlier release is upgraded when Bake starts, with no new login: each model's protocol, Claude's endpoint and adaptive thinking, and the two defaults above are filled in where missing, and a notice names what changed. Values you set by hand, including `false`, are kept. If the settings file cannot be written, the notice asks you to run `/login cliproxyapi` instead.
- When a proxy answers `429` or `503` with `reset_seconds` because every account for a model is cooling down, Bake waits that long before retrying if it fits the route's `retryPolicy.backoff.maxDelayMs`, and otherwise ends the turn at once instead of spending its retries inside the cooldown.
- A pi-ai route or model can set `compat.sendSessionAffinityHeaders` to send the session as `x-session-affinity` on Anthropic Messages and Chat Completions.
- Automatic compaction no longer trims old tool results just below the context threshold over and over. A trim alone must free at least half the room a summary would; otherwise the same pass also summarizes. Each rewrite invalidates the provider's prompt cache, and one long session had re-sent about 200k tokens after trims that saved 2k.
- `update_goal` accepts an objective or round cap copied unchanged from `get_goal` in an action that does not use it, instead of rejecting the call.

## [0.1.8] - 2026-09-28

- The goal on the header shows its state and round count without the objective, which Ctrl+O still opens. On a narrow terminal it gives up its parts one at a time instead of being clipped at the right edge, and keeps `● 3/256` where its label no longer fits.
- The header's spinner is now a round ball of dough being kneaded: squashed, pressed into a dome, folded from one side, rounded up, and folded from the other. It keeps its three-cell width and running orange.
- Installs and updates bake a loaf instead of the old ASCII oven: steam rises over it and it browns from dough to crust as the download and install advance. The installers, `bake update`, and `/update` in the terminal now draw the same row; `/update` shows it above the prompt instead of as notice text. Terminals that are not UTF-8, and CJK locales, get an ASCII loaf.
- `/settings` opens a panel of terminal settings saved to the settings file: screen mode (inline, the default, or fullscreen), language, borders, whether the header names the goal's objective, tool output lines, menu rows, the Ctrl+C quit window, and the default model and access for new sessions. Screen mode and language apply at the next launch; `--screen` still overrides the saved screen. Each value list marks the profile's `Default`, a pinned row resets the terminal settings after a confirmation, and a failed save keeps the panel open with its reason.
- `/login cliproxyapi` serves Claude models over Anthropic Messages instead of the proxy's Responses translator, so their prompts are cached: a long session re-reads its context at the cached rate instead of full price each turn. Claude models that offer `xhigh` or `max` use adaptive thinking. Image and video generators no longer appear in `/model`. Run the login again to update an existing route.
- A pi-ai model entry can set its own `baseURL`, for a model moved to a protocol that joins a different path onto the gateway.
- The model and reasoning effort chosen with `/model` or Shift-Tab are remembered: the next launch and `/new` start on them. Resumed sessions keep the model they recorded.

## [0.1.7] - 2026-09-27

- `--screen fullscreen` opens a scrollable transcript with the input pinned at the bottom. PgUp/PgDn scroll, Ctrl+Home goes to the start, and Ctrl+End follows new output. Exit restores the shell; inline mode remains the default.
- Closing the task, subagent, or goal view no longer lifts the prompt to the middle of the screen: the rows the view took stay blank above the prompt until new output fills them.
- An Escape that arrives together with the next key, as on a slow terminal, still closes an open view, and the typed characters go to the prompt.
- The subagents row counts the children and how many are working instead of listing their names, which the Ctrl+G view still shows. With tasks, a goal, and subagents all showing, the goal on the header drops its objective as well as its round count.
- Kimi, Moonshot, GLM, Qwen, DeepSeek, and MiniMax models added by `/login cliproxyapi` use Chat Completions. A Kimi turn after parallel tool calls no longer fails with `tool_call_ids did not have response messages`. Run the login again to update an existing CLIProxyAPI route.
- A model in a pi-ai provider profile can set its own `api`, so one route and credential can serve models over different wire protocols.

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
