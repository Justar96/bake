# Scripts and other agents

`bake -p` runs prompts without a terminal and exits, so a script, a CI job, or another coding agent can hand Bake a task. With `--output-format json` it prints one JSON object: the answer, how the run ended, the session to continue, and what the run used. The flags follow the headless mode of other agent command lines, such as Claude Code's and Gemini CLI's `-p` and `--output-format`.

## Running a task

```sh
bake -p --output-format json "Fix the failing test in src/parse.rs" < /dev/null
git diff | bake -p --output-format json "Review this change"
bake -p --output-format json --session <sessionId> "Now add a test for it" < /dev/null
```

- `-p` runs the prompts and exits. Print mode also starts when `--output-format` or `--mode` is given, or when standard input or output is not a terminal.
- Read `result` for the answer and `isError` for whether to trust it. The exit status agrees: 0 only for a complete answer.
- Continue the conversation with `--session <sessionId>` (or the `sessionFile` path), or name it up front with `--session-id <id>`, which opens the session with that id or starts one with it. `-c` continues the latest session in the working directory. `--no-session` saves nothing. A continued session keeps its model, thinking level, and permission unless a flag changes them.
- Each message argument is its own prompt, sent after the previous one finishes; the result describes the last. `@file` arguments attach files to the first prompt.

## Output formats

| `--output-format` | Standard output |
|---|---|
| `text` | The final answer's text, as `-p` prints it |
| `json` | One line: the result object |
| `stream-json` | The session header and every event as JSON lines, as `--mode json` writes them, then the result object as the last line |

`--output-format` and `--mode` choose the same thing, so give one of them. `--mode json` is Pi's event stream and ends without the result object; `--mode rpc` is a long-lived connection that takes prompts and answers approvals over standard input (`bake docs rpc`). Warnings and errors go to standard error in every format.

## The result object

```json
{"type":"result","subtype":"success","isError":false,"result":"The test now passes.","error":null,"stopReason":"stop","sessionId":"0194c3…","sessionFile":"/home/me/.bake/sessions/…/2026-…_0194c3….jsonl","runId":null,"model":"cliproxyapi/gpt-5.5","numTurns":3,"modelCalls":3,"durationMs":41250,"usage":{"input":51200,"output":2100,"cacheRead":180300,"cacheWrite":0,"totalTokens":233600},"costUsd":0.0412}
```

Every member is present; one that does not apply is `null`.

| Member | Meaning |
|---|---|
| `subtype` | `success`; `error`, a model or runtime failure; `truncated`, an answer cut off at the model's output limit; `budget_exhausted`, stopped by `--max-model-calls` or `--run-timeout-ms`; or `cancelled` |
| `isError` | `false` only for `success` |
| `result` | The final answer's text, also after a failure that left one |
| `error` | What went wrong, as standard error got it |
| `stopReason` | Why the last answer ended: `stop`, `length`, `toolUse`, `error`, or `aborted` |
| `sessionId`, `sessionFile` | The session to continue; `sessionFile` is `null` under `--no-session` |
| `runId` | A `--recoverable` run's id, for `--resume-run` (`bake docs sessions`) |
| `model` | The model as `provider/id`, which `--model` accepts |
| `numTurns`, `modelCalls` | The agent's turns, and the model requests admitted, retries and summaries included |
| `durationMs` | From the first prompt to the end of the run |
| `usage`, `costUsd` | The tokens this invocation used, a continued session's earlier ones left out, and their cost in dollars from the model's prices (0 when it has none) |

A failure during startup, such as a model that does not exist, a session that cannot be found, or no usable API key, still prints a result object, with `subtype` `error`, the errors in `error`, and no session. An argument Bake cannot parse prints only its error, and a run stopped by a signal prints no result.

## Standard input

- When standard input is not a terminal and the command line gives no message, `@file`, or `--resume-run`, it is the prompt: Bake reads it to its end however long that takes, up to 64 MiB.
- Beside a message or `@file`, piped input is read when it sends something within 1 second, and then to its end. A pipe that stays silent, as one a caller opened and never writes does, is left unread with a warning, so the run does not wait forever.
- `-` reads standard input to its end in every case, a terminal included, so `slow-command | bake -p - "Explain this output"` waits for the slow command. Redirect from `/dev/null` (`NUL` on Windows) to skip the read.
- The input is trimmed and joined to the first message with no separator; start the message with a blank line to keep them apart.

## Exit status

| Status | Meaning |
|---|---|
| 0 | The prompts ran and the last answer is complete |
| 1 | An invalid flag, a startup failure, a model or runtime failure, an answer cut off at the output limit, or an exhausted `--max-model-calls` or `--run-timeout-ms` |
| 2 | An argument that is not UTF-8 |
| 129, 130, 143 | Stopped by `SIGHUP`, `SIGINT`, or `SIGTERM`, after the session and the processes its tools started were closed |

## Permissions and limits

- Print mode cannot ask for approval, so a call that needs wider access is refused. Choose the access before the run: `--sandbox read-only` for review and questions, the default `workspace-write` to change files in the project, and `--permission danger-full-access` only where nothing outside the task can be harmed (`bake docs permissions`).
- `--max-model-calls <n>` and `--run-timeout-ms <n>` bound a run's requests and time; either one stops the run with `budget_exhausted`.
- `--tools`, `--no-mcp`, `--no-context-files`, and `--no-skills` limit what the run can use and read (`bake docs cli`).
- After the last prompt, Bake waits up to 10 minutes for background jobs and subagent runs it started and for the turns their results open, so the answer printed is the last one; it then stops what is still running and says so on standard error.
