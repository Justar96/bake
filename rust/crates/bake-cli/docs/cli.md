# The command line

bake-rs runs one headless turn per invocation: it sends the prompts, prints the result, and exits. It has no interactive mode yet. `bake-rs --help` lists every option, and `bake-rs docs` lists these topics.

## Running a prompt

```sh
bake-rs -p "Summarize the open TODOs in src/"
git diff | bake-rs -p $'\n\nReview this change'
bake-rs --mode json -p "List the tests" > events.jsonl
```

- `-p`, `--print` sends the prompts and prints the final answer. bake-rs also prints when `--mode` is given or standard input or output is not a terminal; otherwise it stops with an error.
- Each message argument is a prompt, sent in order. `--` ends the options, so later arguments are messages even when they start with `-`.
- When standard input is not a terminal, bake-rs reads it to the end before starting, up to 64 MiB, so give a run with nothing to pipe `< /dev/null`. The input is trimmed and joined to the first message with no separator, which is why the example above starts its message with a blank line.
- `--mode text` prints the text of the final answer; `--mode json` prints the session header and then every event as one JSON line.
- `@file` arguments are not supported yet.

Model flags are in `bake-rs docs models`, session flags in `bake-rs docs sessions`, and permission flags in `bake-rs docs permissions`.

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
| 0 | The turn finished |
| 1 | An error: a bad option, a startup failure, no usable model, or, in text mode, a model error (JSON mode reports it as an event) |
| 2 | An argument bake-rs cannot parse: one that is not UTF-8, or a bad argument to `preview` or `docs` |
| 129, 143 | Stopped by `SIGHUP` or `SIGTERM`; the session is closed first |

Startup errors and warnings go to standard error, most as `Error: ` and `Warning: ` lines; `No models available` and `No session found matching` print without the prefix, as in Pi. A model error in text mode prints as its message.

## Other commands

- `bake-rs docs [topic]` prints this documentation. It needs no terminal, model, or Bake home.
- `bake-rs preview [--inline]` opens a sample of the terminal interface with sample content. It needs an interactive terminal and connects to no model; Ctrl+C quits.
- `bake-rs --version` prints the version.
