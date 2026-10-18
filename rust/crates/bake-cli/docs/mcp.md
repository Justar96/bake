# MCP servers

bake-rs starts the Model Context Protocol servers listed in `mcp.json` in the Bake home for every session, and gives the model their tools as `mcp__<server>__<tool>`. Only stdio servers run for now: each is a program bake-rs starts and talks to over its standard input and output.

## mcp.json

The file uses the `mcpServers` shape other MCP clients share:

```json
{
  "mcpServers": {
    "files": {
      "command": "mcp-files",
      "args": ["serve", "."],
      "env": { "FILES_TOKEN": "${FILES_TOKEN}" },
      "cwd": "tools",
      "timeout": 60
    }
  }
}
```

- `command` is the program, run directly without a shell; `args` are its arguments. A leading `~` in either names your home directory.
- `env` sets variables over the server's environment. A value may reference variables as `$NAME` or `${NAME}`; a value that starts with `!` would run a command, which bake-rs refuses.
- `cwd` is the server's directory, relative to the session's working directory.
- `timeout` is each request's limit in seconds (default 60).
- `enabled: false` keeps an entry without starting it.
- `exposure` is `direct` (the default: its tools are given to the model) or `hidden` (they are not); `toolExposure` sets it for single tools by name or `*` pattern.
- `description` is a note for you.

A server name may use letters, digits, `_`, and `-`, up to 32 characters. bake-rs reads only this file: there is no project `mcp.json`, so a repository cannot add or change a server.

## Environment and credentials

A server starts with bake-rs's environment minus the variables that look like credentials (names containing `KEY`, `PASSWORD`, `SECRET`, or `TOKEN`, in any case), every `DSH_*` variable, and the `BAKE_*` variables Bake manages, such as `BAKE_HOME`. A variable named in `env` is passed even when it would be removed, so `"FILES_TOKEN": "${FILES_TOKEN}"` hands that one credential to that one server. Because of that, bake-rs reads `mcp.json` only when its owner alone can change it, and otherwise starts none of its servers:

- `mcp.json` must be a regular file, not a link or a FIFO, of at most 1 MiB.
- On Unix it and every directory above it must belong to you or root, and other users must not be able to write them (`chmod go-w`); a directory with the sticky bit, such as `/tmp`, is allowed.
- Below your home directory, the file must not be inside the session's working directory, which the agent's tools can change, or inside a git work tree, whose files come from its repository. Set `BAKE_HOME` to another directory, or run with `--no-mcp`.

bake-rs reads those directories and the file one step at a time without following links, so a link changed while it reads cannot point it elsewhere.

On Windows, bake-rs cannot yet check who may change the file, so it refuses an existing `mcp.json` there unread and starts no server from it; remove the file or use `--no-mcp` until that check is supported.

## The sandbox

Each server starts under the session's sandbox mode (`bake-rs docs permissions`) and keeps that mode until it stops: under `workspace-write` it can change files only in the working directory and the temporary directories, and under `read-only` nowhere. Where no sandbox backend enforces the mode (Windows, for now), the server does not start; under `danger-full-access` it runs unconfined. Changing the permission later does not restart or reconfine a running server, and MCP tool calls are not approved one by one.

## Startup and problems

Servers start in the background as the session opens. The first prompt waits up to 10 seconds for servers still starting, so their tools are in its first request; a server that is slower adds its tools to a later prompt. Esc stops a running MCP call without stopping its server. When the session ends, bake-rs stops every server and the processes it started before it exits; in print mode Ctrl+C, `SIGTERM`, and `SIGHUP` do the same at any point, while startup waits for piped input or for servers, or while a prompt runs. `/new`, `/resume`, `/fork`, and `/clone` stop the current session's servers before the next session starts its own.

## Tool flags

`--tools`, `--exclude-tools`, and `--no-tools` restrict MCP tools as they restrict the built-in ones, including tools a server adds later. A tool they leave out is neither given to the model nor run if the model names it.

- `--tools` with names or `*` patterns gives the model only the MCP tools it matches, such as `--tools read,'mcp__files__*'`. A list of only `+name` and `-name` changes the built-in defaults and leaves MCP tools alone.
- `--exclude-tools` removes the MCP tools it matches.
- `--no-tools` alone gives the model no tool, so bake-rs starts no MCP server; with `--tools`, only the tools it names.

A problem with `mcp.json` or one of its entries is a warning when bake-rs starts; the other entries still start. In print mode, a server that could not start, failed, or is still connecting after the wait is also a warning before the answer. Interactive mode does not show a server's own failure yet.

`--no-mcp` reads no `mcp.json` and starts no server.

## Not supported yet

Servers with a `url` (Streamable HTTP) are skipped with a warning, because bake-rs cannot yet admit their network access. OAuth sign-in, reconnecting a server that stopped (its tools are removed instead), resources, prompts, server instructions, code mode and tool search exposures, and a `/mcp` command are not available.
