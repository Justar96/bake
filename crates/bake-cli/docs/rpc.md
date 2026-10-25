# RPC mode

`bake --mode rpc` runs one long-lived session controlled by JSON records on standard input and output. Use it to embed the agent in an editor, another language, or a user interface of your own. It speaks Pi's RPC protocol, so a client written for Pi works, with the differences this page lists.

```sh
bake --mode rpc --no-session
```

The other command-line options still select the working directory's session, the model, the tools, the resources, and the permission: `--provider`, `--model`, `--session`, `--session-dir`, `--no-session`, `--name`, `--permission`, `--sandbox`, `--approval`, `--no-mcp`, and the rest of `bake --help`. Message arguments and piped input are not prompts in this mode: standard input carries the protocol, and prompts arrive as `prompt` commands. `@file` arguments are refused. A session still needs a usable model, as `-p` does; without one the startup fails with status 1.

## Records and framing

Four kinds of record cross the pipes:

| Direction | Record | Purpose |
|---|---|---|
| stdin | Command | Prompt, inspect state, change settings, or manage the session |
| stdout | `response` | Whether one command succeeded, with its data or its error |
| stdout | Session event | The same events `--mode json` writes |
| Both | `extension_ui_request` and `extension_ui_response` | Approval questions and their answers |

Framing is strict JSONL: one complete JSON object per record, terminated by a line feed. Split stdout on `\n` only. A line reader that also splits on U+2028 or U+2029 will break records, because those characters are valid inside JSON strings. An optional carriage return before the line feed is accepted on input. Standard output carries protocol records only; warnings and diagnostics go to standard error, and are never records.

Every command takes an optional string `id`, and its response repeats it:

```json
{"id":"req-1","type":"get_state"}
{"id":"req-1","type":"response","command":"get_state","success":true,"data":{"sessionId":"...","thinkingLevel":"off","isStreaming":false,"messageCount":0}}
```

Each command is handled as it arrives, so a long command delays no other: `abort` reaches a running `compact`, prompt, or branch summary, and `get_state`, `get_messages`, and the other inspection commands answer meanwhile. Responses therefore arrive in the order the commands finish, not the order they were sent; correlate them by `id`. What may run together is the session's own rule: a `prompt` during a compaction is refused with the reason to wait for it, a `compact` while a turn runs stops that turn first, and settings changes are applied one at a time. Each record is written whole, so concurrent commands never interleave bytes within a line. Read standard output continuously: at most one chunk waits behind the writer, so a client that stops reading slows the session down.

## Prompting

A `prompt` response reports what happened to the prompt, not the end of the model's work:

```json
{"id":"req-2","type":"prompt","message":"Review this repository"}
{"id":"req-2","type":"response","command":"prompt","success":true,"data":{"disposition":"started"}}
```

- `started`: a turn is running. Follow the events, and wait for `agent_settled` to learn that nothing more follows automatically.
- `queued`: the prompt waits for the running turn. A `prompt` with `streamingBehavior` set to `steer` or `followUp` queues while a turn runs, as `steer` and `follow_up` always do.
- A `prompt` sent while a turn runs without `streamingBehavior` is refused, with the reason to pass one.

Pi's third disposition, `handled`, reports an extension consuming the prompt; `bake` has no extensions, so it never answers `handled`. `/skill:<name>` prompts and prompt templates are expanded as everywhere else and start a turn.

## Commands

The commands `bake` serves, with the data of a successful response:

| Command | Members | Data |
|---|---|---|
| `prompt` | `message`, `images`, `streamingBehavior` | `{disposition}` |
| `steer`, `follow_up` | `message`, `images` | `{disposition}` |
| `abort` | | none |
| `clear_queue` | | `{steering, followUp}` |
| `get_state` | | the session state |
| `get_messages` | | `{messages}` |
| `set_model` | `provider`, `modelId` | the model |
| `cycle_model` | | `{model, thinkingLevel, isScoped}`, or `null` |
| `get_available_models` | | `{models}` |
| `set_thinking_level` | `level` | none |
| `cycle_thinking_level` | | `{level}`, or `null` |
| `get_available_thinking_levels` | | `{levels}` |
| `set_steering_mode`, `set_follow_up_mode` | `mode` (`all` or `one-at-a-time`) | none |
| `compact` | `customInstructions` | the compaction result |
| `set_auto_compaction` | `enabled` | none |
| `abort_retry` | | none |
| `get_session_stats` | | the counts, tokens, and cost |
| `export_html` | `outputPath` | `{path}` |
| `new_session` | | `{cancelled}` |
| `switch_session` | `sessionPath` | `{cancelled}` |
| `fork` | `entryId` | `{text, cancelled}` |
| `clone` | | `{cancelled}` |
| `get_fork_messages` | | `{messages}` |
| `get_entries` | `since` | `{entries, leafId}` |
| `get_tree` | | `{tree, leafId}` |
| `get_last_assistant_text` | | `{text}`, `null` when there is none |
| `set_session_name` | `name` | none |
| `get_commands` | | `{commands}` |

`new_session`, `switch_session`, `fork`, and `clone` replace the session, one replacement at a time: a question the running session had put to the client is withdrawn and recorded as cancelled, its turn is stopped, and it is shut down with its MCP servers and background work; the new session's events then arrive on the same stream, and its tools ask the same client. Its setup warnings go to standard error. `cycle_model` steps through the models with a configured key, and reports `isScoped: false`, since a session keeps no `--models` scope to cycle. `get_commands` lists the prompt templates and skills a prompt can invoke, each with a `sourceInfo` whose `origin` is always `top-level`.

These Pi commands are refused with a reason, because `bake` has no equivalent:

- `bash` and `abort_bash`: there is no shell execution outside the model's `bash` tool.
- `set_auto_retry`: automatic retry follows `settings.json`'s `retry.enabled`, which no command changes.
- `new_session` with `parentSession`: use `fork` or `clone`.
- Pi's extension commands, extension UI methods other than the approval question below, and its theme commands do not exist here; their records are unknown commands.

## Approvals

Under `--approval ask`, a tool call that asks for wider access than the session's sandbox allows is put to the client as Pi's `extension_ui_request` with the `confirm` method, so a Pi client can answer it:

```json
{"type":"extension_ui_request","id":"approval-1-6b1f2c54-9f3a-4d27-8f41-0c6d9a2b7e55","method":"confirm","title":"Approve this tool call?","message":"bash (call call_1) requests: escalate sandbox to danger-full-access: install the dependency","approval":{"toolName":"bash","callId":"call_1","reason":"escalate sandbox to danger-full-access: install the dependency","arguments":{"command":"npm install"}}}
```

`approval` is Bake's addition, so a client can show what would run; a Pi client ignores it. Answer with the request's `id`, which is fresh for each question and not to be guessed or constructed:

```json
{"type":"extension_ui_response","id":"approval-1-6b1f2c54-9f3a-4d27-8f41-0c6d9a2b7e55","confirmed":true}
```

- `confirmed: true` allows that one call, with the access it asked for. Nothing else is granted: no standing permission, preset, or later call changes, and a later call asks again.
- `confirmed: false`, or a response with neither member, rejects the call.
- `cancelled: true` withdraws the question; the call is refused.

Requests are decided oldest first, and only the oldest is announced. An answer for a request already decided or withdrawn settles nothing. Every request and its answer are written to the session file before the call runs, exactly as in an interactive session. A client that disconnects or never answers leaves the call refused: the pending question is cancelled when the session stops, and the call runs nothing. See [Permissions](permissions.md#approvals).

## Errors

A failed command answers once, with the reason:

```json
{"id":"req-3","type":"response","command":"set_model","success":false,"error":"Model not found: openai/none"}
```

A line that is not JSON answers as a `parse` failure without an id, and a record whose `type` names no command answers `Unknown command: <type>`. A record whose member cannot be read answers that command with the reason it could not be read. An `id` that is not a string is a `parse` failure too, and its command does not run, since the response could not repeat the id. A successful response covers the command only: a provider failure or an abort after a prompt was accepted appears in the events.

## Shutdown

Close the child's standard input to shut down: the pending approval question is cancelled, the running turn is stopped, the session is shut down with its MCP servers, background shells, and plugin helpers, their process trees are awaited, and `bake` exits 0. `SIGTERM` exits 143, `SIGHUP` 129, and `SIGINT` 130, each after the same shutdown. A long command in flight does not delay the shutdown: the stop request is taken up at once, the command is stopped with the session, and it answers with its own failure, such as `Compaction cancelled`, before the process exits. Handle the child's unexpected exit too: a request in flight when the child exits gets no response.
