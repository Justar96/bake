# Context files

Context files hold instructions for the agent, such as how to build and test a project. bake-rs adds them to the system prompt when a session starts, and again when they change between runs of a resumed session.

## Where they are found

In each directory searched, bake-rs uses the first of these that is a readable file: `AGENTS.override.md`, `AGENTS.md`, `AGENTS.MD`. One file per directory is used, so an `AGENTS.override.md` hides the `AGENTS.md` beside it. `CLAUDE.md` files are not read.

The directories are searched in this order:

1. `~/.agents` in your home directory, for instructions that apply to every project.
2. Each directory from the filesystem root down to the session's working directory, so a project's file comes after its parent directories' files.

A file is included once even when two searches find it. In a git worktree nested inside its main repository, the main repository's file is left out when the worktree has its own. A file larger than 16 MiB, or one that cannot be read, is skipped with a warning.

Only the working directory and its parents are searched: a file in a subdirectory applies only when bake-rs runs there. A resumed session searches from the working directory it was started in.

Skills, which the model reads only when a task calls for them, are in `bake-rs docs skills`.
