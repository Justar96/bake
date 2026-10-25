# Context files

Context files hold instructions for the agent, such as how to build and test a project. bake adds them to the system prompt when a session starts, and again when they change between runs of a resumed session.

## Where they are found

In each directory searched, bake uses the first of these that is a readable file: `AGENTS.override.md`, `AGENTS.md`, `AGENTS.MD`. One file per directory is used, so an `AGENTS.override.md` hides the `AGENTS.md` beside it. `CLAUDE.md` files are not read.

The directories are searched in this order:

1. `~/.agents` in your home directory, for instructions that apply to every project.
2. Each directory from the filesystem root down to the session's working directory, so a project's file comes after its parent directories' files.

A file is included once even when two searches find it. In a git worktree nested inside its main repository, the main repository's file is left out when the worktree has its own. A file larger than 16 MiB, or one that cannot be read, is skipped with a warning.

Only the working directory and its parents are searched: a file in a subdirectory applies only when bake runs there. A resumed session searches from the working directory it was started in. Pass `-nc`, `--no-context-files` to skip loading `AGENTS.md` files for a run.

Skills, which the model reads only when a task calls for them, are in `bake docs skills`.

## Project trust

Pi asks whether to trust a project folder before it loads the project's own files, and `-a`, `--approve` and `-na`, `--no-approve` answer that for one run. bake has no such decision: a project's `AGENTS.md` files, its `.agents/skills`, and its `.bake/prompts` are always loaded, and it reads no project `settings.json` and no project `mcp.json`. Both flags are therefore refused with that reason instead of silently doing nothing; `-nc`, `-ns`, and `-np` leave each kind of file out for a run. `--approval`, which is a different flag, sets the session's approval policy (`bake docs permissions`).
