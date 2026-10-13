# Sessions

bake-rs saves each conversation as a session file in Pi's session format, so a later run can continue it. A session records its messages, its model and thinking level, compactions, and its permission.

## Where sessions live

Sessions live in `sessions/` in the Bake home, in one directory per working directory: `/home/me/project` becomes `--home-me-project--`. Each file is named after its start time and id, such as `2026-01-02T03-04-05-678Z_<id>.jsonl`. `sessionDir` in `settings.json` names another directory to use instead.

## Choosing a session

- With no session flag, a run starts a new session.
- `-c`, `--continue` continues the most recently changed session for the working directory, or starts one when there is none.
- `--session <path|id>` opens a session. An argument that contains `/` or `\` or ends in `.jsonl` is a path, relative to the working directory; a path to a file that does not exist starts a new session saved there. Anything else is a session id or the start of one, looked up among the working directory's sessions. An id that belongs to another project's session is an error, and one that matches nothing prints `No session found matching '<id>'`.
- `--no-session` keeps the session in memory and saves nothing.

A resumed session keeps its model, thinking level, and permission unless a flag changes them.

## The format

A session file is JSON Lines: a header line (`type` `session`, `version` 3, `id`, `timestamp`, `cwd`), then one entry per line, each with an `id` and a `parentId` naming the entry it follows, so the entries form a tree. Entries are appended and never rewritten, except that a file in Pi's older versions 1 and 2 is upgraded when it is opened. Lines that are damaged or cut short are skipped. bake-rs does not take a lock on the file, so two runs must not write the same session at once. Bake 0.3 Session logs are a different format, which bake-rs does not read.
