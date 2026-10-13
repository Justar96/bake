# The command line

bake-rs runs one headless turn per invocation: it sends the prompts, prints the result, and exits. It has no interactive mode yet. `bake-rs --help` lists every option, and `bake-rs docs` lists these topics.

## Running a prompt

```sh
bake-rs -p "Summarize the open TODOs in src/"
git diff | bake-rs -p "Review this change"
bake-rs --mode json -p "List the tests" > events.jsonl
```

- `-p`, `--print` sends the prompts and prints the final answer. bake-rs also prints when `--mode` is given or standard input or output is not a terminal; otherwise it stops with an error.
- Each message argument is a prompt, sent in order. Piped standard input, up to 64 MiB, is put before the first one. `--` ends the options, so later arguments are messages even when they start with `-`.
- `--mode text` prints the text of the final answer; `--mode json` prints the session header and then every event as one JSON line.
- `@file` arguments are not supported yet.

Model flags are in `bake-rs docs models`, session flags in `bake-rs docs sessions`, and permission flags in `bake-rs docs permissions`.

## Tools

A session starts with `read`, `bash`, `edit`, and `write` (or the `defaultTools` setting). `grep`, `find`, and `ls` are also available; `grep` and `find` need `rg` and `fd` on `PATH`.

- `-t`, `--tools <list>` gives a comma-separated list. Names and `*` patterns allow only those tools; `+name` and `-name` add to or remove from the default set.
- `-xt`, `--exclude-tools <list>` removes tools by name or `*` pattern.
- `-nt`, `--no-tools` starts with no tools; with `-t`, only the tools it names.
- `-nbt`, `--no-builtin-tools` starts with none of the built-in tools active.

For example, `bake-rs -t +grep,+find -p "..."` adds `grep` and `find` to the defaults.

## Exit status

| Status | Meaning |
|---|---|
| 0 | The turn finished |
| 1 | An error: a bad option, a startup failure, no usable model, or, in text mode, a model error (JSON mode reports it as an event) |
| 2 | A bad argument to `preview` or `docs` |
| 129, 143 | Stopped by `SIGHUP` or `SIGTERM`; the session is closed first |

Startup errors and warnings go to standard error as `Error: ` and `Warning: ` lines, and a model error in text mode as its message.

## Other commands

- `bake-rs docs [topic]` prints this documentation. It needs no terminal, model, or Bake home.
- `bake-rs preview [--inline]` opens a sample of the terminal interface with sample content. It needs an interactive terminal and connects to no model; Ctrl+C quits.
- `bake-rs --version` prints the version.
