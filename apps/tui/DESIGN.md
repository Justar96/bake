# TUI wiring reference

The TUI owns one terminal and displays one root Agent at a time. [Application usage](packages/app/README.md), [presentation](packages/ui/README.md), and [dependency references](DEPENDENCIES.md) describe its supported behavior.

## 1. Topology

The shipped `tui` profile combines `dsh-base` and `@dsh-tui/app`. `tui-startup` parses flags with Commander through `parseCmdline`; its `tuiStartup` service supplies the runner's lazy configuration. The patch also mounts the `standard` preset roster and its required subagent model-selection settings. The launcher provides `cmdlineArgs` and `appExit`.

## 2. Boot sequence

The runner registers its disposal effect before awaiting Loader settlement. It then resolves the workspace and model selection, creates or resumes the Agent, connects observers in the registry's `setup` callback, replays history, and renders Ink. The runner requires TTY input and output and explicitly enables interactive rendering, including when CI variables are present. Failure before rendering leaves the terminal untouched; failure afterward reaches the same release function as ordinary exit.

A fresh session records `cwd` and the resolved `agentPreset` in its header. Resume calls `agents.resume` with the exact requested id. It refuses a missing session, a live owner, a different workspace, or a conflicting explicit preset. The `agentPreset` projection determines resumed composition; a session without a recorded preset requires an explicit `--preset`, which is recorded after successful mounting.

## 3. Service wiring

| Action | Harness owner |
|---|---|
| Session navigation | `sessionQuery.filterSessions` and `readTitleSnapshots`; `navigation.ts` owns the displayed handle |
| Create and resume | `agents.create` / `agents.resume`, returning an owned `AgentHandle` |
| Model selection | `llm.listProviders` / `listModels` / `resolveModelInfo`, `agentDefaultModel.currentSelection`, and `installModelSelection`; resume reads the last `session.requestHeader()` |
| Preset composition | `agentPresets.resolve` / `mount` and the `agentPreset` projection |
| History | `sessionQuery.observeSession` plus `session/event` |
| New prompt / steering | `agent.followup` / `agent.steer`, selected from current `agent.status` |
| Slash input | `parseCommand`, `commands.find` / `execute`; unregistered names remain ordinary user input |
| Slash discovery | `commands.list(agent)` and the preset-owned `skills.snapshot({ cwd, scope: agent, signal })`, filtered with `isUserInvocable` |
| File discovery | `agent.ctx.fileReferences.list(agent, query, signal)`, supplied by the composed `file-reference-local` provider |
| Tool approval | `approval/request` waterfall |
| Questions and plan review | `user-questions/request` waterfall |
| Discard queued human input | `agent.inbox.remove(id)` for both pending targets |
| Interrupt | `agent.cancel({ kind: 'user' }, { keepInbox: true })` |
| Shutdown | `AgentHandle.dispose`, which drains the driver and persistence |

## 3b. One authority per fact

| Displayed fact | Authority |
|---|---|
| Activity | `agent.status`, notified by `agent/status` |
| Pending input | `sessionProjections.stateOf(session, 'inbox')`, notified by `onChanged` |
| Context occupancy | `sessionProjections.snapshot(session, ['contextPressure']).values.contextPressure.projectedTokens` and `contextWindow`, notified by `onChanged` |
| Committed transcript | Session events projected into immutable rows |
| Task list | `sessionProjections.stateOf(session, 'todos')`, notified by `onChanged` |
| Plan mode | `sessionProjections.snapshot(session, ['contextPressure', 'plan', 'tokenUsage', 'permissions']).values.plan`, notified by `onChanged` |
| Permission mode | `values.permissions.currentValue` from the same snapshot, notified by `onChanged`; child inspection reads the child's live projection or saved observation |
| Thinking level | Agent-scoped `ModelSelectionRef.current.reasoningEffort`, or the selected route's `llm.resolveModelInfo().reasoning.defaultEffort`; unsupported models omit it. A child uses its own request header. |
| Billed tokens | `values.tokenUsage` from the same snapshot: input is `uncachedInputTokens + cacheReadTokens + cacheWriteTokens`, the cache hit is `cacheReadTokens` over it, and a cache field appears only when either cache bucket is non-zero |
| Tool presentation | `tools.get(name)` and the call's own `presentCall` / `presentResult` |
| Live response | Ordered `agent/assistant-stream` frames for the active attempt |
| Human request | The oldest outstanding scoped interaction and its abort signal |

The TUI stores only presentation state: draft text, transient output, the displayed request, command activity, a requested interruption, and application notices. It does not fold turn boundaries, inbox state, tokens, or context pressure independently. Stopping ends when the Agent reports idle. The context figure includes projected growth and compaction after the last provider usage sample; `~` marks it as an estimate. Internal `stateOf` values do not expose the public `projectedTokens` field.

## 4. Transcript flow

The controller subscribes before Agent publication and buffers events while obtaining a query observation. The observation's events and the buffered tail pass through one sequence cursor, so history/live overlap cannot duplicate committed rows. Notices from application setup stay outside the durable conversation.

Committed rows render through Ink `Static`. Assistant chunks render separately until the corresponding durable message or attempt settles. Frame revisions increase across publications; attempt identity and increasing revision order determine which chunks belong to the visible response. Text and reasoning come from the harness `BlockAssembler`.

### 4.3. The rewrite problem

Compaction replacements do not append duplicate tool rows. Terminal scrollback retains the original output, and a localized compaction notice marks the context change. The transcript displays historical output; it does not claim to reproduce the model's current compacted context.

`project` drops any event whose `surfaceOp` is not `append`, for every event type rather than tool results alone, so one rule covers the whole vocabulary instead of a per-case list that later event types escape silently.

### 4.4. One projection

Every event with a terminal presentation is projected by `project`, including the ones this surface words itself: a command echo, a cancelled turn, and a compaction notice. The localized dictionary is a parameter for that reason. A caller that worded those rows where it happened to hold a dictionary would keep them out of the recorded fixtures and out of the Bun component loop, which replay through `project` alone.

### 4.5. Tool cards

A tool says how one of its calls reads by declaring `presentCall` / `presentResult` in `dsh-tools`; each returns a `card`-tagged intent that names no surface. `cards.ts` is the terminal's half of that seam, and the only place a `card` becomes display lines: a command and its description for `terminal`, the changed span between a hunk's shared context for `diff`, numbered lines for `read`, matches grouped by file for `search`, and a status and url for `web`.

The registry is the lookup, so a tool contributed by any plugin presents its own calls without this surface knowing it exists. A tool that declares no presenter, one the registry no longer knows, a card kind newer than this build, and a presenter that throws all fall back to the raw arguments and result text, which is the presentation every tool had before the seam existed. Because the presenters are pure over the arguments and the durable result — including the `meta` the log persists for exactly this — a replayed session reproduces the identical card.

A capped search reports its total and says it was capped. A card that quietly listed the matches it retained would read as a complete result.

### 4.6. The task list

`todo/write` replaces the whole list, so the transcript would carry the same plan several times with a different tick each time. The `todos` projection folds the writes to the one version still true, and a panel above the chrome shows it. The panel is a checklist: a heading with a progress bar and a count, then the tasks in the agent's order, indented, each with a box that is ticked, pointed at, or empty. It deliberately does not take the branch shape of a step's calls and the subagent panel: those are things that ran or are running, and a plan is a list of things to do, so the two panels beside each other must not read as one kind. When rows run short, finished entries give up theirs first: they are what the reader already watched happen. Each task is one truncated row, and the panel is capped like every other, because the dynamic region shares one budget.

### 4.7. Measured transcript cost

Committed history uses immutable linked batches. Appending a batch shares the preceding snapshot without reading or copying its rows. A memoized renderer reads only the unprinted suffix and passes it to Ink `Static`; streaming and composer updates leave committed rows untouched. Initial replay still reads the complete history, and retained memory grows with transcript size.

`packages/ui/tests/scale.spec.tsx` counts history reads at 50 and 10,000 rows, checks row order and single emission across coalesced appends, and compares terminal bytes at 50 and 2000 rows. Each measurement waits for Ink’s render flush on paired fake terminal streams. These checks cover row processing and terminal output, not whole-process latency or memory bounds.

The [whole-process diagnostic](packages/app/performance/README.md) uses the shared Bun production build and native PTY to measure Node profile readiness, input echoes, streaming, retained heap, and peak RSS with fixed synthetic histories. Its large-history workload fails under the diagnostic heap constraint; the measurement card and layout follow-up remain with that owner.

## 5. Input flow

Ink `usePaste` owns bracketed-paste decoding and mode changes. Paste inserts text at the cursor, including line breaks, without submitting. The independent `useInput` handler interprets Enter, Escape, and Ctrl-C; it also handles ordinary text and Enter delivered in one read. A shared composer keeps same-read edits available before the next React paint. Cursor movement and deletion preserve Unicode graphemes; Home/End and Ctrl-A/E use logical-line boundaries. Shift-Enter inserts a newline, and Enter submits the entire draft.

History recall lazily traverses the current pending human input and immutable committed user rows, newest first. A browse visit retains its starting transcript snapshot, the unsent draft and cursor, and local edits to visited entries. Returning past the newest entry restores that draft; submitting clears the visit. No separate durable history is stored, and ordinary typing does not traverse the transcript. Up/Down browses when completion is closed; Ctrl-P/N recalls through an open menu. Recall dismisses completion until the text or cursor changes. Questions and login prompts receive no history source.

A leading slash token opens the command and skill menu while the cursor is in that token. Up/Down selects a row; Tab replaces the token without submitting and preserves subsequent arguments. Command registrations win name collisions. Discovery observes `commands/change` and `skills/change`; cancellation prevents stale reads from replacing current entries. Metadata reads never load skill bodies. Unregistered slash names pass unchanged to the Harness’s ordinary input path; `tool-skill` owns recognition, instruction injection, and durable recording.

An active `@` token at the cursor opens path discovery through `references.ts`. The Harness grammar detects tokens and formats mentions, including quoted spaces and directory suffixes. Tab replaces the entire token while preserving surrounding text and reusing an existing separator. Quoted directory completion leaves the cursor before the closing quote to keep discovery open; moving past that quote closes the menu. Query-tagged results prevent stale insertion, and changing or closing a query aborts its read. The provider owns cwd, exclusions, ranking, limits, and path-only model guidance; the TUI never reads file contents for completion.

Enter submits a non-empty prompt or registered command. While an Agent runs, ordinary text steers its next step. The inbox projection displays queued human input until the harness consumes it. One command runs at a time, with visible feedback for a competing command.

Escape closes an open completion menu, otherwise cancels the displayed interaction, active command, or Agent in that order. Agent interruption retains the inbox. The pending panel offers `/clear-pending`, which removes only queued human messages through `agent.inbox.remove`; plugin context remains untouched. Command results and removals use the existing durable Harness events, so discarded input stays discarded after resume. Ctrl-C displays a quit hint; another Ctrl-C within the configured interval quits. The application owns that timer; presentation components have no clock.

## 6. Human decisions

The interaction queue accepts requests only for the exact owned Agent and calls `next()` for other Agents. Requests remain ordered; one cannot replace another. Abort, explicit cancellation, or disposal settles the affected promise and removes its panel. An answer contains the displayed request id, preventing stale callbacks from answering a later request.

Approval displays the tool, call id when supplied, and reason. Typed Y grants once, N rejects, and Escape cancels. Pasted Y cannot grant permission. Structured questions display every option and the complete detail, including plan Markdown. Numbered answers resolve to the exact option labels; written answers use the service's custom-answer field. Plan approval follows the named option, without assuming its position.

## 6a. Model selection

`/model` offers a filterable route picker and, when declared by that model, an effort picker. `model.ts` reads advisory provider catalogs and resolves exact capabilities only for selected routes. Failed catalogs produce a visible warning without hiding other providers or the current route. Effort values and labels come from the adapter; omitting an effort preserves provider-default behavior. Both pickers share the abortable interaction queue and configured row limit.

The controller changes `ModelSelectionRef.current` only after acceptance and final capability validation. Agent activity aborts the pending selection, preventing a lookup started while idle from committing during a turn. The Harness records the selected route and effort in request headers and supplies model-switch notices. Resume restores the last requested selection and uses `adapterDefaults.reasoningEffort` to distinguish implicit defaults from explicit choices. Unused choices are not durable and do not change profile settings.

The subagent branch panel derives identities from `subagents.listChildren` and activity from live Agent status and scoped subagent run events. Ctrl+G and `/agents` open a filterable picker. A selected child is observed through `sessionQuery.observeSession`, with a sequence-fenced buffered tail and live assistant frames. Inspection owns only observers: it neither resumes nor disposes the child. Escape returns to the mounted parent composer, and parent interactions take focus. Remote runs without local transcripts and unreadable catalog entries report their unavailable state.

## 6b. Session navigation

`/sessions` and `/resume` share one navigation flow. The command finishes its Harness lifecycle before `navigation.ts` lists current-workspace sessions and reads their log-backed titles. The filterable picker pins the current session above saved history, sorts that history newest first, places the new-session action last, omits subagents and other live Agent owners, and keeps ids usable when title reads fail. New sessions use profile defaults; resumed sessions use the existing exact-id workspace, preset, and last-request configuration checks. Legacy preset selection remains a startup operation.

Navigation requires the displayed Agent to be idle with both inbox targets empty, including plugin input. Status and inbox changes abort preparation, and the application refuses composer submissions until navigation settles. The previous controller stays displayed while the replacement mounts and replays; failure or cancellation disposes the candidate. Handoff changes the displayed controller synchronously, then closes and drains the previous controller and handle. Escape cancels preparation before handoff; handle retirement completes once handoff is accepted. Preset mounting has no cancellation parameter, so rollback waits for that work to settle before returning.

The session id keys presentation lifetime. Switching resets the draft, cursor, completion, and recall visit and prints the selected history beneath a localized session heading. Existing terminal scrollback remains, with one history replay per visit. No TUI session store or model-request lifecycle is introduced.

## 6c. Provider login

`/login` is a session-scoped harness command. Configured credential references are described through `credentials`; interactive flows come from `authorization.list`. Secret prompts are masked and their values never enter command arguments, Session events, or model input. Key writes use `credentials.set`; authorization flows receive the command's abort signal. The existing credentials provider owns storage and write restrictions.

## 6d. Attachment input

The attachment controls and supported formats are documented in the [application README](packages/app/README.md#use-this-package). `AttachmentDraft` owns only bounded, unsubmitted source bytes. Scoped Harness filesystem reads resolve paths; the attachment store validates and admits image batches and saves generic files. The selected Harness model reference supplies image capability lookup and is rechecked before inbox submission. No attachment receipts or persistence formats are introduced.

The controller serializes attachment operations, drains storage calls that have no abort parameter, and checks cancellation before submitting. The Agent's current status chooses `steer` or `followup`; accepted inbox input clears the draft. The UI receives metadata and an acceptance result, retaining text and cursor on refusal. Staged bytes block session navigation and are released during controller closure.

## 7. Teardown

One idempotent `releaseTerminal` cancels navigation and closes observers and human requests, aborts command, skill, and file discovery, clears the quit timer, and calls Ink `cleanup`. It runs before asynchronous Agent disposal. The same Cordis effect handles plugin unmount and the launcher's `installFailLoud` release hook. `waitUntilExit` propagates renderer errors instead of leaving the runner waiting for keyboard input.

Model catalog reads use the upstream `listModels` API, which has no cancellation parameter. Canceled commands suppress later UI updates and drain an active catalog call after terminal release.

Ink owns raw mode, bracketed paste, and cursor restoration. The application does not issue a second manual paste-mode lease. Node integration tests exercise ordinary quit and context disposal; the PTY smoke additionally checks actual terminal attributes, exit status, and paste-mode release.

## 8. Configuration

| Field | Default | Meaning |
|---|---|---|
| `resume` | absent | Exact persisted session id |
| `preset` | roster default for a fresh session | Fresh composition, or explicit legacy-session composition |
| `locale` | `en` | `en` or `zh` labels |
| `composerFrame` | `auto` | line glyphs for the composer's rule and the welcome card: `round`, `classic`, or `auto` to read the terminal's encoding, `TERM`, and character locale ([why](DESIGN-LAYOUT.md#the-frame-is-chosen-from-the-terminal-not-assumed)) |
| `doubleInterruptMs` | `2000` | How long the quit prompt waits for a second Ctrl-C; any other key dismisses it sooner |
| `completionLimit` | `8` | Positive integer limiting visible completion, picker, and staged-attachment rows |
| `resultLines` | `4` | Non-negative integer bounding the tool-result lines the transcript keeps under each outcome, and the reasoning rows it keeps from each step, with the rest counted ([why](DESIGN-LAYOUT.md#a-results-output-is-previewed-not-replayed)) |
| `attachmentMaxBytes` | `16777216` | Positive integer bounding total staged source bytes; Harness image limits also apply |
| `attachmentLimit` | `8` | Positive integer bounding staged source count |
| `credentialRefs` | `[]` | Provider key references offered by `/login`; the supplied patch names `DEEPSEEK_API_KEY` |

Schemastery validates and defaults configuration before the runner receives it. Locale dictionaries own application text; model output, tool results, and provider-owned diagnostics remain verbatim.

## 9. Validation

[The dispatcher](scripts/tui.ts) runs every development and validation command; `apps/tui/scripts/tui.ts help` prints them, and `verify` is the pre-push pair. It is Bun, as is everything it runs except the product and the two checks whose subject is the Node process the product runs in.

Its `check` command owns six individually selectable targets: React instance identity for TUI Ink consumers, strict application/test/tooling TypeScript checks, pure-module and tooling tests on Bun, Node component/integration tests, rendered layout invariants, and local Markdown links. `bun run verify` builds the workspace before running the checks and PTY scenarios. Built app and PTY launches use production React; the hot component preview runs separately on Bun. [Bun ownership](BUN.md) describes the shared build and performance driver.

[The PTY smoke](scripts/pty-smoke.ts) allocates a terminal through `bun:ffi` `openpty`, launches the built profile in a private workspace and home and drives it through named scenarios, each declaring what it proves and which scenarios it requires, so `--only` runs one with its prerequisites. Together they replay the shared recorded bash scenario, compare persisted model and real tool output, resume the exact session, check projected context and plan-mode status, and exercise cursor editing, paste, history recall with draft restoration after resume, picker cancellation and new-session/resume navigation, model/effort selection and restored request configuration, slash and quoted-file completion, logged skill invocation, cancellation, pending-input discard, attachment admission and exact stored bytes, durable metadata replay, terminal restoration, and screen-buffer checks for scrollback preservation and wrapped-draft cursor visibility after terminal resize. Every wait is named, so a step that never happens reports that name, the process state, and the screen within its own timeout, with the transcript kept in `tui/.smoke/` and the session state kept on disk. `--live` uses the root `.env` for a real DeepSeek call.

## 10. Limits

Session goals, workspace changes and scheduled follow-ups reach the log but not the screen: their events are dropped. Subagent identities and activity appear in a bounded branch panel with read-only session inspection. Plan mode appears in the status line and the task list has a panel. Navigation supports one displayed session in the current workspace. The picker reads matching records and titles in full while bounding visible rows. Inline scrollback has no virtualized transcript, and long-history performance qualification remains incomplete. Clipboard images and inline attachment previews are deferred. Source launch is unsuitable for qualifying tool execution on this checkout; use the built profile and the runbook in [PLAN.md](PLAN.md#132-build-from-a-clean-checkout).
