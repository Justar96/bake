# Agent Note: Show the files a shell command changed

Status: implemented

English | [中文](2026-10-01-shell-change-report.zh.md)

## Problem

Models often change files through the shell instead of `edit`: `sed -i … && node test.cjs`, or a Python heredoc that rewrites several files and asserts each replacement. In the ten most recent local sessions, 103 `bash` calls edited files this way, 92 of them from Claude Opus. The earlier [content-anchored edits](2026-09-30-content-anchored-edits.md) review counted 384 such edits in 75 sessions. A paired probe showed that this path costs no extra round trips, because Claude chains the edit and the test in one call. Telling the model to prefer `edit` changed which tool Opus used, but not its request count or success rate, so Bake leaves the choice to the model.

The cost fell on the user. An `edit` call shows a diff card, but a shell edit showed only the command's output. The user could not see what changed without running `git diff`, and a non-zero exit hid even the fact that a file changed.

## Decision

Around each foreground, top-level `bash` or `pwsh` call in a git workspace, the tool records the workspace's state, runs the command, and compares. It attaches the changed files, with bounded hunks, to the call's own `tool/result` as presentation-only `meta`. The terminal card draws them as a changes section under the command's output. The model-visible result text, the canonical `value`, the tool schemas, and the descriptions are byte-identical with the report on or off, so the model's input does not change. The report lives in the existing opaque `tool/result.meta` field, so the Session format does not change.

### Presentation-only result metadata

No hook let a tool attach `meta` that does not derive from its canonical `value`:

- `output.presentationMeta` is a pure function of `(args, value)`.
- A `tools/execute` wrapper's success `meta` is dropped when `normalizeDispatchResult` rebuilds the result.
- `PostToolDecision` has no `meta` field.

Putting the report in `value` would make it model input, because the PTC SDK prompt renders every tool's `output.schema` and PTC programs receive `value`. `ToolRunContext.presentResultMeta(meta)` fills the gap. The registry snapshots the value and applies it where `createSuccessResult` computes `meta`, for top-level calls only. It survives a post-execute replacement of the value or the content. A failed result never carries it. A tool that declares `output.presentationMeta` cannot call it, so one result never has two sources of metadata.

### Detection

[`dsh-shell-change-report`](../../../../packages/shell/shell-change-report/README.md) is a library with no service and no `ctx` key. Before the command, it copies the repository's index into a temporary directory and runs `git status --porcelain=v2 --branch -z --untracked-files=all --no-renames --ignore-submodules=all` against the copy. It records an `lstat` signature for every path that already differs from the index, and captures those files' bytes within the bounds. After the command, it runs the same status against the same copy, so a commit, stash, or checkout made by the command cannot hide a change. A clean path takes its previous content from its index blob through `git cat-file blob`, which applies no filters. Hunks come from jsdiff with its `timeout`, because a 10,000-line rewrite took 9.7 s without one.

Reads spawn git directly, not through `ctx.subprocess`. On Linux, the subprocess service's containment starts each process in its own systemd scope, which costs about 250 ms per spawn, measured, against 2 to 15 ms for a read. Each read still owns its process. A POSIX child leads its own process group, which the deadline or the call's cancellation kills, and the read settles only once the child has closed. The environment starts from the shared credential scrub, `scrubbedParentEnv()`. On this repository, with 70 dirty files, a call that changes nothing takes 33 ms longer at the median.

The bounds apply to the complete serialized report, and every bound degrades the report, not the command:

- 200 ms before the command and 750 ms after it;
- 20 files with hunks, and 200 files listed;
- 1 MiB per file read;
- 64 KiB of hunks per file;
- 256 KiB per report;
- 100 ms of diffing per file, and 400 ms per call.

After two consecutive timeouts before the command, a workdir is skipped for ten minutes.

### Hardening and confinement

`git status` executes some repository-local configuration. In a test with git 2.43, it ran a planted `core.fsmonitor` command, and a planted clean filter on a changed file. Every read passes `-c core.fsmonitor=false` and `--no-optional-locks`. Reads never use `git add`, `--filters`, or a diff driver. Clean filters still run during status, so the reads follow the call's effective file policy:

| Policy | Reads |
|---|---|
| `workspace-write` | Run under `read-only` confinement through `ctx.sandbox`. Without a provider, there is no report. |
| `read-only` | Not made: the command cannot write the workspace. |
| `danger-full-access`, or no sandboxing executor | Run directly. They reach nothing the command could not. |

The same finding exposed the status line. It polled `git status` outside the sandbox every two seconds, so a sandboxed command could plant a hook that the poll then ran unconfined. That poll now runs under the same `read-only` confinement for a confined session, with the fsmonitor hook off.

### One call

1. Snapshot only a foreground, top-level call with an agent, when `changeReport` is on and the policy allows the reads.
2. Run escalation approval first, so a long wait does not widen the window.
3. Take the before snapshot, run the command, and on abort release the window and throw `TOOL_ABORTED`, as before.
4. Otherwise compare, and attach `{ shellChanges }` through `presentResultMeta`.
5. Release the temporary index in `finally`.

The report becomes visible only when `tool/result` is appended. `concurrent` marks a window that overlapped another report's window in the same repository, or a caller that had a background job running.

### Terminal presentation

`TerminalResultView` has an optional `changes` field. The terminal card draws one `edited <path>` line per file, a status word when the change is not a plain edit, and the file's numbered `-` and `+` lines as the `edit` card draws them. The head shows the total `+N −M` before the exit status.

The output and the changes are bounded separately. A non-zero exit colors the output red, while the changes keep their own tones. Diff lines from disk pass through the same sanitization as tool output. The `edit` card's diff lines now get it too, since they had skipped it before. A result without `changes` renders exactly as before.

### Configuration

`tool-bash` and `tool-pwsh` take `changeReport: boolean`, default `true`. The bounds stay library defaults, because no consumer has needed to tune them.

### Scope

Only top-level, foreground `bash` and `pwsh` calls in a git workspace are reported. These are out of scope:

- background jobs, whose `tool/result` is committed when the job starts;
- PTC calls nested in `run_code`;
- `tool-bash-persistent`;
- workspaces outside git;
- ignored files, and the contents of submodules and nested repositories;
- any summary the model sees.

### Relation to existing decisions

This decision partly supersedes two implemented decisions, which stay active and cross-linked.

- **[Canonical tool output contract](../architecture/2026-07-20-canonical-tool-output-contract.md).** File mutations there derive diff metadata from `args` and the canonical `value`, "rather than returning UI state from the body". A shell command's before-state is not part of its result, and putting it in `value` would make it model input in PTC mode. `presentResultMeta` is therefore a narrow exception: display-only metadata from the body, for top-level calls only. Tools that can derive their metadata from `value` keep `output.presentationMeta`.
- **[Tagged render-intent union](../architecture/2026-07-02-tool-render-intent-union.md).** That note lists "a terminal card cannot carry a diff" among the invalid states the union rules out. A command that also changed files is a valid state, so `TerminalResultView` carries optional `changes`. The union stays closed.

## Alternatives considered

- **Steer the model to `edit`:** a one-line persona instruction and the old description line moved no request count or success rate in paired probes, and Bake chose not to constrain the model.
- **Put the report in bash's `value` and use `output.presentationMeta`:** `value` and `output.schema` are model input in PTC mode. This would change the model's input, need an eval, and put UI vocabulary in a model contract.
- **A separate plugin around `tools/execute` with a new post-execute `meta` field:** this adds public surface for one capability, needs state carried from the wrapper to post-execute, makes listener order matter, and has a plugin writing another tool's private `meta`. It would also match `tool-bash-persistent`, which registers the same name.
- **A new log event such as `shell/changes`:** it needs a change record and an ignorable event type. The transcript would print it above its call, and the UI would have to merge two sources into one card.
- **Snapshot in the `ctx.shell` executor:** the executor has four providers and several consumers, so this would put a UI concern into a shared service definition.
- **Spawn git through `ctx.subprocess`:** each read would cost about 250 ms on Linux, several times per command.
- **An mtime and hash scan:** 340 to 390 ms per scan on this repository, with no ignore rules and no before-content.
- **A recursive file watcher:** about 800 ms to arm and 305 MB of resident memory. It reports paths only, and it dropped events silently under a 5,000-file `sed -i`.
- **Parse the command:** this misses scripts, `python -c`, formatters, codegen, `git checkout`, and variables.
- **A shadow index built with `git add -A`:** it handles renames best, but it runs clean filters and touched the timestamps of the real object store.
- **Snapshot from the TUI:** the result could not be reconstructed on replay, and the transcript renders logged events.
- **Synthetic `Edit` rows:** these invent calls that are not in the log and skew the step's tool counts.

## Consequences

A shell edit is now as visible as an `edit`, including after a failing command, and the model's requests are unchanged. The work found and closed a sandbox escape in the status line's git poll.

The record states what changed while the command ran, not what the command did. A user's editor, a running process, or another agent in the same workspace can appear in it. Only overlapping report windows and the caller's background jobs are detected and flagged.

Large repositories, network or NTFS filesystems, and racy index entries make status slower. The budgets bound the added time, and a slow repository loses the report rather than slowing the command.

The report is persisted in `meta`, and compaction pruning re-appends the event's data, so a pruned result stores it twice. The 256 KiB cap bounds each copy.

Running git on a repository the agent can write stays a risk class. The flags and confinement close the vectors found so far, and confinement, not the flags, is the main defense against a future git key that executes during status.

Changes made by background jobs, PTC programs, persistent shells, workspaces outside git, and ignored files stay invisible. The `edit` and `write` diffs had the same unbounded cost; `computeHunkDiffs` in `tool-fs` now stops past 1,000 changed lines and reports one block from the first differing line to the last.

## Testing

- [`shell-change-report/tests/change-report.spec.ts`](../../../../packages/shell/shell-change-report/tests/change-report.spec.ts) runs real git repositories. It covers:
  - in-place, created, deleted, renamed, mode, and binary changes;
  - a dirty file compared with its captured bytes;
  - an edit the command committed;
  - no-op rewrites, ignored output, and workspaces outside git;
  - the bounds and the time-bounded diff;
  - the planted fsmonitor hook, and an unwritten `.git`;
  - confinement routing, overlapping windows, and release.
- [`tool-bash/tests/integration.spec.ts`](../../../../packages/shell/tool-bash/tests/integration.spec.ts) drives the real bash tool through the agent loop. It shows the logged report, a model request that carries none of it, and no report for a background call, `changeReport: false`, or an unchanged workspace.
- [`core/tools/tests/tools.spec.ts`](../../../../packages/core/tools/tests/tools.spec.ts) pins `presentResultMeta`: top-level only, snapshotted, kept through post-execute replacement, and absent on failure.
- [`apps/tui/packages/app/tests/git.spec.ts`](../../../../apps/tui/packages/app/tests/git.spec.ts) shows that the confined status-line read keeps a planted clean filter inside real bwrap confinement, and that a refused wrapper leaves the field empty.
- The `shell-edit` PTY scenario runs a recorded `sed -i` and a failing command through the built profile under the default `workspace-write` policy. It shows the drawn changes and the edited file on disk, and `--resume` draws the same lines.
