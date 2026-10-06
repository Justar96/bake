---
description: "The workspace files a shell command changed while it ran, for maintainers of the bash and pwsh tools and anyone tracing how a shell edit reaches the terminal as a diff."
kind: "package-library"
---

# bake-shell-change-report

## Summary

`bake-shell-change-report` finds the workspace files a shell command changed while it ran, so a terminal can show a shell edit the way it shows an `edit` call. It reads git before and after the command, with hardened and time-bounded reads. It returns the changed files with contextual hunks, bounded so the session log stays small. The report is for display only: the shell tools attach it as `tool/result.meta`, and the model never receives it. `tool-bash` and `tool-pwsh` use it; it registers no service and has no `ctx` key.

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

A shell tool opens a window before it runs a foreground command and closes it afterwards. `openChangeReport` decides from the call's file policy whether to report the call, and how to confine the git reads. `finish` returns the report or `undefined`, and `release` closes a window without comparing, as an aborted command does.

```ts
import { openChangeReport, withRunningJobs } from 'bake-shell-change-report'

const window = await openChangeReport(ctx, exec, policy, workdir)
try {
  const result = await ctx.shell.run(spec)
  const changes = await window?.finish(exec.signal)
  if (changes !== undefined) exec.presentResultMeta({ shellChanges: withRunningJobs(ctx, exec, changes) })
} finally {
  await window?.release()
}
```

The tool's `presentResult` reads the logged report back with `shellChangesOf(result.meta)` and turns it into the terminal card's section with `terminalChanges`. Malformed or future-version metadata reads as no report, so a replayed session never fails to render.

### Which calls are reported

| Call | Report |
|---|---|
| Top-level foreground call in a git workspace, unconfined | Git reads run directly |
| Same, under `workspace-write` | Git reads run under `read-only` confinement through `ctx.sandbox`; no report without a provider |
| Under `read-only` | None: the command cannot write the workspace |
| Background job, nested PTC call, or no agent | None |
| Workspace outside git | None |

### The report

`ShellChanges` has `version: 1`, the changed files in path order, and optional `omittedFiles`, `timedOut`, `concurrent`, and `headChanged`. Each file has a path relative to the session's working directory, a status, added and removed line counts, a previous path for a rename, and hunks in the shape the `edit` card draws. A status is one of `created`, `modified`, `deleted`, `renamed`, `mode`, `symlink`, `binary`, `too-large`, or `unknown-before`. `concurrent` means another report's window overlapped this one in the same repository, or the caller had a background job running. The report states what changed while the command ran, which can include other writers.

### Bounds

`DEFAULT_CHANGE_REPORT_LIMITS` holds the defaults. Every bound applies to the complete serialized report, because the session log keeps it:

| Bound | Default | Beyond it |
|---|---|---|
| Time before the command | 500 ms | No report |
| Time after the command | 750 ms | Files found so far, `timedOut` |
| Files with hunks | 20 | Path and status only |
| Files listed | 200 | `omittedFiles` |
| Changed paths read for content | 1,000 | Paths and statuses only |
| Already-dirty files captured | 256 files, 16 MiB | `unknown-before` |
| File size read | 1 MiB | `too-large` |
| Hunk text per file | 64 KiB | Counts only |
| Serialized report | 256 KiB | Hunks dropped from the largest files, then files |
| Diff time | 100 ms per file, 400 ms in all | Status without hunks |

After two consecutive timeouts before the command, a workdir is skipped for ten minutes.

-----

<a id="understand-the-implementation"></a>
## Understand the implementation

<details>
<summary>Implementation internals — click to expand</summary>

### Source map

| File | Role |
|---|---|
| [`src/index.ts`](src/index.ts) | `beginChangeReport`, the comparison, classification, rename pairing, bounds, and `shellChangesOf` |
| [`src/git.ts`](src/git.ts) | The hardened git reader and the `status --porcelain=v2 -z` parser |
| [`src/hunks.ts`](src/hunks.ts) | Time-bounded contextual hunks |
| [`src/tool.ts`](src/tool.ts) | The shell tools' glue: policy-driven confinement, the running-jobs flag, and the terminal card's section |
| — | No runtime invariant companion is published; this library owns no event stream or mutable runtime data beyond its per-process window registry, which its specs cover. |

### How a comparison works

Before the command, the library copies the repository's index into a temporary directory and runs `git status --porcelain=v2 --branch -z --untracked-files=all --no-renames --ignore-submodules=all` against the copy. The copy retains the original index modification time, rounded down to whole seconds, so Git still checks content for entries whose timestamps might hide same-size edits. It records an `lstat` signature for every path that already differs from the index, and captures the bytes of those within the bounds. After the command, it runs the same status against the same copy. Because the base is the index from before the command, a commit, stash, or checkout the command makes cannot hide a change. A dirty path whose signature held is skipped. A clean path takes its previous content from its index blob through `git cat-file blob`, which applies no filters. A deleted file and a created file with the same content are reported as one rename. On this repository, with 70 dirty files, a call that changes nothing adds about 33 ms at the median.

The language-independent [racy-index cases](tests/fixtures/racy-index.json) fix the timestamps and bytes for clean and already-dirty files. The real-Git tests use these cases to verify the comparison without relying on host write timing. Rust migration scope 08 must adopt both cases when porting shell change reporting.

### Hardening

Git reads pass `-c core.fsmonitor=false` and `--no-optional-locks`, set `GIT_OPTIONAL_LOCKS=0`, `GIT_TERMINAL_PROMPT=0`, and `LC_ALL=C`, and clear the variables that would point a read at another repository. They never run `git add`, `--filters`, or a diff driver. `git status` still runs a repository's clean filters, which a sandboxed command could have planted, so under a confined policy the reads themselves run confined.

### Why git runs outside `ctx.subprocess`

On Linux, `ctx.subprocess` starts each process in its own systemd scope. That costs about 250 ms per spawn, measured, against 2 to 15 ms for a git read, and a report makes several reads around each command. Reads spawn git directly instead. Each read still owns its process: a POSIX child leads its own process group, which the deadline or the call's cancellation kills, and a read settles only once the child has closed. The environment starts from `scrubbedParentEnv()`, the shared credential scrub.

### Why hunks are time-bounded

jsdiff is quadratic on dissimilar inputs: a full rewrite of 10,000 lines took 9.7 s, blocking the event loop. `boundedHunks` passes jsdiff's `timeout`, and a file whose diff runs out of time is reported without hunks.

</details>

-----

<a id="further-exploration"></a>
## Further Exploration

- [Shell change report Agent Note](../../../.agents/notes/implemented/feature/2026-10-01-shell-change-report.md) — the design, the alternatives it beat, and its risks.
- [`bake-tool-bash`](../tool-bash/README.md) and [`bake-tool-pwsh`](../tool-pwsh/README.md) — the tools that open a window around each foreground call.
- [`dsh-tools`](../../core/tools/README.md#host-presentation-descriptors) — `presentResultMeta`, the channel the report travels through.

-----

<a id="model-experience"></a>
## Model Experience

None. The report is `tool/result.meta`, which the session's model-history projection leaves out, so the model's requests are byte-identical with or without it.

#### Token effect

None.

#### KV Cache effect

None.

<a id="known-limitations-and-deferred-work"></a>
## Known Limitations and Deferred Work

These limits define what the report deliberately does not cover. They are current package constraints, not a task backlog.

- **Attribution, not causation.** A user's editor, another agent, or a process started before the command can appear in the report. Only overlapping report windows and the caller's background jobs are detected and flagged.
- **Git workspaces only.** A workspace outside git, ignored files, and the contents of submodules and nested repositories are not reported.
- **Not every call.** Background jobs, PTC calls nested in `run_code`, and `tool-bash-persistent` calls are not reported.
- **A file captured past the budget** is reported as `unknown-before`, without hunks.

<a id="dev-note"></a>
### Dev Note

<details>
<summary>Working context for maintainers — click to expand</summary>

This Dev Note is working context for maintainers: open questions and undecided directions. It is non-authoritative; shipped behavior and limits live in the sections above and in the package code.

- A report when a background job settles would need a durable event of its own, because the job's `tool/result` is committed when it starts.
- A workspace outside git could be compared with a file listing and signatures, at the cost of before-content for modified files.

</details>
