# Context files

Context files hold instructions for the agent, such as how to build and test a project. bake adds them to the system prompt when a session starts, and again when they change between runs of a resumed session.

## Where they are found

In each directory searched, bake uses the first of these that is a readable file: `AGENTS.override.md`, `AGENTS.md`, `AGENTS.MD`. One file per directory is used, so an `AGENTS.override.md` hides the `AGENTS.md` beside it. `CLAUDE.md` files are not read.

The directories are searched in this order:

1. `~/.agents` in your home directory, for instructions that apply to every project.
2. Each directory from the filesystem root down to the session's working directory, so a project's file comes after its parent directories' files.

A file is included once even when two searches find it. In a git worktree nested inside its main repository, the main repository's file is left out when the worktree has its own. A file larger than 16 MiB, or one that cannot be read, is skipped with a warning.

Only the working directory and its parents are searched: a file in a subdirectory applies only when bake runs there, unless `nestedContextFiles` is on ([Files found while working](#files-found-while-working)). A resumed session searches from the working directory it was started in. Pass `-nc`, `--no-context-files` to skip loading `AGENTS.md` files for a run.

Skills, which the model reads only when a task calls for them, are in `bake docs skills`.

## Files found while working

With `"nestedContextFiles": true` in `settings.json` (off by default), bake also adds the context files of the subdirectories the model works in. When a `read`, `edit`, `write`, `grep`, `find`, or `ls` call succeeds on a path below the working directory, including a call made from a `codemode` script, bake checks each directory from just below the working directory down to that path's directory, or the path itself when it is a directory, with the same rule as above: the first of `AGENTS.override.md`, `AGENTS.md`, `AGENTS.MD`, with the same size limit and warnings. A file it has not already loaded is added before the model's next request, outermost first. So in a monorepo started at its root, a package's `AGENTS.md` reaches the model once it reads a file in that package. `bash` commands are not inspected, and a path outside the working directory, a failed call, or a run with `-nc` adds nothing.

The files found are in a section of the system prompt of their own, after the others, in the same `<project_instructions path="...">` form as the startup files. Each file is added once per session, even when the model has read it with a tool. Adding a file sends that whole section again, with every file found so far, as a change at the end of the conversation; the rest of the conversation stays in the provider's prompt cache. The section holds at most 64 KiB of file text: a file that would go past that is left out, and you see a warning (the model does not). In the interactive terminal each added file shows as `Loaded context file <path>`, `-p` prints the warnings on standard error, and `--mode json` reports a `nested_context` event with the `loaded` paths and the `warnings`.

The files found are recorded in the session, so a resumed or forked session and one after compaction keep them without adding them again; their text is read again when the session is opened, as the startup files are. `/tree` keeps those found on the branch it moves to. A subagent or spawned agent finds its own.

## Project trust

## Project trust

Pi asks whether to trust a project folder before it loads the project's own files, and `-a`, `--approve` and `-na`, `--no-approve` answer that for one run. bake has no such decision: a project's `AGENTS.md` files, its `.agents/skills`, and its `.bake/prompts` are always loaded, and it reads no project `settings.json` and no project `mcp.json`. Both flags are therefore refused with that reason instead of silently doing nothing; `-nc`, `-ns`, and `-np` leave each kind of file out for a run. `nestedContextFiles` extends this to the context files of the subdirectories the model works in. `--approval`, which is a different flag, sets the session's approval policy (`bake docs permissions`).
