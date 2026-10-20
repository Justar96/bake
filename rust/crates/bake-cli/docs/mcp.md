# MCP servers

bake-rs starts the Model Context Protocol servers listed in `mcp.json` in the Bake home for every session, and gives the model their tools as `mcp__<server>__<tool>`. A server is either a program bake-rs starts and talks to over its standard input and output (`command`), or a Streamable HTTP endpoint it connects to (`url`). A server that offers resources also adds three tools that list and read them, and a server's own instructions reach the system prompt.

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
    },
    "docs": {
      "url": "https://mcp.example.com/mcp",
      "headers": { "Authorization": "Bearer ${DOCS_TOKEN}" }
    }
  }
}
```

- `command` is the program, run directly without a shell; `args` are its arguments. A leading `~` in either names your home directory.
- `env` sets variables over the server's environment. A value may reference variables as `$NAME` or `${NAME}`; a value that starts with `!` would run a command, which bake-rs refuses.
- `cwd` is the server's directory, relative to the session's working directory.
- `url` is a Streamable HTTP endpoint instead of a program; `headers` are sent with every request to it, and their values may reference variables the same way. The legacy HTTP+SSE transport (`"type": "sse"`) is not supported.
- `timeout` is each request's limit in seconds (default 60). It also bounds a server that never answers `initialize`.
- `enabled: false` keeps an entry without starting it.
- `exposure` is `direct` (the default: its tools are given to the model) or `hidden` (they are not); `toolExposure` sets it for single tools by name or `*` pattern. A `hidden` server's resources are not reachable either.
- `description` is a one-sentence note about what the server offers. The model reads its first line in the `mcp_servers` section of the system prompt, in place of the server's own instructions.

A server name may use letters, digits, `_`, and `-`, up to 32 characters. bake-rs reads only this file: there is no project `mcp.json`, so a repository cannot add or change a server.

## Environment and credentials

A server starts with bake-rs's environment minus the variables that look like credentials (names containing `KEY`, `PASSWORD`, `SECRET`, or `TOKEN`, in any case), every `DSH_*` variable, and the `BAKE_*` variables Bake manages, such as `BAKE_HOME`. A variable named in `env` is passed even when it would be removed, so `"FILES_TOKEN": "${FILES_TOKEN}"` hands that one credential to that one server. Because of that, bake-rs reads `mcp.json` only when its owner alone can change it, and otherwise starts none of its servers:

- `mcp.json` must be a regular file, not a link or a FIFO, of at most 1 MiB.
- On Unix it and every directory above it must belong to you or root, and other users must not be able to write them (`chmod go-w`); a directory with the sticky bit, such as `/tmp`, is allowed.
- Below your home directory, the file must not be inside the session's working directory, which the agent's tools can change, or inside a git work tree, whose files come from its repository. Set `BAKE_HOME` to another directory, or run with `--no-mcp`.

bake-rs reads those directories and the file one step at a time without following links, so a link changed while it reads cannot point it elsewhere.

On Windows, bake-rs cannot yet check who may change the file, so it refuses an existing `mcp.json` there unread and starts no server from it; remove the file or use `--no-mcp` until that check is supported.

## The sandbox

Each `command` server starts under the session's sandbox mode (`bake-rs docs permissions`) and keeps that mode until it stops: under `workspace-write` it can change files only in the working directory and the temporary directories, and under `read-only` nowhere. Where no sandbox backend enforces the mode (Windows, for now), the server does not start; under `danger-full-access` it runs unconfined. Changing the permission later does not restart or reconfine a running server, and MCP tool calls are not approved one by one.

A `url` server starts no process: bake-rs itself connects to it. Like `web_search` and `web_fetch`, that connection is allowed in every sandbox mode, including `read-only`, because you authorized the endpoint by writing it in `mcp.json`; the sandbox has no network policy for it to bypass.

## What a url server may reach

`mcp.json` authorizes exactly the origin it names — the scheme, host, and port — and bake-rs holds the connection to it:

- The host is resolved once per connection and that address is used for every request of it, so a name that later answers with another address cannot move the connection.
- A loopback or private address is allowed, so a local server works; `http` is refused when the host resolves to an address that leaves this machine's networks, because the `headers` would cross a public network in clear text. Use `https` there.
- A redirect to another origin is refused; a redirect within the same origin is followed up to three times.
- `headers` are the only credentials sent. Their values are never printed in a warning, an error, or a log, and a server that answers with them in its own message has them removed from what bake-rs shows.
- Connections are made directly. `HTTPS_PROXY`, `HTTP_PROXY`, and `NO_PROXY` are not read: a proxy would resolve the host itself, so the address could not be pinned.
- A response or a server-sent event over 16 MiB is refused.

## A url server's connection

The server's tools are published once it has answered `initialize` and `tools/list`, as a `command` server's are, and every request after that carries the session the server assigned.

- When the server expires that session (HTTP 404), bake-rs connects again at once, lists the tools afresh, and gives the next prompt the new list. A prompt in flight sees the tools it started with.
- A lost connection is retried, waiting 500 ms and doubling to 30 seconds, and the server gives up after ten failures in a row; a connection that served for longer than 30 seconds starts that count again.
- A tool call interrupted by any of this fails with an error: bake-rs never sends a call twice, because the server may have run the first one already.
- A server that answers 401, or 403 asking for a wider scope, needs OAuth sign-in, which bake-rs does not support yet: it reports that and stops. Put a token in `headers` instead.
- Closing a session ends each HTTP session (`DELETE`) before bake-rs exits.

## Resources

A server that declares the `resources` capability makes its resources reachable through the three tools Codex and opencode use, so a model trained on them uses them unchanged:

- `list_mcp_resources` lists resources. Without arguments it lists every server with resources; with `server` it lists one page of one server, which `cursor` continues.
- `list_mcp_resource_templates` lists parameterized resources the same way. A server that does not implement the method has none.
- `read_mcp_resource` takes a `server` and a `uri` and returns the resource.

The three are one shared set: they appear as soon as a server with resources connects and disappear when the last one goes, and the session file logs that change like any other tool change, so a resumed session gives the model the same tools. A session whose servers offer no resources has none of them, and `--tools`, `--exclude-tools`, and `--no-tools` restrict them by name as they restrict every other tool.

Listings are JSON. Resources meant for a host's user interface (`ui://` URIs and `profile=mcp-app` media types) and icons are left out. A read becomes text and images for the model: an image is passed through, another binary resource is saved to a private file the result names, and text over 20 KB keeps its start and end, with the whole text in a file the result names so `read` can reach it. Resource reads do not ask for approval, as MCP tool calls do not, and a plugin that lists one of the three in its manifest vetoes its calls.

## Server instructions

A server may return `instructions` from its handshake, describing what it offers. bake-rs keeps at most 4 KB of them and gives the model the first line, up to 250 characters, in an `mcp_servers` section of the system prompt, under a line that says the text is the server's own and not an instruction to follow. A `description` in `mcp.json` replaces it, so you can say what a server is for in your own words. A session with no connected server that reports either sends no such section, and its requests are unchanged.

## Startup and problems

Servers start in the background as the session opens. The first prompt waits up to 10 seconds for servers still starting, so their tools are in its first request; a server that is slower, or a `url` server still reconnecting, adds its tools to a later prompt. Esc stops a running MCP call without stopping its server. When the session ends, bake-rs stops every server and the processes it started before it exits; in print mode Ctrl+C, `SIGTERM`, and `SIGHUP` do the same at any point, while startup waits for piped input or for servers, or while a prompt runs. `/new`, `/resume`, `/fork`, and `/clone` stop the current session's servers before the next session starts its own.

## Tool flags

`--tools`, `--exclude-tools`, and `--no-tools` restrict MCP tools as they restrict the built-in ones, including tools a server adds later. A tool they leave out is neither given to the model nor run if the model names it.

- `--tools` with names or `*` patterns gives the model only the MCP tools it matches, such as `--tools read,'mcp__files__*'`. A list of only `+name` and `-name` changes the built-in defaults and leaves MCP tools alone.
- `--exclude-tools` removes the MCP tools it matches.
- `--no-tools` alone gives the model no tool, so bake-rs starts no MCP server; with `--tools`, only the tools it names.

A problem with `mcp.json` or one of its entries is a warning when bake-rs starts; the other entries still start. In print mode, a server that could not start, failed, or is still connecting after the wait is also a warning before the answer. Interactive mode does not show a server's own failure yet.

`--no-mcp` reads no `mcp.json` and starts no server.

## Not supported yet

OAuth sign-in, reconnecting a `command` server that stopped (its tools are removed instead), resource subscriptions, prompts, code mode and tool search exposures, and a `/mcp` command are not available.
