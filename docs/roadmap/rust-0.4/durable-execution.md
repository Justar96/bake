# Durable execution direction for the Rust harness

## Summary

Shape the native runtime so that every unit of agent work (a model request, a tool call, a compaction, a child agent, a background job) is an owned, typed task whose start, progress, and outcome are recoverable from the Session log. This page takes the task-and-scheduler model of Pi Durable as a design reference and applies it inside Bake's retained contracts: Session format 3 ([D4](scope-00/support.md#decision-register)), one serialized owner per agent, and reconstructable model requests. It adds internal structure and crash-recovery proofs to scopes 03 and 05–13. Changes the model or a released reader could observe are listed separately as post-0.4.0 candidates, each needing its own decision and paired eval.

**Status: proposed direction, 2026-10-08.** Nothing here is implemented or qualified, and it changes no support decision. Pi Durable's storage, API, and entry kinds are not compatibility targets, as the [Pi reference](pi-reference.md#compatibility-and-adoption) already states.

## Table of Contents

- [Sources](#sources)
- [What Bake already records](#what-bake-already-records)
- [Design rules for 0.4.0](#design-rules-for-040)
- [Post-0.4.0 candidates](#post-040-candidates)
- [Non-goals](#non-goals)
- [Scope additions](#scope-additions)
- [Crash-recovery matrix](#crash-recovery-matrix)
- [Dev Note](#dev-note)

## Sources

| Source | Revision or date | Used for |
|---|---|---|
| [Pi Durable announcement](https://earendil.com/posts/pi-durable/) | Read 2026-10-08 | Tasks, replay classes, ownership, background compaction, documents, attachable views |
| [`packages/durable/README.md`](https://github.com/earendil-works/pi/blob/ce950d78f424dcaf9f5d6a03ce80ab141130eb1d/packages/durable/README.md) | Pi `main` at `ce950d78f424dcaf9f5d6a03ce80ab141130eb1d`, after release v1.1.0 | Task states, waiting policies, abort order, compaction placement, commit and storage rules |
| Josh Rosen, "Pi Durable vs. OpenCode: An Architectural Comparison" ([post](https://x.com/JoshARosen/status/2107840212915077350)) | 2026-10-07 | The distinction between persisted *session state* and persisted *execution state* |

The comparison's central point applies directly to Bake. A session-based harness persists the conversation, but the work in flight lives in process memory: after a crash the harness can show what happened, yet it cannot tell which work was unfinished or whether repeating it is safe. Pi Durable persists execution state too, so a new process can find unfinished work and continue it. Pi Durable is experimental, its source was read but not run, and its API may change; record a fresh revision before adopting any specific behavior.

## What Bake already records

Bake 0.3 is closer to durable execution than a typical session harness. The Rust port must keep these properties and should build on them rather than add a second record of the same facts.

| Durable execution concern | Bake 0.3 behavior | Gap the native design should close |
|---|---|---|
| Tool intent before effect | `tool/call` is logged before dispatch; [crash repair](../../../packages/core/session/src/repair.ts) closes an unmatched call with `TOOL_NOT_STARTED` or `TOOL_OUTCOME_UNKNOWN` and tells the model not to retry side effects blindly | Tools carry no declared replay class, so the runtime cannot reason about safety itself; the decision always goes to the model |
| Interrupted turns | Synthetic closers end the open step and turn with `interrupted` | Recovery runs on load; an interrupted run does not continue unless the user resubmits |
| Partial model output | `assistant/attempt` records stream records of an attempt | Scope 06 must state how much of a cut-off stream reaches the log and when |
| Compaction | `compaction/start`, `compaction/summary`, `compaction/summary-error`, and `compaction/end` are logged | The summary is produced on the agent's critical path |
| Child agents | `subagent/start`, `subagent/end`, and routing events are logged; `subagent/not-resumable` exists | Scope 11 must inventory which child state survives a restart |
| Background jobs | [`jobs-local`](../../../packages/jobs/README.md) runs and stores jobs in this process | Jobs do not survive the process |
| UI state | The terminal renders authoritative projections ([analysis](analysis.md#execution-and-ownership)) | None in kind; the native transport should make the projection the only thing a client attaches to |

## Design rules for 0.4.0

These rules change internal structure, not the model surface or the Session format. Each is testable through the matrix below.

### 1. Work is an owned task with a recoverable status

Model the runtime's work as typed tasks with explicit states: *pending* (admitted but not started), *running*, *waiting* (on named child tasks, with an all-settled or fail-fast policy), *completing* (outcome decided, owned work still draining), and *terminal* (completed, failed, or aborted). In Rust these are values held by the agent's single owner task, not free-floating futures; each running task has one `JoinHandle` and one cancellation token derived from its owner.

The durable footprint of a task is the Session events Bake already writes. A task's recovered status must be derivable from the log alone: `tool/call` without `tool/result` is a started tool task, an open `compaction/start` is an unfinished compaction, and so on. Scope 02 defines this derivation as a pure projection beside request reconstruction; the projection, not process memory, answers "what was unfinished".

### 2. Record intent before effect, and classify replay

Keep the existing order: commit the call before any effect, commit the result before the next step uses it. Give every native tool an internal replay class:

| Class | Meaning | 0.4.0 recovery |
|---|---|---|
| `safe` | Rerunning has no external effect beyond the first run (read, glob, search, fetch) | Same as 0.3: report `TOOL_OUTCOME_UNKNOWN` to the model |
| `idempotent` | Rerunning with the same key has the same effect (a write of identical bytes, a child lookup by owner) | Same as 0.3 |
| `report` | Effects may have happened and repeating could duplicate them (shell, edits with relative changes, external deploys) | Same as 0.3 |

In 0.4.0 the class is internal metadata with tests, not a behavior change: automatic rerun alters what the model sees and is a [post-0.4.0 candidate](#post-040-candidates). The native plugin API ([D6](scope-00/support.md#decision-register)) requires every contributed tool to declare a class and defaults to `report`. MCP tools default to `report`, matching the [Pi reference](pi-reference.md#native-mcp-direction) rule against retrying calls whose outcome is uncertain.

### 3. One ownership tree, with an explicit background boundary

Agents, turns, model requests, tool calls, processes, child agents, jobs, and timers form one tree. Aborting a node aborts what it owns bottom-up, and a node becomes terminal only after its owned work has finished, so each owner undoes or reports its own effects before its parent proceeds. This is the [defensive patterns](../../defensive-patterns.md) obligation made structural.

Mark the boundary between foreground and background work explicitly. Foreground work belongs to the current turn: the agent is idle only once it is done, and Esc cancels it. Background work (a job, a scheduled follow-up, a detached child) belongs to the agent but not the turn: the turn can end while it runs, an ordinary cancel leaves it alone, and only disposing the agent or cancelling that node stops it. Each existing Bake behavior (Esc during a child call, `/jobs` kill, goal continuation) is mapped onto this tree in scope 11, and the mapping must reproduce 0.3's observable outcome.

### 4. Commit related events as a recoverable group

Format 3 appends events one row at a time, so a crash can leave any prefix. The native writer exposes a group append for events that belong together (a tool result and the context it adds, an inbox claim and its user message, a goal change and the event that caused it), flushes the group in order, and keeps the owner from publishing a view until the group is written. Recovery must handle every prefix of every group, as `interruptedTurnClosers` already does for turns. Scope 03 enumerates the groups and tests a crash after each row. True all-or-nothing commits would need a format change and stay out of 0.4.0 ([D4](scope-00/support.md#decision-register)).

### 5. Compaction is a task, not a phase of the loop

Implement compaction as a task owned by the agent, with its own cancellation, retries, usage accounting, and recovered status, instead of a step the loop calls inline. In 0.4.0 the loop still waits for it where 0.3 waits, so prompts and placement are unchanged. The structure lets a later release start a summary in the background and place it at a turn boundary without redesigning the loop.

### 6. Clients attach to the owner's view

The agent owner publishes one view: transcript projection, live stream, running tasks and their output, inbox, usage, and context pressure. The TUI, headless output, and Desktop each take the current view, then deltas; a client that joins late or reconnects starts from the current view and receives no replay of missed deltas. A slow client's queue is bounded and collapses to the newest full view. This makes the existing "render projections, keep no competing copy" rule a transport property, and gives scope 15's child inspection the same mechanism as the main agent.

### 7. Decisions are recorded once

Any human or hook decision that gates an effect, such as an approval, a question answer, or a hook's block or rewrite, is committed before the effect it gates. Recovery reads the recorded decision instead of asking again or re-evaluating a hook that may answer differently. Scope 08 inventories which 0.3 decisions are already logged; an unlogged decision that is re-asked after a crash is accepted 0.3 behavior, but the native design must not lose a decision that 0.3 records.

## Post-0.4.0 candidates

Each of these changes what the model sees, what a released reader must understand, or a public protocol. None is a 0.4.0 requirement. Each needs an owner decision, a regression test, and, when model-visible, a paired eval record.

| Candidate | Benefit | Constraint |
|---|---|---|
| Continue an interrupted run on resume | A crash or sleep no longer ends the user's request | Must not rerun `report` tools; the resumed request must be reconstructable from the log |
| Automatically rerun `safe` tools after a crash | Fewer wasted model round trips after recovery | Changes the model surface; eval against the 0.3 repair message |
| Background compaction placed at a turn boundary | Long sessions stop pausing for summaries | New placement and staleness rules; prompt-cache and request reconstruction proofs; long-session eval |
| Idempotent submissions keyed by a client request id | Headless and Desktop retries after a crash cannot double-submit | Desktop protocol version 1 is fixed ([D9](scope-00/support.md#decision-register)); needs a coordinated protocol change |
| Durable background jobs and schedules | Background work survives restarts | Process and sandbox ownership after restart; scope 04 proofs for adopting or killing orphans |
| Typed agent documents (plans, todos, goals) with fork rules | Application state stays consistent with the transcript through forks | Session format change; migration and rollback per [session format status](../../session-format-status.md) |
| Hook memos and plugin-defined task types | Native plugins get crash-safe decisions and long-running work | Part of the D6 plugin API design; needs a named consumer |

## Non-goals

- No SQLite, Pi Durable JSONL, or other new storage backend. Bake's generations, leases, and Session format remain the only storage.
- No general scheduler that bypasses the agent owner. Concurrency stays bounded work that returns results to the owner, and only the owner commits.
- No multi-writer or multi-process execution of one Session; the writer lease still excludes a second process.
- No change to tool names, schemas, results, or recovery messages in 0.4.0.

## Scope additions

These additions extend the scope specifications in the [roadmap](README.md#scope-specifications); they do not reorder scopes.

| Scope | Addition | Proof |
|---|---|---|
| 02 | Pure *unfinished-work* projection: started-without-result tools, open compactions, open children, pending inbox | Matches 0.3 repair on every historical fixture; property cases over truncated logs at every row |
| 03 | Group append and crash-after-every-row recovery | Kill the writer after each row of each group; every resulting log loads, repairs deterministically, and reconstructs a provider-valid request |
| 05 | Ownership tree and the foreground/background boundary as the lifecycle primitive | Cancel and dispose at each node; no owned task, process, or timer outlives its owner |
| 06 | Defined persistence point for partial streams | A crash mid-stream yields the same `assistant/attempt` content in both runtimes |
| 08 | Replay class on every tool, approvals committed before effects | Schema-level test that every registered tool declares a class; crash during each tool class yields the 0.3 recovery result |
| 09 | Agent owner holds task states; recovery reads the projection, not memory | Crash at each admission, dispatch, and commit boundary; recovered state equals the 0.3 oracle's |
| 10 | Compaction as an owned task with unchanged placement | Cancel and crash during summary; the log recovers and the next request matches 0.3 |
| 11 | Map Esc, job kill, child cancel, and goal continuation onto the ownership tree | Abort order is bottom-up; no completion notice is admitted twice after restart |
| 12 | Plugin API tools and MCP tools declare or default a replay class | A plugin tool without a class is registered as `report` |
| 13 | Clients attach to view plus deltas with bounded queues | A stalled client cannot block the owner; reconnect renders the current view |

## Crash-recovery matrix

Add these cases to [verification](verification.md) as each scope begins. Every case kills the process (not a graceful shutdown) at the named point, restarts against the same home, and asserts on the log, the files, the process table, and the next reconstructed request.

| Kill point | Expected outcome |
|---|---|
| After the `assistant/message` with a tool-call block, before its `tool/call` | `TOOL_NOT_STARTED` closer; no effect on disk |
| After `tool/call`, before the tool's first effect | `TOOL_OUTCOME_UNKNOWN` closer; no effect on disk |
| During a `report` tool (shell writing a file) | `TOOL_OUTCOME_UNKNOWN`; no automatic rerun; no surviving child process after restart |
| During a `safe` tool | Same as 0.3 in 0.4.0 |
| Mid model stream | The recorded attempt is preserved; the next request is provider-valid |
| During compaction summary | The log loads; no half-applied replacement; the next request matches the oracle |
| Between rows of an event group | Deterministic repair; projection matches the oracle |
| While a child agent runs | Parent and child logs both load; no duplicate completion notice; the child's processes are gone |
| While a background job runs | 0.3 outcome for jobs (lost with the process), reported to the agent, with no orphan process |

## Dev Note

This page owns the durable-execution direction and its Pi Durable provenance. The [Pi reference](pi-reference.md) owns the other inspected Pi revisions, and the [support register](scope-00/support.md#decision-register) owns product decisions; any candidate above that is adopted moves into that register first.
