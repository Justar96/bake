# The command line

In a terminal, bake-rs opens an interactive session; with `-p` or `--mode`, or when standard input or output is not a terminal, it runs one headless turn, prints the result, and exits. `bake-rs --help` lists every option, and `bake-rs docs` lists these topics.

## Interactive sessions

```sh
bake-rs                       # a new session, fullscreen
bake-rs --inline -c           # continue the latest session, in the terminal's scrollback
bake-rs "Explain src/main.rs" # send a first prompt, then keep the session open
```

- The session is set up before the terminal changes: the same model, session, tool, and permission flags as `-p` apply, and an error such as an unreadable file exits with status 1 and leaves the terminal as it was. With no usable model or no API key, the session opens anyway with a note, so you can run `/login` or `/model`; prompts are refused until a model is ready. `-p` still exits with status 1.
- Message arguments and `@file` attachments are sent first, in order, each after the previous turn ends.
- Enter sends the draft. While a turn runs, Enter steers it: the prompt joins the turn at its next step. A steer takes text only: a draft with staged images (Ctrl+V or an image path) is kept until the turn ends. Alt+↑ sends prompts that wait.
- Esc stops the turn. Steers it had not taken yet go back into the draft.
- A prompt the session refuses, such as one sent without an API key, goes back into the draft with its images and the reason.
- `/skill:<name>` and prompt-template prompts are expanded as in `-p`.
- `/model [pattern]` sets a model that matches exactly, or opens a list of models by name, grouped by provider, with each model's route; `/thinking` opens the thinking levels. The model list filters as you type, including by id or route. Both lists mark the current choice and scroll with ↑↓, PgUp, PgDn, Home, and End. In either list, Enter changes this session, and Ctrl+S also saves it as the default in `settings.json`. `/settings` changes the steering mode, follow-up mode, and auto-compaction, saving each change in `settings.json`.
- `/login` signs in to CLIProxyAPI: it asks for the proxy address, then the API key (shown as dots), checks the key against the proxy's model list, and saves the account (see [Models](models.md#the-cliproxyapi-account)). `/refresh-models` reads the model list again, and `/logout` signs out. Esc cancels a command until it saves; once the check has passed, the save always finishes and is reported.
- `/session` shows the session's name, file, id, and message and token counts; `/name <name>` names the session, and `/name` alone asks for a name, starting from the current one. `/new` starts a new session, `/resume` lists and searches this working directory's sessions to switch to, `/tree` continues from an earlier point of this session, and `/fork` starts a new session from an earlier message (see [Sessions](sessions.md#in-an-interactive-session)). `/compact [instructions]` summarizes the older conversation to free context, with optional instructions for the summary (see [Compacting](sessions.md#compacting)).
- Pi's other slash commands, such as `/clone`, are not available yet; they are refused and the draft is kept. Only one command runs at a time.
- Ctrl+C twice quits. Quitting, `SIGINT`, `SIGTERM`, and `SIGHUP` stop the turn and close the session before the terminal is restored.
- Under `--approval ask`, a call that asks for wider access shows an approval panel with the tool, the call id, the reason, and the call's arguments. Once its keys show, press `y` then Enter to allow that call once, `n` to reject it, or Esc to cancel it. Keys typed before the panel appeared, a paste, and Enter alone never approve. Nothing is granted for the rest of the session. Quitting or closing the terminal while a request waits refuses it. See [Permissions](permissions.md#approvals).

## Running a prompt

```sh
bake-rs -p "Summarize the open TODOs in src/"
git diff | bake-rs -p $'\n\nReview this change'
bake-rs --mode json -p "List the tests" > events.jsonl
```

- `-p`, `--print` sends the prompts and prints the final answer. bake-rs also prints when `--mode` is given or standard input or output is not a terminal; otherwise it opens an interactive session.
- Each message argument is a prompt, sent in order. `--` ends the options, so later arguments are messages even when they start with `-`.
- When standard input is not a terminal, bake-rs reads it to the end before starting, up to 64 MiB, so give a run with nothing to pipe `< /dev/null`. The input is trimmed and joined to the first message with no separator, which is why the example above starts its message with a blank line.
- `--mode text` prints the text of the final answer; `--mode json` prints the session header and then every event as one JSON line.
- `@file` arguments attach files to the first prompt: text files are wrapped in `<file name="...">` tags, and supported images (`png`, `jpg`, `gif`, `webp`, `bmp`) are attached and resized to the model's limits. Empty files are skipped, and a missing file stops the run with exit status 1.
- `--system-prompt <text|path>` replaces the default system prompt preamble, rules, and docs section with literal text or the contents of a file, while keeping the run, context, skills, permissions, and environment sections.
- `--append-system-prompt <text|path>` appends literal text or a file's contents to the system prompt. Give it more than once to join several parts with blank lines.
- `--prompt-template <path>` loads a Markdown prompt template file or directory (repeatable); `-np`, `--no-prompt-templates` skips the default `prompts/` directories (`$BAKE_HOME/prompts` and `<cwd>/.bake/prompts`). A prompt starting with `/<name> [args...]` expands that template (`$1`, `$2`, `$@`, `$ARGUMENTS`, `${N:-default}`, `${@:N}`, `${@:N:L}`).

Model flags are in `bake-rs docs models`, session flags in `bake-rs docs sessions`, context flags in `bake-rs docs context`, permission flags in `bake-rs docs permissions`, and `--skill`, `--no-skills`, and `/skill:<name>` prompts in `bake-rs docs skills`.

## Tools

A session starts with `read`, `bash`, `edit`, and `write` (or the `defaultTools` setting). `grep`, `find`, and `ls` are also available; `grep` and `find` need `rg` and `fd` (or `fdfind`) on `PATH`.

- `-t`, `--tools <list>` gives a comma-separated list. Names and `*` patterns allow only those tools; `+name` and `-name` add to or remove from the default set.
- `-xt`, `--exclude-tools <list>` removes tools by name or `*` pattern.
- `-nt`, `--no-tools` starts with no tools; with `-t`, only the tools it names.
- `-nbt`, `--no-builtin-tools` starts with none of the built-in tools active.

For example, `bake-rs -t +grep,+find -p "..."` adds `grep` and `find` to the defaults.

When the model sends several tool calls in one response, they run at once if they only read; a response with a `bash`, `edit`, or `write` call runs its calls one at a time, in the order the model wrote them, so a command that checks an edit sees the edited file.

Commands the `bash` tool runs receive the session's details: `BAKE_SESSION_ID`, `BAKE_SESSION_FILE` (when the session is saved), `BAKE_PROVIDER`, `BAKE_MODEL`, and `BAKE_REASONING_LEVEL`. Output too long for the result is saved to a `bake-bash-…` or `bake-output-…` file in the temporary directory, which the result names.

## Exit status

| Status | Meaning |
|---|---|
| 0 | The turn finished, or the interactive session was quit |
| 1 | An error: a bad option, a startup failure, no usable model, or, in text mode, a model error (JSON mode reports it as an event) |
| 2 | An argument bake-rs cannot parse: one that is not UTF-8, or a bad argument to `preview` or `docs` |
| 129, 130, 143 | Stopped by `SIGHUP` or a closed terminal, `SIGINT` (interactive only), or `SIGTERM`; the session is closed first |

Startup errors and warnings go to standard error, most as `Error: ` and `Warning: ` lines; `No models available` and `No session found matching` print without the prefix, as in Pi. A model error in text mode prints as its message.

## Other commands

- `bake-rs docs [topic]` prints this documentation. It needs no terminal, model, or Bake home.
- `bake-rs preview [--inline]` opens a sample of the terminal interface with sample content and scripted turns. It needs an interactive terminal and connects to no model; Ctrl+C twice quits.
- `bake-rs --version` prints the version.
