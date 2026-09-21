# TUI wiring reference

The TUI owns one terminal and one root Agent. [Application usage](packages/app/README.md), [presentation](packages/ui/README.md), and [dependency references](DEPENDENCIES.md) describe its supported behavior.

## 1. Topology

The profile combines `dsh-base`, a disabled headless runner, and the TUI patch. `tui-startup` parses flags with Commander through `parseCmdline`; its `tuiStartup` service supplies the runner's lazy configuration. The patch also mounts the `standard` preset roster and its required subagent model-selection settings. The launcher provides `cmdlineArgs` and `appExit`.

## 2. Boot sequence

The runner registers its disposal effect before awaiting Loader settlement. It then resolves the workspace and model selection, creates or resumes the Agent, connects observers in the registry's `setup` callback, replays history, and renders Ink. Failure before rendering leaves the terminal untouched; failure afterward reaches the same release function as ordinary exit.

A fresh session records `cwd` and the resolved `agentPreset` in its header. Resume calls `agents.resume` with the exact requested id. It refuses a missing session, a live owner, a different workspace, or a conflicting explicit preset. The `agentPreset` projection determines resumed composition; a session without a recorded preset requires an explicit `--preset`, which is recorded after successful mounting.

## 3. Service wiring

| Action | Harness owner |
|---|---|
| Create and resume | `agents.create` / `agents.resume`, returning an owned `AgentHandle` |
| Model selection | `agentDefaultModel.currentSelection` and `installModelSelection` |
| Preset composition | `agentPresets.resolve` / `mount` and the `agentPreset` projection |
| History | `sessionQuery.observeSession` plus `session/event` |
| New prompt / steering | `agent.followup` / `agent.steer`, selected from current `agent.status` |
| Slash input | `parseCommand` and `commands.execute`, including a session-scoped `/login` registration |
| Tool approval | `approval/request` waterfall |
| Questions and plan review | `user-questions/request` waterfall |
| Interrupt | `agent.cancel({ kind: 'user' }, { keepInbox: true })` |
| Shutdown | `AgentHandle.dispose`, which drains the driver and persistence |

## 3b. One authority per fact

| Displayed fact | Authority |
|---|---|
| Activity | `agent.status`, notified by `agent/status` |
| Pending input | `sessionProjections.stateOf(session, 'inbox')`, notified by `onChanged` |
| Committed transcript | Session events projected into immutable rows |
| Live response | Ordered `agent/assistant-stream` frames for the active attempt |
| Human request | The oldest outstanding scoped interaction and its abort signal |

The TUI stores only presentation state: draft text, transient output, the displayed request, command activity, a requested interruption, and application notices. It does not fold turn boundaries, inbox state, tokens, or context pressure independently. Stopping ends when the Agent reports idle.

## 4. Transcript flow

The controller subscribes before Agent publication and buffers events while obtaining a query observation. The observation's events and the buffered tail pass through one sequence cursor, so history/live overlap cannot duplicate committed rows. Notices from application setup stay outside the durable conversation.

Committed rows render through Ink `Static`. Assistant chunks render separately until the corresponding durable message or attempt settles. Frame revisions increase across publications; attempt identity and increasing revision order determine which chunks belong to the visible response. Text and reasoning come from the harness `BlockAssembler`.

### 4.3. The rewrite problem

Compaction replacements do not append duplicate tool rows. Terminal scrollback retains the original output, and a localized compaction notice marks the context change. The transcript displays historical output; it does not claim to reproduce the model's current compacted context.

## 5. Input flow

Ink `usePaste` owns bracketed-paste decoding and mode changes. Paste only inserts text, including line breaks. The independent `useInput` handler interprets Enter, Escape, and Ctrl-C; it also handles ordinary text and Enter delivered in one read. A shared composer keeps same-read edits available before the next React paint. Backspace removes one Unicode grapheme.

Enter submits a non-empty prompt or registered command. While an Agent runs, ordinary text steers its next step. The inbox projection displays queued human input until the harness consumes it. One command runs at a time, with visible feedback for a competing command.

Escape cancels the displayed interaction, otherwise the active command, otherwise the Agent. Agent interruption retains the inbox. Ctrl-C displays a quit hint; another Ctrl-C within the configured interval quits. The application owns that timer; presentation components have no clock.

## 6. Human decisions

The interaction queue accepts requests only for the exact owned Agent and calls `next()` for other Agents. Requests remain ordered; one cannot replace another. Abort, explicit cancellation, or disposal settles the affected promise and removes its panel. An answer contains the displayed request id, preventing stale callbacks from answering a later request.

Approval displays the tool, call id when supplied, and reason. Typed Y grants once, N rejects, and Escape cancels. Pasted Y cannot grant permission. Structured questions display every option and the complete detail, including plan Markdown. Numbered answers resolve to the exact option labels; written answers use the service's custom-answer field. Plan approval follows the named option, without assuming its position.

## 6b. Provider login

`/login` is a session-scoped harness command. Configured credential references are described through `credentials`; interactive flows come from `authorization.list`. Secret prompts are masked and their values never enter command arguments, Session events, or model input. Key writes use `credentials.set`; authorization flows receive the command's abort signal. The existing credentials provider owns storage and write restrictions.

## 7. Teardown

One idempotent `releaseTerminal` closes observers and human requests, clears the quit timer, and calls Ink `cleanup`. It runs before asynchronous Agent disposal. The same Cordis effect handles plugin unmount and the launcher's `installFailLoud` release hook. `waitUntilExit` propagates renderer errors instead of leaving the runner waiting for keyboard input.

Ink owns raw mode, bracketed paste, and cursor restoration. The application does not issue a second manual paste-mode lease. Node integration tests exercise ordinary quit and context disposal; the PTY smoke additionally checks actual terminal attributes, exit status, and paste-mode release.

## 8. Configuration

| Field | Default | Meaning |
|---|---|---|
| `resume` | absent | Exact persisted session id |
| `preset` | roster default for a fresh session | Fresh composition, or explicit legacy-session composition |
| `locale` | `en` | `en` or `zh` labels |
| `doubleInterruptMs` | `500` | Interval for a second Ctrl-C to quit |
| `credentialRefs` | `[]` | Provider key references offered by `/login`; the supplied patch names `DEEPSEEK_API_KEY` |

Schemastery validates and defaults configuration before the runner receives it. Locale dictionaries own application text; model output, tool results, and provider-owned diagnostics remain verbatim.

## 9. Validation

[The check script](scripts/check.sh) runs strict application/test TypeScript checks, pure Bun tests, and Node component/integration tests. [The PTY smoke](scripts/pty-smoke.py) launches the built profile in a private workspace and home, replays the shared recorded bash scenario, compares persisted model and real tool output, resumes the exact session, and checks terminal restoration. `--live` uses the root `.env` for a real DeepSeek call.

## 10. Limits

One session owns the process. Inline scrollback has no session switcher or virtualized transcript. The component development loop, attachment UI, model picker, and long-history performance qualification remain separate work. Source launch is unsuitable for qualifying tool execution on this checkout; use the built profile and the runbook in [PLAN.md](PLAN.md#132-build-from-a-clean-checkout).
