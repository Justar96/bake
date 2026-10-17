# Durable execution direction for the Rust harness

## Summary

Shape the native runtime so that every unit of agent work (a model request, a tool call, a compaction, a child agent, a background job) is an owned, typed task whose start, progress, and outcome are recoverable from the session file. This page takes the task-and-scheduler model of Pi Durable as a design reference and applies it inside Bake's retained contracts: Pi's session format ([D32](scope-00/support.md#decision-register)), one serialized owner per agent, and model requests reconstructable from the session file. It adds internal structure and crash-recovery proofs to scopes 02 to 13. Changes the model or Pi's session reader could observe are listed separately as post-0.4.0 candidates, each needing its own decision and paired eval.

**Status: proposed direction, 2026-10-08; reconciled with [D22](scope-00/support.md#decision-register), [D32](scope-00/support.md#decision-register), and [D33](scope-00/support.md#decision-register) on 2026-10-14.** The rules below are requirements for the native runtime, and none is qualified. [Current implementation](#current-implementation) says which already hold in `bake-rs` and which test, if any, shows it; [Crash-recovery matrix](#crash-recovery-matrix) marks each kill point's evidence. This page changes no support decision. Bake Session format 3 and its events (`tool/call`, `assistant/attempt`, `compaction/start`, and the rest) are not written or read by 0.4; where this page names them, it describes 0.3. Pi Durable's storage, API, and entry kinds are not adopted.

## Table of Contents

- [Sources](#sources)
- [What 0.3 recorded and what Pi's session records](#what-03-recorded-and-what-pis-session-records)
- [Design rules for 0.4.0](#design-rules-for-040)
- [Current implementation](#current-implementation)
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

## What 0.3 recorded and what Pi's session records

Bake 0.3 was closer to durable execution than a typical session harness. Under [D32](scope-00/support.md#decision-register) its Session events are not carried forward, but the properties they gave are obligations the native runtime keeps inside Pi's format. Pi's session file holds one JSON line per entry, linked by `id` and `parentId`: `message` entries (user, assistant with its `toolCall` blocks, and `toolResult` messages), `compaction` and `branch_summary` entries, model and thinking-level changes, labels, session info, context edits, and `custom` entries that never reach the model.

| Concern | Bake 0.3 | Pi's session in `bake-rs` | Gap the native design closes |
|---|---|---|---|
| Tool intent before effect | `tool/call` logged before dispatch; [crash repair](../../../packages/core/session/src/repair.ts) closed an unmatched call with `TOOL_NOT_STARTED` or `TOOL_OUTCOME_UNKNOWN` | In a persisted session, the assistant `message` holding the `toolCall` blocks is appended and synced before its tools run; if that append fails, the agent is aborted and tool preparation, which checks the abort first, runs no tool. No entry records that a call started. When the next request is built, Pi's `transformMessages` answers a call with no `toolResult` with a synthetic error result, `No result provided`, in the request only; the file is not changed. An assistant message that ended in an error or abort is dropped from the request with its calls | No started/not-started distinction and no replay class, so neither the runtime nor the model can tell whether an effect happened |
| Interrupted turns | Synthetic closers ended the open step and turn | The file ends at the last synced entry; the next request is rebuilt from the path to the leaf | An interrupted run does not continue unless the user resubmits |
| Partial model output | `assistant/attempt` recorded an attempt's stream | An assistant message is appended only at its end, including one that ended in an error or abort; a failed attempt Pi retries stays in the file and is omitted from context by a context edit | A crash mid-stream loses the partial output; scope 06 defines what reaches the file |
| Compaction | `compaction/start` to `compaction/end` logged | A `compaction` entry is appended once its summary exists | The summary runs on the agent's critical path |
| Child agents | `subagent/catalog` and descriptors logged | Not implemented | Scope 11 decides which child state survives a restart |
| Background jobs | [`jobs-local`](../../../packages/jobs/README.md) ran them in-process | Not implemented | Jobs do not survive the process |
| Decisions | Approvals and hook decisions were logged before their effects | The session's permission settings are `bake.permission` custom entries; print mode has no answerer, so escalations are refused and nothing is asked | Interactive approvals and their logged decisions are not implemented |
| UI state | The terminal rendered authoritative projections ([analysis](analysis.md#execution-and-ownership)) | The terminal preview has no agent connection | The native transport should make the projection the only thing a client attaches to |

## Design rules for 0.4.0

These rules change internal structure, not the model surface or the session format. Each is testable through the matrix below.

### 1. Work is an owned task with a recoverable status

Model the runtime's work as typed tasks with explicit states: *pending* (admitted but not started), *running*, *waiting* (on named child tasks, with an all-settled or fail-fast policy), *completing* (outcome decided, owned work still draining), and *terminal* (completed, failed, or aborted). In Rust these are values held by the agent's single owner task, not free-floating futures; each running task has one `JoinHandle` and one cancellation token derived from its owner.

The durable footprint of a task is the session entries the runtime already writes. A task's recovered status must be derivable from the file alone: a `toolCall` on the path to the leaf without a matching `toolResult` is an unfinished tool task, and so on. Scope 02 defines this derivation as a pure projection beside the context rebuild; the projection, not process memory, answers "what was unfinished".

### 2. Record intent before effect, and classify replay

Keep the order: persist the assistant message that requests a call before any effect, and persist the result before the next request uses it. Give every native tool an internal replay class:

| Class | Meaning | 0.4.0 recovery |
|---|---|---|
| `safe` | Rerunning has no external effect beyond the first run (read, glob, search, fetch) | Pi's synthetic error result in the next request; no automatic rerun |
| `idempotent` | Rerunning with the same key has the same effect (a write of identical bytes, a child lookup by owner) | Pi's synthetic error result; no automatic rerun |
| `report` | Effects may have happened and repeating could duplicate them (shell, edits with relative changes, external deploys) | Pi's synthetic error result; no automatic rerun |

In 0.4.0 the class is internal metadata with tests, not a behavior change: automatic rerun alters what the model sees and is a [post-0.4.0 candidate](#post-040-candidates). The native plugin API ([D6](scope-00/support.md#decision-register)) requires every contributed tool to declare a class and defaults to `report`. MCP tools default to `report`, matching the [Pi reference](pi-reference.md#native-mcp-direction) rule against retrying calls whose outcome is uncertain.

### 3. One ownership tree, with an explicit background boundary

Agents, turns, model requests, tool calls, processes, child agents, jobs, and timers form one tree. Aborting a node aborts what it owns bottom-up, and a node becomes terminal only after its owned work has finished, so each owner undoes or reports its own effects before its parent proceeds. This is the [defensive patterns](../../defensive-patterns.md) obligation made structural.

Mark the boundary between foreground and background work explicitly. Foreground work belongs to the current turn: the agent is idle only once it is done, and Esc cancels it. Background work (a job, a scheduled follow-up, a detached child) belongs to the agent but not the turn: the turn can end while it runs, an ordinary cancel leaves it alone, and only disposing the agent or cancelling that node stops it. Scope 11 maps each Bake surface it ports (Esc during a child call, job kill, goal continuation) onto this tree and states each observable outcome.

### 4. Commit related entries as a recoverable group

Pi's format appends entries one line at a time, so a crash can leave any prefix of a group that belongs together (a tool result and the context it adds, an inbox claim and its user message, a goal change and the entry that caused it). The native writer exposes a group append that writes a group in order, and the owner publishes no view until the group is written. Loading must handle every prefix of every group. Scope 03 enumerates the groups and tests a crash after each line. True all-or-nothing commits would need a format change and stay out of 0.4.0.

`SessionManager::append_group` is that writer. It takes typed `GroupEntry` values, each the entry its single append writes with the same checks; there is no raw-JSON entry and no callback into the manager. The group's semantics:

- **Checks first.** Every entry is built and checked before anything is written. A failed check (a context edit's target missing, off the active branch, or not editable) returns its error and changes nothing in memory or on disk. Recovery omission treats a failed check as a session gap and stops later writes and requests, even though the group wrote nothing.
- **Order and exclusion.** The entries are chained (the first a child of the leaf, each later one a child of the one before) and written while the caller holds the manager: in `AgentSession`, under the session lock at the write boundary, so no other entry lands between them and no caller reading through that manager or under that lock sees part of the group. The writer lock excludes other writers, not readers: another process reading the file meanwhile sees whatever prefix has reached it.
- **Line by line.** From one open of the file, each line is written and then synced before the next. A session whose file is not due yet writes nothing, as for every append; one whose file is due creates it with the entries before the group, then appends the group.
- **Not atomic.** A crash leaves any prefix of the group, possibly ending in a torn line, which loads as Pi's loader reads any file. A torn line whose whole JSON reached the file reads as its entry.
- **Failure before the first line** (writer lock held or closed, file not creatable): an error other than `SessionError::GroupWrite`; nothing of the group is written or kept in memory.
- **Failure at a line**: `SessionError::GroupWrite` (`GroupWriteError`) names the entries written and synced, which the manager keeps with the leaf on the last of them, and says what happened to the next line (`FailedLine`): `NotWritten`, `Torn` (some of its bytes are in the file), or `Unsynced` (all its bytes were written but the sync failed, so a reader may see the line now and a crash may lose it). No later line is written. Unlike a single append, which keeps a failed entry in memory as Pi does, the manager keeps only the known synced prefix. That is not always what an immediate reopen reads: an `Unsynced` line, or a `Torn` one whose whole JSON reached the file, reads as one entry more, and a crash may still lose it. Owners show the known synced prefix and halt; nothing reconciles the two views within the run.
- **No retry.** A failed group is never written again: the file may hold its prefix, and a second copy would follow it under new ids. `AgentSession` records the failure as the session's gap at the write boundary, so nothing more is written, the running turn stops, and later prompts are refused.

| Group | Consumer | Entries, in order | On a failed write |
|---|---|---|---|
| Model switch | `AgentSession::set_model` | `model_change`, then `thinking_level_change` when the level changes | Nothing applied unless the model line is known synced; then the new model with the previous level clamped to it. The session stops. A reopen projects the same only when the level line is unreadable; an unsynced or whole-JSON torn level line reads as the new level |
| Recovery omission | `Core::omit_recovery_attempt` (retry and overflow recovery) | A `context_edit` per tool result of the failed attempt, then the attempt's | The transcript is refreshed to the edits known synced, and the failure ends the run before another provider request (no retry wait, no compaction); the session stops. Pi writes the attempt's edit first; Bake writes it last, so no prefix leaves a tool result whose call is omitted, which a provider refuses. The written group projects as Pi's order does |
| New session's model and level | `AgentSession::create` | `model_change` when there is a model, then `thinking_level_change` | Creation fails. A new session's file is created with these entries and its first message in one write; only a file already written without a conversation appends them as a group |

Not groups: a compaction is one entry, its usage inside it; a message, a session name, a thinking-level change alone, and a label are single entries. Future groups use the same API with a new `GroupEntry` kind: a tool result and the context it adds, an inbox claim and its user message, a goal change and the entry that caused it, and plugin entries under the D6 plugin API. Each needs its own prefix analysis, as the omission's order shows.

### 5. Compaction is a task, not a phase of the loop

Implement compaction as a task owned by the agent, with its own cancellation, retries, usage accounting, and recovered status, instead of a step the loop calls inline. In 0.4.0 the loop still waits for it where Pi waits, so prompts and placement are unchanged. The structure lets a later release start a summary in the background and place it at a turn boundary without redesigning the loop.

### 6. Clients attach to the owner's view

The agent owner publishes one view: transcript projection, live stream, running tasks and their output, inbox, usage, and context pressure. The TUI, headless output, and any retained transport client each take the current view, then deltas; a client that joins late or reconnects starts from the current view and receives no replay of missed deltas. A slow client's queue is bounded and collapses to the newest full view. This makes the existing "render projections, keep no competing copy" rule a transport property, and gives scope 15's child inspection the same mechanism as the main agent.

### 7. Decisions are recorded once

Any human or hook decision that gates an effect, such as an approval, a question answer, or a hook's block or rewrite, is committed before the effect it gates. Recovery reads the recorded decision instead of asking again or re-evaluating a hook that may answer differently. Scopes 04, 08, and 12 choose how each decision is recorded in the session file before its effect; the session's permission settings already use `bake.permission` custom entries, which the model does not see.

## Current implementation

At candidate `e32476594bace82b57980fe8e1d57e48e3c4c53c`, with the group append added on 2026-10-17 at `41e5e867a3` ([ledger record](ledger/scope-03/2026-10-17-session-group-append.json)). "Tested" names the test that shows the behavior; "not tested" means the code path exists without a test of the property.

| Rule or property | State in `bake-rs` | Evidence |
|---|---|---|
| One writer per session file | Implemented: an exclusive `File::try_lock` on a `.jsonl.lock` sidecar; a second writer fails with `SessionError::Locked` and takes over after the holder releases, exits, or is killed | Tested: `tests/session/writer_exclusion.rs` in `bake-coding-agent`, on Linux and Windows CI and locally on Linux |
| Durable appends and rewrites | Implemented: each append is synced and starts on a new line after a torn tail; rewrites go through a synced temporary file renamed over the session | Tested with written torn bytes (`a_torn_tail_resumes_and_appends`, `golden::torn_tail_repairs_the_same`) and a writer process killed after each synced line of a group and after part of a line it wrote (rule 4 row); no process is killed inside a `write` system call |
| Intent before effect (rule 2) | In a persisted session, assistant messages are appended at `message_end`, and the agent awaits that listener before it prepares the message's tools; a failed append aborts the agent, and tool preparation checks the abort before running a tool. A `--no-session` run writes nothing | Not tested, by a crash or by a failed append |
| Unfinished-work projection (rule 1) | Partial, added after that candidate. `session::unanswered::build_unanswered_tool_calls` is the provider-visible part. It is a pure function of the entries and a leaf, giving the calls the next request closes with Pi's synthetic result. It reads the projected context, so compaction, context edits, and dropped assistant messages apply. The path-history part is not implemented: which calls on the path to the leaf lack a recorded result, whether or not the request still shows them. Neither part tells whether a call ran. Open children and the pending inbox wait for scope 11 | Tested: `tests/session/unanswered.rs`, against hand-derived cases and `transform_messages`, over every byte and line truncation of a session with tool calls |
| Group append (rule 4) | Implemented for the model switch, the recovery omission, and a new session's model and level ([rule 4](#4-commit-related-entries-as-a-recoverable-group)) | Tested: `session::manager::group::tests` in `bake-coding-agent` loads every byte prefix of both consumer groups, checks each against the projection of the entries it holds and a provider-valid request, and takes over as a writer at each line boundary and at every 13th byte; injects a refused, torn, and unsynced line at each line of a group and checks the file bytes, the kept entries, and that no later line is written; kills a child writer process after each synced line and after a partial line it wrote; checks that an unsynced line, or a torn one whose whole JSON was written, is read by an immediate reopen though the manager keeps only the synced prefix; the seam panics if its own write or sync fails; the killed child's exit status must be the parent's kill, not its watchdog or a returned write; `agent_session::{control_tests, recovery_tests}` check each consumer's failure through real file bytes, including an overflow-recovery omission that publishes only its synced prefix and makes no compaction request. Not tested: a kill inside a `write` system call, real `ENOSPC` or `EIO` |
| Replay classes, compaction as a task, attached views, logged decisions (rules 2, 3, 5 to 7) | Not implemented | None |
| Ownership tree (rule 3) | Partial: every tool's child (bash, exec, grep, find) is spawned through `OwnedProcess` under the session's `WorkGate`, which after a kill awaits the leader and its whole tree with no time limit, never signals a tree whose leader it no longer pins, and on drop settles the process under the same permit; a model request's task awaits its provider task; `AgentSession`'s `WorkGate` owns the session's tasks and blocking work, with cancellation passed from a task to the tasks it spawned, and `spawn_background` is the background lifetime that `abort` leaves and shutdown cancels and awaits; `AgentSession::shutdown` removes listeners and closes the writer | Tested for bash and exec process trees and for the gate and session (`owned_work::tests`, `agent_session::service_tests`); process-tree waits are proven on Linux with a subreaper (`tests/process_reap.rs`), Windows and macOS only cross-checked, and provider tasks over loopback HTTP (`model_registry::tests`); a normal exit's backgrounded descendants, a descendant that leaves its tree, the bubblewrap payload's namespace, `config_value` commands, and sandbox probes are outside it; no tree spans agents, jobs, or children |

## Post-0.4.0 candidates

Each of these changes what the model sees, what a released reader must understand, or a public protocol. None is a 0.4.0 requirement. Each needs an owner decision, a regression test, and, when model-visible, a paired eval record.

| Candidate | Benefit | Constraint |
|---|---|---|
| Continue an interrupted run on resume | A crash or sleep no longer ends the user's request | Must not rerun `report` tools; the resumed request must be reconstructable from the session file |
| Automatically rerun `safe` tools after a crash | Fewer wasted model round trips after recovery | Changes the model surface; eval against Pi's synthetic `No result provided` result |
| Background compaction placed at a turn boundary | Long sessions stop pausing for summaries | New placement and staleness rules; prompt-cache and request reconstruction proofs; long-session eval |
| Idempotent submissions keyed by a client request id | Headless and retained transport clients' retries after a crash cannot double-submit | Desktop is deferred beyond 0.4.0 ([D9](scope-00/support.md#decision-register)); a Desktop client needs a coordinated protocol change in a later release |
| Durable background jobs and schedules | Background work survives restarts | Process and sandbox ownership after restart; scope 04 proofs for adopting or killing orphans |
| Typed agent documents (plans, todos, goals) with fork rules | Application state stays consistent with the transcript through forks | New entry kinds that Pi's reader must still open; a decision on extending Pi's format |
| Hook memos and plugin-defined task types | Native plugins get crash-safe decisions and long-running work | Part of the D6 plugin API design; needs a named consumer |

## Non-goals

- No SQLite, Pi Durable JSONL, or other new storage backend. Pi's session files, with the writer exclusion scope 03 adds, remain the only storage.
- No general scheduler that bypasses the agent owner. Concurrency stays bounded work that returns results to the owner, and only the owner commits.
- No multi-writer or multi-process execution of one session; the writer lock excludes a second writer.
- No change in this direction to tool names, schemas, results, or recovery messages, which follow Pi ([D33](scope-00/support.md#decision-register)).

## Scope additions

These additions extend the scope specifications in the [roadmap](README.md#scope-specifications); they do not reorder scopes.

| Scope | Addition | Proof |
|---|---|---|
| 02 | Pure *unfinished-work* projection over the path to the leaf: `toolCall` blocks without a `toolResult`, and later open children and pending inbox | Property cases over files truncated at every line; the projection and Pi's `transformMessages` agree on which calls are unfinished |
| 03 | Group append and crash-after-every-line recovery | Kill the writer after each line of each group; every resulting file loads as Pi loads it and rebuilds a provider-valid request |
| 05 | Ownership tree and the foreground/background boundary as the lifecycle primitive | Cancel and dispose at each node; no owned task, process, or timer outlives its owner |
| 06 | Defined persistence point for partial streams | A crash mid-stream leaves the file Pi would leave and a provider-valid next request |
| 08 | Replay class on every tool, approvals committed before effects | Schema-level test that every registered tool declares a class; a crash during each tool class yields Pi's synthetic result and no rerun |
| 09 | Agent owner holds task states; recovery reads the projection, not memory | Crash at each admission, dispatch, and commit boundary; the recovered state equals the projection of the file |
| 10 | Compaction as an owned task with unchanged placement | Cancel and crash during summary; the file loads with no half-applied replacement and the next request is provider-valid |
| 11 | Map Esc, job kill, child cancel, and goal continuation onto the ownership tree | Abort order is bottom-up; no completion notice is admitted twice after restart |
| 12 | Plugin API tools and MCP tools declare or default a replay class | A plugin tool without a class is registered as `report` |
| 13 | Clients attach to view plus deltas with bounded queues | A stalled client cannot block the owner; reconnect renders the current view |

## Crash-recovery matrix

Add these cases to [verification](verification.md#acceptance-matrix) as each scope begins. Every case kills the process (not a graceful shutdown) at the named point, restarts against the same home, and asserts on the session file, the files, the process table, and the next rebuilt request. The *evidence* column records what exists at candidate `e32476594b`, with the group-append row updated on 2026-10-17; a passing writer-lock or torn-tail test does not prove the other rows.

| Kill point | Expected outcome | Evidence |
|---|---|---|
| During an append of any entry | The torn last line is skipped as Pi’s loader skips it; the next append starts on a new line | Every-byte truncation of a file with tool calls, whose unanswered calls survive repair and the next append; a writer process killed after it wrote part of a group’s line (`a_writer_killed_between_group_lines_leaves_a_loadable_prefix`). No process is killed inside a `write` system call |
| While another process holds the writer lock | The next writer takes over once the holder dies | Tested: `a_killed_holder_process_is_taken_over` |
| During a rewrite (migration, branched session) | The old or the new file is complete, never a mix | Rename path tested without a kill |
| After the assistant message with a `toolCall`, before the tool's first effect | The next request carries Pi's synthetic error result; no effect on disk | Not tested |
| During a `report` tool (shell writing a file) | Pi's synthetic error result; no automatic rerun; no surviving child process after restart | Not tested |
| During a `safe` tool | Same as `report` in 0.4.0 | Not tested |
| Mid model stream | The file ends at the last finished entry; the next request is provider-valid | Not tested |
| During compaction summary | The file loads; no half-applied replacement; the next request is provider-valid | Not tested |
| Between lines of an entry group | Deterministic loading; the projection matches the file | Tested: a writer process killed after each synced line of a recovery omission, and after a partial line it wrote, leaves a file that loads to the synced lines, rebuilds a provider-valid request, and is taken over (`a_writer_killed_between_group_lines_leaves_a_loadable_prefix`); every byte prefix of the model-switch and omission groups is loaded without a kill. A kill inside a `write` system call is not tested |
| While a child agent runs | Parent and child files both load; no duplicate completion notice; the child's processes are gone | Not implemented |
| While a background job runs | The job is lost with the process and reported to the agent, with no orphan process | Not implemented |

## Dev Note

This page owns the durable-execution direction and its Pi Durable provenance. The [Pi reference](pi-reference.md) owns the other inspected Pi revisions, and the [support register](scope-00/support.md#decision-register) owns product decisions; any candidate above that is adopted moves into that register first.
