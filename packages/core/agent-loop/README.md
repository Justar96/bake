---
description: "The default agent driver for users and maintainers choosing, configuring, or debugging how agents are created and how turns and steps run."
kind: "package-reference"
---

# bake-agent-loop

## Summary

`bake-agent-loop` creates fresh agents or resumes persisted sessions, then drives each turn through model requests, streamed responses, tool execution, and durable session history. Mount it for standard agent compositions; declarative entries start agents at boot, while the public `ctx.agents` API supports programmatic creation and resume. `maxParallelToolCalls` limits concurrent parallel-safe calls, and exclusive calls retain ordering. Cancellation preserves streamed text already delivered to the user. Choose a custom `Agent` implementation only when the standard "call model, run tools, repeat" lifecycle is insufficient.

## Table of Contents

- [Use this package](#use-this-package)
- [Understand the implementation](#understand-the-implementation)
- [Further Exploration](#further-exploration)
- [Model Experience](#model-experience)
- [Known Limitations and Deferred Work](#known-limitations-and-deferred-work)
- [Dev Note](#dev-note)

-----

<a id="use-this-package"></a>
## Use this package

Mount `bake-agent-loop` in any composition that should run agents. It supplies the driver behind `ctx.agents` and starts agents declared in its config; [`bake-base`](../../bundle/base/README.md) mounts it as an explicit row.

### Configure declarative agents

Agents declared in the config start automatically when the plugin loads. Each entry needs an `id` label; a model call additionally requires both `provider` and `model` (`agent/request` may supply a missing pair before dispatch).

```yaml
- name: 'bake-agent-loop'
  config:
    maxParallelToolCalls: 10
    agents:
      - id: 'main'
        provider: deepseek
        model: deepseek-chat
        reasoningEffort: high
        cwd: /workspace
```

| Field | Default | Meaning |
|---|---|---|
| `maxParallelToolCalls` | `10` | Parallel-safe tool calls in flight per step; `1` is serial |
| `agents[].id` | required | Stable label; a fresh session mints `${id}-session-<uuid>` unless `sessionId` is set |
| `agents[].provider` / `agents[].model` | — | Model route; both required before dispatch |
| `agents[].reasoningEffort` | — | Non-empty initial reasoning effort; `agent/request` may override it |
| `agents[].maxTokens` | — | Positive per-request output-token cap |
| `agents[].cwd` | — | Workspace directory for a fresh session |
| `agents[].sessionId` | — | Exact identity: first use creates, a remount resumes materialized history |
| `agents[].resumeSessionId` | — | Load this persisted session instead of creating one; mutually exclusive with `sessionId` |

The generated [configuration catalog](../../../docs/config-catalog.md#bake-agent-loop) is the exhaustive source for every accepted field. The adapter validates the effective reasoning effort and the loop records it in the request header. `maxParallelToolCalls` is also the whole `agent-loop` settings section, so a user layer over this entry caps the next tool group without a restart.

### Create or resume agents programmatically

Plugins and hosts create agents through `ctx.agents.create()` and resume persisted sessions through `ctx.agents.resume()`; both return an `AgentHandle` whose `dispose()` owns exact teardown. The loop runs each created agent to completion — the handle is only needed when the caller must tear the agent down itself.

```text
const handle = await ctx.agents.create({
  sessionId,
  agentOptions: { provider: 'deepseek', model: 'deepseek-chat' },
  setup: (agentCtx, agent) => { /* scoped registrations plus explicit unpublished Agent */ },
})
```

Every inbox mutation commits one normalized `agent/inbox/spliced` event. The projection registry folds that event synchronously, so the live projection reflects the splice when `Session.append()` returns. Insertions, edits, removals, claiming, and cancellation replay through the same standard splice coordinates. Ordinary removals carry `outcome: 'canceled'` and emit `agent/inbox/discarded { message }`; claiming uses pure deletions with no outcome and emits `agent/inbox/claimed`. Every insertion emits `agent/inbox/inserted { message }`. `MessageId` stays unique across both pending lists. Consumers that need a removed message use the claimed or discarded notification instead of depending on a pre-splice `session/event` view.

### What a step does

Each step sends the session's derived history — with the latest non-empty `system/message` node as the effective prompt, or no system messages when the rendered prompt is empty — and its visible tool schemas; the model's tool calls run through the guarded tool pipeline and every accepted fact is appended to the session log before the next step derives from it. Parallel-safe calls may overlap up to `maxParallelToolCalls`; exclusive calls run alone as ordering barriers. Cancellation is cooperative: `agent.cancel()` aborts the current activity and, unless `keepInbox` is set, clears pending work; a cancelled stream finalizes the text already delivered to the user. A reply cut off at the output token limit runs none of its tool calls; the first time a step in a turn drops calls that way, the loop tells the model which calls did not run and takes one more step. A tool policy or hook can halt a turn: calls already running finish, calls that had not started get synthetic results, and the turn ends as cancelled by a hook.

-----

<a id="understand-the-implementation"></a>
## Understand the implementation

<details>
<summary>Implementation internals — click to expand</summary>

This section explains how the package realizes the behavior above; the observable contract is covered in [Use this package](#use-this-package).

### Design concept

The package is the one concrete implementation of the public `Agent` contract. It registers itself as the `AgentFactory` on `ctx.agents`, so consumers never import this package; ownership of each created agent lives with the caller fiber and the loop provider, converging on one memoized quiescence boundary. Every observable effect happens through session events and the `agent/*` taxonomy — package internals are never part of the public surface.

### Request headers and adapter defaults

After `agent/request`, `ctx.llm.prepareCall()` validates adapter-owned fields and resolves reasoning-effort and output-token defaults under the active turn signal. The loop retains that exact adapter through resolution, `request/header` logging, and dispatch. It writes a full header for the first request, a changed envelope (config or tools — the prompt is not part of the header), an explicit message-series start, a request after a surface replacement or image-offload decision, and resume; unchanged steps, retries, and ordinary later turns in the same series inherit the latest header, and an in-history prompt append is not a replacement, so the request that follows it inherits the header too. Beside the header, the loop logs `request/context` — provider, model, `contextWindow`, and the route's `systemPromptUpdate` mode from `prepareCall()` — only when one of those differs from the latest snapshot. Before the next waterfall, the loop removes adapter-default fields so the current route resolves them again, while explicit settings persist. An unhandled route still fails with `NO_ADAPTER`.

The loop deep-freezes each derived message identity on its first request and reuses that proof only within the same agent. Restored messages keep their identity; request construction does not freeze their containing event wrappers. Each request freezes its local canonical header, fresh message array, and envelope while leaving the cancellation signal live. The [request-freeze decision](../../../.agents/notes/implemented/simplification/2026-09-06-agent-request-freeze-evidence.md) explains ownership and measurement.

### Source map

| File | Role |
|---|---|
| [`src/index.ts`](src/index.ts) | Plugin entry: `AgentLoop` service, config schema, declarative agent startup, factory registration |
| [`src/agent.ts`](src/agent.ts) | The concrete `ReactLoopAgent` driver: inbox, turn/step machine, cancellation |
| [`src/inbox.ts`](src/inbox.ts) | Package-internal `ReactLoopInbox`: durable projection, structural commands, and loop-only claim state |
| [`src/tool-calls.ts`](src/tool-calls.ts) | Tool scheduling: exclusive barriers, the bounded parallel pool, and policy halts |
| [`src/tool-progress.ts`](src/tool-progress.ts) | Bounding and throttling of process-local tool progress |
| [`src/max-tokens-notice.ts`](src/max-tokens-notice.ts) | The notice that tells the model which truncated tool calls did not run |
| [`src/runtime-context.ts`](src/runtime-context.ts) | Per-step runtime-context snapshot handling |
| [`src/constants.ts`](src/constants.ts) | `DEFAULT_MAX_PARALLEL_TOOL_CALLS` |
| [`src/invariant.ts`](src/invariant.ts) | Invariant companion: request reconstruction from the session log |

### Creation and teardown

Creation is one rollback-covered transaction: construct a private session, concrete agent, and scoped context; await optional setup; enter both registries; announce `session/created`; await serial `agent/created` listeners; then release queued input. A caller creating a runtime child sets `options.parentAgent`; the caller Context separately owns the transaction and live handle. Setup, commit, or listener failure and owner disposal roll back the prepared resources. Announcements already delivered remain observable and receive matching disposal notifications. Teardown stops and drains the driver, unwinds the scope, closes the session's write path, detaches the agent, then detaches the session. Every detach binds the exact entered object, so a stale disposer cannot remove a later same-id replacement. The launcher emits and awaits `app/shutdown` before unloading the plugin tree; the loop closes every agent through that event while the persistence service still routes its final events. Root fiber teardown joins the same disposal if it has already started.

### Persistence integration

The loop is the production acquisition point for session write handles. When `ctx.sessionPersistence` is mounted, `create`/`createAgent` call `persistence.create(header)` — storing the durable identity and taking write ownership before publication — and append the constructor seed through the handle; `resume` calls `persistence.open(id, 'write')` first (excluding a concurrent resume of the same id), reads the physically valid log through the handle, and appends `interruptedTurnClosers` for a log crashed mid-turn as an ordinary batch — semantic crash repair is the agent layer's job, not a storage entry point. Immediately before publication, `appendUnstoredSuffix` stores any events appended during the setup window (seed markers, delegation policy records), which never re-emit through `session/event`. Once published, the mounted backend routes the session's `session/event` batches, `session/flush` barriers, and `session/disposed` retirement into the active write handle by session id; the loop touches storage only through the handle it owns. The memoized teardown closes the handle — close drains any routed buffer — after the loop commits the session's closing events, provably releasing write ownership. Without a backend, sessions are memory-only and nothing else changes.

### Turn and step flow

The driver owns one agent for its lifetime and runs inside `ctx.agents.withInitiator(agent, ...)`. `AgentLoop` registers the standard `inbox` projection for its service lifetime, so cold reads work before any Agent exists and after all Agents unload. Its package-internal `ReactLoopInbox` uses that shared projection for structural commands and loop-only claims. At a turn boundary it opens the durable turn, then atomically claims pending next-step input plus one queued prompt; between steps it claims only next-step input. The driver assembles the prompt and tools, projects runtime context, and runs `agent/pre-step`. A rejected decision or empty first batch opens no step. On the first attempt after acceptance, `step/start` precedes the `agent/request` waterfall and `prepareCall()`; neither async phase sees the pending system prompt or accepted users committed to history, and cancellation during either commits neither. For every attempt, the loop then synchronously reconciles the rendered prompt against the surviving `system/message` nodes using the prepared call capability, appends the accepted `user/message` batch only on the first attempt, logs the header and context as needed, and derives and freezes the request before streaming through that bound prepared call. Retries reuse the same rendered assembly without repeating assembly, `agent/pre-step`, or user admission. Reconciliation sees pre-step and retry compaction; a broken series consolidates the prompt at the head rather than appending an update after already-committed users. The request carries `header.config`, `deriveMessages()`, `header.tools`, and `toolHistory()`; it carries no `system` field. Each model attempt emits one process-local `start`, emits each `chunk` immediately as it streams, and emits exactly one terminal `end` after durable settlement; `committed` follows a durable `assistant/attempt` or `assistant/message`, while `abandoned` marks a settlement that could not commit either event. Each successful model call appends one message anchor, and a cancelled stream appends an `interrupted: true` anchor with the delivered prefix so the next request contains what the user saw. Within a step, exclusive calls form barriers and parallel-safe calls use the bounded rolling pool; policy, durable results, and result context remain model-ordered. Process-local signals do not wait for that order: `agent/tool-executed` fires as each call's execution finishes, and `agent/tool-progress` carries a call's reported snapshot, bounded to its newest `TOOL_PROGRESS_MAX_CHARS` characters and throttled to the newest one per `TOOL_PROGRESS_INTERVAL_MS`. A snapshot still pending when the call finishes is dropped, and neither event reaches the session log or the model.

Prompt admission uses the actual `prepareCall()` result, not the preceding `request/context`. With no system node, even an empty prompt is appended (reserving node 0 without a wire message). On an incapable route or at a new request series, a non-empty rendering is consolidated at the first system node: each non-empty later system node receives a logged per-node empty replacement, then the head is rewritten if needed. Dormant empty tails need no replacement and do not determine the effective text. Consolidation applies even when the latest effective text is unchanged. On a continuing `in-history` series, an unchanged effective prompt produces no event and a non-empty change appends. An empty rendering clears every non-empty later system node through a logged per-node empty replacement, then empties the head if needed, regardless of route or series state. No older instructions remain model-visible. An empty head with no active later system node means no prompt; repeated clears and resume leave it empty. A restored non-empty prompt follows the same route/series rule: a continuing capable route may append it, while an incapable route or new series refills the head. A step starts a series when the pre-step decision declares `startsRequestSeries`, when `session.surface.contentGeneration` changed since attachment or the last request (a replacement or image-offload decision), or when visible tool schemas changed on a route without native tool updates. Resume and a provider or model swap alone continue the series; the prepared route still governs admission. Per-node empty replacements preserve intervening history without a surface delete operation.

When visible tool names change after the initial request, the loop records `request/tool-update` against the new header and latest user/tool-result message, then supplies `session.toolHistory()` to the LLM runtime. Routes declaring `toolUpdate` can activate deferred declarations in conversation order; `in-history` also retains removed declarations and sends removal notices. Other routes receive the complete active list. Missing anchors, changed definitions, and compaction prevent unsafe prefix reuse; resume alone preserves a complete declaration series.

### Failure and cancellation

Cancellation records a fresh `AgentCancelCause` in `turn/end`, retaining the caller's `kind` and the hook's `reason` text. The live `AbortSignal.reason` remains the caller's object, which a transport may extend — Node's fetch assigns a `stack` onto it — so the copy keeps that trace out of the log and keeps the ending appendable.

Final adapter selection, dispatch, and iteration failures arrive as terminal finishes and enter `agent/request-error`; a handling listener returns `{ kind: 'retry' }` without calling `next()`, while an unhandled failure is terminal. Middleware, result-processing, tool, and other extension failures remain thrown and close the turn directly — plugin failure ends the turn, not the loop. A `prepareCall()` failure before any dispatch is started is terminal the same way and does not enter `agent/request-error`: no attempt was made, so a retry would resubmit the identical route and config and fail identically; this covers adapter model/config resolution, unsupported reasoning effort, and middleware defects alike. `NO_ADAPTER` alone falls through instead of throwing, because an `llm/stream` listener may still serve an otherwise-unregistered route at dispatch time. Source and built tool runtimes share the same scheduler symbol. The loop retains the tool service for each call batch, so service teardown after a `tool/call` is logged can settle under the turn's cancellation signal. Undispatched model tool calls after cancellation receive synthetic `tool/call` plus `ABORTED_BEFORE_DISPATCH` result pairs. The [explicit-cancellation decision](../../../.agents/notes/implemented/architecture/2026-07-16-explicit-turn-cancellation.md) owns the signal lifecycle.

A step that finishes at the output token limit commits its safe assistant content but drops every tool call, complete or not, because the assembler cannot tell which calls finished streaming; the logged `assistant/message` keeps none of them. The first such step in a turn that dropped at least one call queues a plugin-sourced notice at the front of next-step input naming those calls, so the turn takes another step instead of running `agent/turn-stopping`; the notice is admitted as an ordinary `user/message`, so the request stays reconstructable from the log. A truncated step that dropped no calls, or any truncated step after the notice, ends the turn as before. A turn that reached the limit at any step ends as `max-tokens` even when a later step completes, so consumers that stop on truncation still see it.

A `tools/pre-execute` deny or any `tools/post-execute` decision may carry `halt: { reason }`. The tool runtime keeps the first halt an execution receives and stamps it on that call's final result. Once a prepared call or a committed result carries a halt, the scheduler starts no more calls, lets calls already running finish without aborting them, commits their results in model order, and gives calls that never started synthetic `ABORTED_BEFORE_DISPATCH` results. The driver then cancels with `{ kind: 'hook', reason }` and `keepInbox`, so `turn/end` records `aborted` with that cause, while post-execute context and pending input stay queued for the next wake. A halt that arrives after another cancellation leaves the earlier cause in place. The halt reason is not model-visible.

</details>

-----

<a id="further-exploration"></a>
## Further Exploration

The package-level contract is enough for most consumers; read these when you need the surrounding domain and the design rationale.

- [agent package](../agent/README.md) — the `Agent` handle, registry, and `agent/*` events this loop implements.
- [Core subsystem](../../../docs/subsystems/core.md) — the turn flow and interception decisions.
- [Session subsystem](../../../docs/subsystems/session.md) — the durable log the loop writes and derives from.
- [Tools subsystem](../../../docs/subsystems/tools.md) — the pipeline the loop dispatches through.
- [Explicit-cancellation Agent Note](../../../.agents/notes/implemented/architecture/2026-07-16-explicit-turn-cancellation.md) — signal lifetime and cancellation races.
- [Core group map](../README.md) — how the core packages compose.
- [Runtime request fixtures](tests/request-reconstruction.fixture.spec.ts) — three captured runs (a tool-call turn, dynamic tools, and a retried attempt) whose committed logs must replay to independent request expectations.

-----

<a id="model-experience"></a>
## Model Experience

### Complete conversation request

#### What the model sees

For each step, the loop sends the session's derived messages and visible tool schemas. Non-empty `system/message` nodes carry the prompt, with the latest as the effective version; an empty rendering clears all prompt versions from derived history. It supplies `provider`, `model`, and `cwd` variable values but no additional fixed prose.

#### Token effect

System text and schemas are paid again on every step, and on an `in-history` route every retained prompt version is paid until compaction shadows it or prompt reconciliation empties it. Per-agent scoping chooses the contributions, while the authoritative assembly waterfall can alter the final request and makes its listener responsible for protocol coherence.

#### KV Cache effect

Unchanged request prefixes remain eligible for provider caching. A capable route appends non-empty system updates and native tool additions/removals in conversation order. Unsupported routes, changed tool definitions, prompt consolidation, or surface replacement can invalidate reuse from the first changed token. Actual cache reuse depends on the provider.

### Retained message history

#### What the model sees

Accepted user messages, assistant messages, tool calls and results, injected context, and steering are logged and sent on later steps. Raw stream chunks and lifecycle boundaries add no message. Required tool updates contribute native provider notices through the logged tool history.

#### Token effect

Input grows with every surface message until a compaction replacement shadows older nodes; a multi-step tool turn resends the accumulated history each step.

#### KV Cache effect

Ordinary history growth is append-only and preserves reusable entries. A surface replacement or compaction invalidates reuse from the first shadowed history token.

### Undispatched calls after cancellation

#### What the model sees

If a later request replays an aborted or halted step, each tool call that cancellation or a policy halt prevented from dispatching has error code `ABORTED_BEFORE_DISPATCH` and result text `Error: tool call aborted before dispatch`.

#### Token effect

One fixed error result per skipped call remains in history until compaction shadows it.

#### KV Cache effect

Append-only; each synthetic result follows the reusable request prefix and does not invalidate existing KV Cache entries.

### Tool calls cut off at the output token limit

#### What the model sees

A truncated reply's tool calls are removed from the logged assistant message and never run. The first time a step in a turn drops calls this way, the next request adds one user message ahead of any steering that waited for the same step: `Your previous reply was cut off at the output token limit, so its tool calls did not run: <names>. Issue the calls you still need again, keeping each one small enough to finish within a single reply; for example, split large file content across several calls.` `<names>` lists the dropped calls' tool names in stream order, separated by `, `; a call whose name never streamed is left out, and when no name streamed the first sentence ends at `did not run.` with no list.

#### Token effect

At most one notice of a few dozen tokens per turn, plus the extra step that re-issues the calls; the notice stays in history until compaction shadows it.

#### KV Cache effect

Append-only; the notice follows the truncated assistant message and does not invalidate existing KV Cache entries.

## Known Limitations and Deferred Work

<a id="known-limitations-and-deferred-work"></a>


These limits define when the loop needs special care. They are current package constraints, not a task backlog.

- **Classification is unary** — calls whose safety depends on comparing siblings or resources must remain exclusive ([rationale](../../../.agents/notes/implemented/feature/2026-07-10-parallel-tool-call-execution.md)).
- **Config labels are fresh by default** — omitting `sessionId` creates a fresh `${id}-session-<uuid>` on every startup; exact resume-or-create behavior requires an explicit stable `sessionId`, while `resumeSessionId` requires existing persisted history.
- **Config agents have no per-agent persona field or setup hook** — they use the deployment persona; scoped persona and tool composition are available only through the programmatic `ctx.agents.create()` / `resume()` factory options.
- **No built-in turn budget** — tool calls or steering continue the current turn; a policy that bounds runaway turns must cancel from an existing lifecycle extension point such as `agent/turn-stopping`, or halt from a tool decision.
- **A halt from a nested `run_code` call waits for the program** — the scheduler sees only the model's own tool calls, so `run_code` forwards a halt that a policy or hook returns for one of its nested calls onto its own result. The turn stops once the whole program settles; the program's later nested calls still run.

<a id="dev-note"></a>
### Dev Note

<details>
<summary>Working context for maintainers — click to expand</summary>

None.

</details>
