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

## The format

A session file is JSON Lines: a header line (`type` `session`, `version` 3, `id`, `timestamp`, `cwd`), then one entry per line, each with an `id` and a `parentId` naming the entry it follows, so the entries form a tree. Entries are appended and never rewritten, except that a file in Pi's older versions 1 and 2 is upgraded when it is opened. Lines that are damaged or cut short are skipped.

bake-rs syncs each append to disk, and a rewrite (an upgrade, or a branched session) goes through a temporary file renamed over the original, so a crash never leaves a half-written file. A line cut short by a crash stays skipped, and the next entry starts on a new line.

Only one run writes a session at a time. A run that writes a session holds a lock on a file beside it, `<session>.jsonl.lock`, while it runs; the lock is released when the run ends, even when it is killed. Another run that tries to write the same session, with `--continue` or `--session`, waits up to a second and then exits with status 1 and `Session file is in use by another writer: <path>`, before sending anything. Listing and finding sessions take no lock. The `.lock` files are left in place. Bake 0.3 Session logs are a different format, which bake-rs does not read.
