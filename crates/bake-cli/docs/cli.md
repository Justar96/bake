# The command line

In a terminal, bake opens an interactive session; with `-p` or `--mode`, or when standard input or output is not a terminal, it runs one headless turn, prints the result, and exits. `bake --help` lists every option, and `bake docs` lists these topics.

## Interactive sessions

```sh
bake                       # a new session, fullscreen
bake --inline -c           # continue the latest session, in the terminal's scrollback
bake -r                    # pick the session to open, then keep it open
bake "Explain src/main.rs" # send a first prompt, then keep the session open
```

- The session is set up before the terminal changes: the same model, session, tool, and permission flags as `-p` apply, and an error such as an unreadable file exits with status 1 and leaves the terminal as it was. With no usable model or no API key, the session opens anyway with a note, so you can run `/login` or `/model`; prompts are refused until a model is ready. `-p` still exits with status 1.
- Message arguments and `@file` attachments are sent first, in order, each after the previous turn ends.
- Enter sends the draft. While a turn runs, Enter steers it: the prompt joins the turn at its next step. A steer takes text only: a draft with staged images (Ctrl+V or an image path) is kept until the turn ends. Alt+↑ sends prompts that wait.
- Esc stops the turn. Steers it had not taken yet go back into the draft.
- A prompt the session refuses, such as one sent without an API key, goes back into the draft with its images and the reason.
- `/skill:<name>` and prompt-template prompts are expanded as in `-p`.
- `/model [pattern]` sets a model that matches exactly, or opens a list of models by name, grouped by provider and, under each provider, by the lab that makes them (Anthropic, Google, OpenAI, and so on), with each model's id and route; `/thinking` opens the thinking levels, and Shift+Tab in the composer moves to the model's next level, from its highest back to its lowest, for this session; the status line shows the level. The model list filters as you type, including by id or route. Both lists mark the current choice and scroll with ↑↓, PgUp, PgDn, Home, and End. In either list, Enter changes this session, and Ctrl+S also saves it as the default in `settings.json`. `/settings` changes the steering mode, follow-up mode, and auto-compaction, saving each change in `settings.json`.
- `/login` signs in to CLIProxyAPI: it asks for the proxy address, then the API key (shown as dots), checks the key against the proxy's model list, and saves the account (see [Models](models.md#the-cliproxyapi-account)). `/refresh-models` reads the model list again, and `/logout` signs out. Esc cancels a command until it saves; once the check has passed, the save always finishes and is reported.
- `/session` shows the session's name, file, id, and message and token counts; `/name <name>` names the session, and `/name` alone asks for a name, starting from the current one. `/new` starts a new session, `/resume` lists and searches this working directory's sessions to switch to, `/tree` continues from an earlier point of this session, `/fork` starts a new session from an earlier message, and `/clone` starts a new session holding the whole current conversation (see [Sessions](sessions.md#in-an-interactive-session)). `/compact [instructions]` summarizes the older conversation to free context, with optional instructions for the summary (see [Compacting](sessions.md#compacting)).
- `/changelog` shows the release notes this build ships, oldest first, under `What's New`. `/share` uploads the session as a secret GitHub gist, and `/bug [description]` files a bug report as a public issue on Bake's GitHub repository or exports it as a zip archive; both use the GitHub CLI (`gh`) and ask before anything is sent (see [Sharing a session and reporting a bug](sessions.md#sharing-a-session-and-reporting-a-bug)).
- Slash commands bake does not run yet are refused with the draft kept; they are planned for a later 0.4.x release. They are Pi's remaining built-ins (`/scoped-models`, `/export`, `/import`, `/copy`, `/hotkeys`, `/trust`, `/reload`, `/quit`) and Bake 0.3's terminal and runtime commands (`/agents`, `/attach`, `/clear-attachments`, `/remove-attachment`, `/help`, `/terminal-setup`, `/update`, `/sessions`, `/clear`, `/goal`, `/feedback`, `/permission`). Any other `/word` that names no prompt template is refused as well. Only one command runs at a time.
- A message argument is checked the same way before it is sent. One that is refused, or that names a command bake runs, such as `/model`, is not sent: it goes into the draft with the reason, and the messages after it still run. Print and RPC mode refuse nothing: they send the text to the model as Pi does.
- Pi's extension and theme flags (`--extension`, `-e`, `--no-extensions`, `-ne`, `--theme`, `--use-theme`, `--no-themes`) and `--tui-mode` are not part of bake: it refuses them as unknown options, with status 1. `--inline` chooses between the full screen and the scrollback, and themes are not configurable yet.
- Ctrl+C twice quits. Quitting, `SIGINT`, `SIGTERM`, and `SIGHUP` stop the turn and close the session before the terminal is restored.
- Under `--approval ask`, a call that asks for wider access shows an approval panel with the tool, the call id, the reason, and the call's arguments. Once its keys show, press `y` then Enter to allow that call once, `n` to reject it, or Esc to cancel it. Keys typed before the panel appeared, a paste, and Enter alone never approve. Nothing is granted for the rest of the session. Quitting or closing the terminal while a request waits refuses it. See [Permissions](permissions.md#approvals).

## Running a prompt

```sh
bake -p "Summarize the open TODOs in src/"
git diff | bake -p $'\n\nReview this change'
bake --mode json -p "List the tests" > events.jsonl
```

- `-p`, `--print` sends the prompts and prints the final answer. bake also prints when `--mode` is given or standard input or output is not a terminal; otherwise it opens an interactive session.
- Each message argument is a prompt, sent in order. `--` ends the options, so later arguments are messages even when they start with `-`.
- When standard input is not a terminal, bake reads it to the end before starting, up to 64 MiB, so give a run with nothing to pipe `< /dev/null`. The input is trimmed and joined to the first message with no separator, which is why the example above starts its message with a blank line.
- `--mode text` prints the text of the final answer; `--mode json` prints the session header and then every event as one JSON line.
- `--max-model-calls <n>` limits model requests across all prompts, retries, and compaction summaries in one print invocation. `--run-timeout-ms <n>` sets its deadline, starting before the first prompt after startup. Both are optional; zero admits no request. Exhaustion stops new work, cancels active work, waits for cleanup, and exits 1. Cleanup can extend past the deadline. These flags require print mode.
- JSON `agent_settled` events include an `outcome` with `kind` equal to `finished_turn`, `cancelled`, `budget_exhausted`, or `failed`. A finished turn does not assert that the task's result has been independently verified.
- `--recoverable` journals one print prompt. `--resume-run <id> --session <path>` selects its saved run, retaining spent requests and refusing uncertain tool effects or interrupted deadlines. Recovery takes no new prompt or replacement limits; see `bake docs sessions`.
- `@file` arguments attach files to the first prompt: text files are wrapped in `<file name="...">` tags, and supported images (`png`, `jpg`, `gif`, `webp`, `bmp`) are attached and resized to the model's limits. Empty files are skipped, and a missing file stops the run with exit status 1.
- `--system-prompt <text|path>` replaces the default system prompt preamble, rules, and docs section with literal text or the contents of a file, while keeping the run, context, skills, permissions, and environment sections.
- `--append-system-prompt <text|path>` appends literal text or a file's contents to the system prompt. Give it more than once to join several parts with blank lines.
- `--prompt-template <path>` loads a Markdown prompt template file or directory (repeatable); `-np`, `--no-prompt-templates` skips the default `prompts/` directories (`$BAKE_HOME/prompts` and `<cwd>/.bake/prompts`). A prompt starting with `/<name> [args...]` expands that template (`$1`, `$2`, `$@`, `$ARGUMENTS`, `${N:-default}`, `${@:N}`, `${@:N:L}`).

- `--verbose` is accepted and does nothing. Pi's flag forces verbose startup over its `quietStartup` setting; bake has no such setting and no quiet startup, so it always reports the session file, a model fallback, every setup warning, and each MCP server's failure.

Model flags are in `bake docs models`, session flags in `bake docs sessions`, context flags in `bake docs context`, permission flags in `bake docs permissions`, `--no-mcp` in `bake docs mcp`, and `--skill`, `--no-skills`, and `/skill:<name>` prompts in `bake docs skills`.

## Ghostty and Herdr

On Linux and macOS, Bake enables Kitty keyboard reporting for Ghostty and other terminals that support it. Shift+Enter inserts a newline, Enter submits, and held-key repeats cannot answer an approval. Bake restores the previous keyboard mode on exit, including handled signals. Terminals without this protocol keep their existing key behavior.

The terminal title reads `bake - <directory>`, or `bake - <name> - <directory>` once `/name` names the session. While a turn runs it starts with `◐`, and while an approval waits, with `?`. Bake saves the previous title on the terminal's title stack and restores it on exit where the terminal keeps one. Ghostty does not keep one; its shell integration sets the title again at the next prompt.

In Ghostty, Bake also uses two status features:

- **Progress bar.** Ghostty's progress bar (OSC 9;4) moves while a turn or `/compact` runs, pauses while an approval waits, and turns to the error state for 10 seconds after a turn fails. Quitting removes it.
- **Desktop notifications.** A notification (OSC 777) says when a turn completes or fails, or when a tool needs approval. It is sent only while the Ghostty window or tab is out of focus. Bake turns on focus reporting for this and turns it off on exit. Ghostty's `desktop-notifications = false` setting also stops them.

Bake detects Ghostty from `TERM_PROGRAM=ghostty`. Over SSH, where that variable is usually not passed on, it uses `TERM=xterm-ghostty`. Under tmux or Herdr, which set `TERM_PROGRAM` to their own names, both features stay off. The `terminal.showTerminalProgress` and `terminal.notifications` settings (`bake docs settings`) turn them on or off in any terminal. Keep the progress bar off in a terminal that shows every OSC 9 sequence as a notification.

Run `bake` inside a [Herdr](https://herdr.dev/docs/add-herdr-support/) pane to report its state automatically: `working` during a turn, `blocked` while an approval waits, and `idle` when ready. A `blocked` report gives the reason, such as `bash needs approval`. A dialog opened by a command such as `/model` or `/resume` leaves the state unchanged, since you are already answering it. No integration installation is needed. Reporting requires the `HERDR_ENV=1`, `HERDR_PANE_ID`, `HERDR_BIN_PATH`, and `HERDR_SOCKET_PATH` environment Herdr supplies. Reports run in the background with a short timeout; failures do not interrupt Bake. Quitting releases the pane.

Bake also sends Herdr display tokens for its agent sidebar:

| Token | Value |
|---|---|
| `$model` | The model, without its provider |
| `$thinking` | The thinking level, for models that reason |
| `$context` | How much of the context window the last response used, such as `42%` |
| `$session` | The session's name |
| `$activity` | What a turn is doing, such as `running bash`, or what it is waiting for |

Herdr shows a token only when a row in `[ui.sidebar.agents] rows` names it, for example `rows = [["state_icon", "agent", "$model", "$context"], ["$activity"]]`. Reports and tokens contain only these values, never prompts, tool arguments, or answers. Quitting clears the tokens before releasing the pane. Inside Herdr, Bake's title becomes the pane's `terminal_title`; `terminal_title_stripped` leaves out the `◐` marker.

Herdr 0.9.2 and later can reopen a saved session after a server restart. Bake reports `bake --session <file>` with its current model, thinking level, and launch permission, sandbox, tool restrictions, and offline/inline options. Session changes refresh this command. Unsaved sessions reopen as fresh unsaved sessions. Herdr cannot accept restore arguments containing apostrophes or control characters, or oversized commands; those sessions also reopen fresh. If the launch restrictions themselves cannot be represented, the fresh session is offline, read-only, and has no tools. This replaces any older session's restore command. Keep custom environment settings in your shell configuration for restored panes.

## Tools

A session starts with `read`, `bash`, `edit`, `write`, the background-job tools `job_output`, `job_list`, and `job_kill`, the web tools `web_search` and `web_fetch`, and `codemode` (or the `defaultTools` setting). `grep`, `find`, and `ls` are also available; `grep` and `find` need `rg` and `fd` (or `fdfind`) on `PATH`.

- `-t`, `--tools <list>` gives a comma-separated list. Names and `*` patterns allow only those tools; `+name` and `-name` add to or remove from the default set.
- `-xt`, `--exclude-tools <list>` removes tools by name or `*` pattern.
- `-nt`, `--no-tools` starts with no tools; with `-t`, only the tools it names.
- `-nbt`, `--no-builtin-tools` starts with none of the built-in tools active.

For example, `bake -t +grep,+find -p "..."` adds `grep` and `find` to the defaults.

`codemode`, which runs a model-written JavaScript script that calls the other tools, is in the default set: `-xt codemode`, a `-t` list or `-name` entry without it, or a `defaultTools` list without it turns it off (`bake docs codemode`).

## Web tools

`web_fetch` retrieves one public HTTP(S) URL anonymously, without cookies or credentials, and returns it as text, with HTML converted to Markdown. It refuses URLs with credentials and any destination that is not a public address, such as `localhost`, private networks, link-local and cloud metadata addresses, checking every address a name resolves to and connecting only to those it checked. It follows at most five redirects, all on the same origin; a redirect to another origin is reported for a new call. A fetch reads at most 5,000,000 bytes and 100,000 characters, stops after 30 seconds, and returns at most 200,000 characters. It reaches the network directly, not through a proxy.

`web_search` runs one to four queries through the CLIProxyAPI route: each is a request to one of the route's Anthropic Messages models with the provider's server-side web search tool, and only the sources that tool returns are given to the model. The `webSearch.model` setting chooses that model (`bake docs settings`). Each query costs one model request on the route; a search stops after 60 seconds and returns at most eight sources. The request each query sent, without its key, is kept with the tool result in the session.

Both tools are in every session's default set, a subagent's included, and are removed like any other tool, such as with `-t -web_search,-web_fetch` or a `defaultTools` list without them. They need no approval in any permission (`bake docs permissions`). Page and search content is marked as untrusted data for the model, but a model can still send data to a public URL it fetches.

MCP servers configured in the Bake home's `mcp.json` add tools named `mcp__<server>__<tool>`. These flags restrict them too, including tools a server adds later: a `--tools` list of names or patterns must match one (`+name`/`-name` lists leave them alone), `--exclude-tools` removes the ones it matches, and `--no-tools` alone starts no server. `--no-mcp` starts no MCP server, so a session has none of their tools (`bake docs mcp`).

When the model sends several tool calls in one response, they run at once if they only read; a response with a `bash`, `edit`, or `write` call runs its calls one at a time, in the order the model wrote them, so a command that checks an edit sees the edited file.

Commands the `bash` tool runs receive the session's details: `BAKE_SESSION_ID`, `BAKE_SESSION_FILE` (when the session is saved), `BAKE_PROVIDER`, `BAKE_MODEL`, and `BAKE_REASONING_LEVEL`. Output too long for the result is saved to a `bake-bash-…` or `bake-output-…` file in the temporary directory, which the result names.

## Exit status

| Status | Meaning |
|---|---|
| 0 | The turn finished, or the interactive session was quit |
| 1 | An error, a model failure, or an exhausted run budget; model failures also appear in JSON events |
| 2 | An argument bake cannot parse: one that is not UTF-8, or a bad argument to `preview`, `docs`, `--self-check`, or a `plugin` command |
| 129, 130, 143 | Stopped by `SIGHUP` or a closed terminal, `SIGINT` (Ctrl+C; also Ctrl+Break on Windows), or `SIGTERM`; the session, its MCP servers, and the processes its tools started are closed first |

Startup errors and warnings go to standard error, most as `Error: ` and `Warning: ` lines; `No models available` and `No session found matching` print without the prefix, as in Pi. Model failures and exhausted run budgets print their reason to standard error and stop later prompt arguments.

## Other commands

- `bake docs [topic]` prints this documentation. It needs no terminal, model, or Bake home.
- `bake preview [--inline]` opens a sample of the terminal interface with sample content and scripted turns. It needs an interactive terminal and connects to no model; Ctrl+C twice quits.
- `bake plugin check <dir> [<tool> <args-json>]` is a diagnostic. It starts the Wasm plugin package in `<dir>` (its `plugin.json` and its component, `plugin.wasm`) in a helper process with the default plugin limits, and prints one line per step as it happens: the manifest, `helper ready:` with the helper's memory limit and process id, and, given a tool name and a JSON object of arguments, `calling <tool> (request N)` once the helper is running the call, each log line of the plugin's answer, and then `continue` or `block: <reason>`. It then shuts the helper down. Text from the plugin or the helper is printed with control characters escaped. It exits 0 only when the plugin started, answered, and the helper then exited by itself and was awaited with its output closed; 1, with an `Error:` line, for any other outcome, including a shutdown that was not clean or did not finish; and 130 after Ctrl+C, which cancels the start or call and waits for the helper to be torn down. It needs no model or Bake home.
- `bake plugin install <dir>`, `plugin enable <sha256>`, `plugin disable <id>`, and `plugin list` manage the Wasm plugins the Bake home enables for sessions (`bake docs plugins`).
- `bake auth check`, `bake auth print-api-key`, and `bake auth print-bearer-token` report whether a provider is ready to send and print its credential (`bake docs models`). `bake auth` prints their help. They read the Bake home's files, reach no network, and write nothing.
- `bake --version` prints the version.
- `bake --self-check`, given alone, checks that this executable runs on this host and carries its documentation, and prints one line of JSON naming its version, its platform (such as `linux-x64`), and the SHA-256 of each documentation topic. It reads no Bake home, credentials, or network, needs no terminal, Node, or Bun, and does not test a model, provider, or sandbox. It exits 1 on a platform Bake does not release for, and 2 when given another argument.
