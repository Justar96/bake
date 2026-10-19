# Code mode

The `codemode` tool lets the model write a JavaScript script that calls bake-rs's other tools. Only the script's output reaches the model, so a script can run calls in parallel and filter large results before the model sees them.

The tool is off by default. Turn it on for a run with `--tools`, for example `bake-rs -t +codemode` or `bake-rs -t read,bash,codemode`, or for every session with a `defaultTools` setting that names it (`bake-rs docs settings`). While it is off, requests are exactly what they are without it.

## Scripts

The tool input is raw JavaScript source, not JSON and not a Markdown code fence. It runs as the body of an async function in a QuickJS sandbox, so top-level `await` and `return` work. The sandbox has no Node APIs, file system, network, or timers; scripts reach the outside world only through tools.

A script may start with an options line. It must be the first line; an options line anywhere else fails the call before the script runs:

```js
// @options: {"max_output_tokens": 2000, "timeout_ms": 60000}
```

- `max_output_tokens` (default 10000) limits the output. Longer output keeps its start and end, and the full text is written to a temp file whose path is included in the result. A script fails when its output passes 16777216 characters of text and base64 image data or 100000 `text()`, `image()`, and `console` calls; write large data to a file with a tool instead.
- `timeout_ms` is a hard deadline for the whole script. It is unset by default.

The result starts with `Script completed` or `Script failed`, the wall time, and the output. Text and image items appear in order, each on its own line. When the output has more than one text item (from `text()` or `return`), each starts with a `==> text N/M <==` line. `console` calls follow in one `<console_output>` block with one line per call. A failed script keeps its partial output, followed by `Script error:` and the error. Tool calls are real: calls made before a failure are not undone. Calls still running when the script ends are cancelled, and unawaited promises are discarded. Esc stops the script and cancels its running calls.

## Globals

| Global | Purpose |
|---|---|
| `tools.<name>(args)` | Call a tool. See [Call tools](#call-tools). |
| `text(value)` | Add a text item to the output. Strings are added as is, other values as JSON. |
| `image(value)` | Add an image to the output: a base64 `data:` URL, an `{ image_url }` object, or an image block `{ type: "image", data, mimeType }` such as those returned by MCP tools and `tools.read()`. Remote URLs are not supported. PNG, JPEG, GIF, and WebP are accepted. Each image is also saved to a temp file, and the result names the path before the image. |
| `console.log(...)` | Add a line to the `<console_output>` block after the other output. Arguments are joined with spaces; `info`, `warn`, `error`, and `debug` do the same. |
| `return value` | A top-level `return` adds the value like `text()`. |
| `exit()` | End the script successfully. |
| `store(key, value)` / `load(key)` | Keep small JSON values across `codemode` calls. See [Store values](#store-values). |
| `ALL_TOOLS` | Every callable tool as `{ name, description }`, including tools the description does not list. |
| `searchTools(query, { limit?, namespace? })` | Rank callable tools by relevance (BM25, default limit 8). Resolves to `{ name, description }[]`. |
| `describeTool(name)` | Resolves to a tool's description and TypeScript declaration, or `undefined`. |
| `describeNamespace(name)` | Resolves to `{ name, description?, tools }` for a namespace such as an MCP server, or `undefined`. |

## Call tools

Every tool the session gives the model, other than `codemode` itself, is a method of `tools`, named by its identifier: characters that are not valid in a JavaScript identifier become `_`. Each method takes one object with the tool's arguments. A script's calls go through the same permissions, approvals, sandbox, plugins, and hooks as the model's own calls (`bake-rs docs permissions`), each under its own id, and they are recorded in the `codemode` result's details rather than as results of their own. At most 16 of a script's calls run at once; the others wait, in the order the script made them, and Esc cancels the waiting ones too.

What a call resolves to depends on the tool:

- Tools with an output schema resolve to a structured value. `bash` resolves to `{ output, truncated, full_output_path?, exit_code, wall_time_seconds }`, also for non-zero exit codes. Its `output` is not limited to the 2000 lines or 50KB the model sees: it holds up to 1 MiB, and longer output keeps its first and last 512 KiB around an omission marker, with `truncated` set and the full output in `full_output_path`.
- MCP tools resolve to their `CallToolResult`, including `isError` and `structuredContent`.
- `read` resolves to the file's text, or for an image to an image block `{ type: "image", data, mimeType, note }` that `image()` shows. `data` is the base64 image the model would see and `note` the text that goes with it, such as resize hints.
- Other tools, such as `edit` and `write`, resolve to their text output.

A call that fails, is blocked, is refused, or gets invalid arguments rejects with an `Error` that carries the tool's error text. Use `Promise.allSettled()` to keep the results of the calls that succeed.

Each MCP server is a namespace named `mcp__<server>`, with the server's name as an alias and its `description` from `mcp.json` (`bake-rs docs mcp`). Tools a server keeps hidden are not callable from scripts either.

While `codemode` is active, `codemode.mode` in [settings](#settings) decides how the other tools are presented. With `on` (default) declared tools stay declared, and their descriptions say what a script's call resolves to. With `only` they are left out of the model's requests and listed in the `codemode` description instead, so the model calls them through scripts. The session file still records them as declared. Tool declarations in the `codemode` description, `describeTool()`, and `ALL_TOOLS` carry the tools' guidelines, since the system prompt's rules only cover declared tools.

The `codemode` description lists tools with their TypeScript declarations, grouped by namespace. Every tool the model can call directly is declared to it already, so with `on` the description lists none; with `only` it lists the callable tools. Listed declarations share a budget of 3000 estimated tokens (`codemode.inlineBudget`). Scripts find the other tools with `searchTools()`, `describeTool()`, `describeNamespace()`, or by filtering `ALL_TOOLS`.

## Store values

`store(key, value)` keeps a JSON value under a string key for later `codemode` calls; storing `undefined` deletes the key. `load(key)` returns the value, or `undefined`. Writes are kept only when the script succeeds: each successful script that stores values appends a `codemode-store` custom entry to the session, so resumed sessions keep the values and each branch sees only the values written on its path, after `/tree` or a fork too. If the session cannot record the writes, the result keeps the script's output and ends with `[The codemode store could not be written: …]`.

The store is for small state such as IDs, cursors, or summaries. One value may have at most 262144 characters of JSON and all values together at most 1048576. Do not store image data; show images with `image()`, which also saves them to a temp file.

## Settings

| Member | Meaning |
|---|---|
| `codemode.mode` | `on` (default) keeps declared tools declared; `only` presents them only through `codemode` |
| `codemode.inlineBudget` | The estimated tokens the `codemode` description's tool declarations may take; 3000 when unset |

## Limits

- A script's VM has 256 MB of memory. Running out throws `InternalError: out of memory`; filter or aggregate large data instead of accumulating it.
- At most two scripts run at once in one bake-rs process. A cancelled script occupies its slot until its engine stops. At capacity, new scripts fail immediately; retry after an earlier one ends.
- A script may have at most 1024 calls waiting for an answer, whose arguments together take at most 64 MiB. Passing either fails the script; await calls before making more, or batch them.
- A script that waits on a promise that can never settle (no tool call pending) fails immediately, since there are no timers.
- Scripts cannot start other `codemode` scripts.
