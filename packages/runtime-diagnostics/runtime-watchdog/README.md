---
description: "Process health diagnostics for long sessions: event-loop delay and heap records and fatal-error forensics under the Harness home, for users and maintainers debugging memory growth or stalls."
kind: "package-reference"
---

# bake-runtime-watchdog

## Summary

This package leaves evidence when a long session slows down, grows toward V8's heap limit, or dies of a fatal error. It samples Node's event-loop delay, V8 heap use, and resident memory every ten seconds on an unreferenced timer. When the event loop stays delayed, or the heap reaches a high fraction of its limit, it appends a rate-limited JSON record to a file under `$DSH_HOME/diagnostics`. When Node runs with the flags that keep secrets and network data out of them, it also directs Node's fatal-error report, and optionally a near-limit heap snapshot, into that directory. It writes nothing to stdout, stderr, the session log, or model context. The `dsh` base bundle mounts it for every profile.

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

The base bundle already mounts the watchdog with its defaults, so a Bake profile needs no setup. Read the records after a session felt sluggish or ran out of memory; tune the thresholds below when the defaults record too much or too little.

### When to use it

Keep it mounted in any process that runs long agent sessions: the terminal, the desktop bridge, and long headless tasks. Its cost is a sampling tick of under 20 µs every ten seconds and an event-loop probe that wakes Node 20 times a second. Disable the row when a deployment must not write files under the Harness home.

### Configuration

The base bundle mounts this row:

```yaml
- id: runtime-watchdog
  name: 'bake-runtime-watchdog'
  config:
    directory: !!js dshHomePath('diagnostics')
```

A patch replaces the row's whole `config`, so an override restates `directory`:

```yaml
- id: runtime-watchdog
  config:
    directory: !!js dshHomePath('diagnostics')
    eventLoopDelayMs: 500
    heapSnapshots: 1
```

| Field | Default | Meaning |
|---|---|---|
| `directory` | required | Absolute directory for records, reports, and snapshots; created with mode `0700` |
| `intervalMs` | `10000` | Milliseconds between samples; each sample closes one event-loop delay window |
| `eventLoopDelayMs` | `250` | A window whose 99th-percentile event-loop delay reaches this value counts as delayed |
| `sustainedMs` | `30000` | How long consecutive delayed windows, or one continuous stall, must last before a delay is recorded |
| `heapFraction` | `0.85` | Heap use at or above this fraction of V8's heap size limit is recorded |
| `recordIntervalMs` | `300000` | Minimum time between two records of the same kind; crossings in between are counted in the next record |
| `fatalErrorReport` | `true` | Direct Node's fatal-error report into `directory` when Node runs with `--report-exclude-env` |
| `heapSnapshots` | `0` | Near-limit heap snapshots V8 may write into `directory` when Node's `--diagnostic-dir` is `directory`; `0` disables them |

A relative `directory` fails at startup, because it would resolve inside the workspace. The generated [configuration catalog](../../../docs/config-catalog.md#bake-runtime-watchdog) documents every accepted value.

### What you get

The first threshold crossing creates `watchdog.<YYYYMMDD>.<HHMMSS>.<pid>.jsonl` in `directory` with mode `0600`; later records from the same process append to it. Each line is one record, wrapped here for reading:

```json
{"time":"2026-09-28T12:00:00.000Z","kind":"event-loop-delay","pid":4242,"uptimeMs":3600000,
 "eventLoop":{"p50Ms":40,"p99Ms":320,"maxMs":1200,"meanMs":60,"samples":199,"windowMs":10000,"delayedForMs":30000},
 "memory":{"heapUsed":157286400,"heapTotal":209715200,"heapLimit":4395630592,"rss":402653184,"peakHeapUsed":167772160,"peakRss":419430400},
 "suppressed":0}
```

- `kind` is `event-loop-delay` or `heap-limit`. Both kinds carry the same fields, because heap pressure and a busy event loop often explain each other.
- `eventLoop` covers the sampling window that ended at `time`, in milliseconds of delay beyond the probe period. `delayedForMs` is how long consecutive windows have stayed delayed, or `0` when a single stall triggered the record.
- `memory` is in bytes; the peaks are the highest sampled values since the watchdog started.
- `suppressed` counts crossings of the same kind that the rate limit skipped since the previous record.

Each record also produces one warning on the Cordis logger named `runtime-watchdog`. Shipped profiles mount no console logger, so the file is the durable copy.

The launcher, not this plugin, writes `rejections.<YYYYMMDD>.<HHMMSS>.<pid>.jsonl` into `$DSH_HOME/diagnostics` for unhandled rejections it survives after startup, whatever this plugin's `directory`; the [launcher README](../../../apps/cli/README.md#startup-and-shutdown) describes those records.

Node's own diagnostics are armed only when they cannot leak into the workspace or disclose credentials:

- **Fatal-error report.** With `fatalErrorReport`, and Node started with `--report-exclude-env`, a fatal error such as running out of heap writes Node's `report.<date>.<time>.<pid>.<thread>.<seq>.json` into `directory`. The report holds the JavaScript and native stacks, heap space statistics, resource usage, and libuv handles, without environment variables. Without the flag the plugin leaves `process.report` untouched and logs why; `--report-exclude-network` also keeps network interfaces out.
- **Near-limit heap snapshot.** With `heapSnapshots` above zero, and Node's `--diagnostic-dir` resolving to `directory`, V8 writes up to that many `Heap.<date>.<time>.<pid>.<thread>.<seq>.heapsnapshot` files as the heap approaches its limit. A snapshot is several times the heap's size, pauses the process while V8 writes it, and contains every string in memory, including credentials and file contents. Otherwise the plugin logs why it did not arm them.

Launch Node with the flags as command-line arguments, not through `NODE_OPTIONS`, which the agent's subprocesses would inherit:

```sh
node --report-exclude-env --report-exclude-network --diagnostic-dir="$DSH_HOME/diagnostics" apps/cli/lib/bin.js --profile tui
```

-----

<a id="understand-the-implementation"></a>
## Understand the implementation

<details>
<summary>Implementation internals — click to expand</summary>

This section explains how the watchdog measures, decides, and records, and points at the code that does it; the observable behavior is covered in [Use this package](#use-this-package).

### Design notes

- **Cheap sampling.** Each tick reads the delay histogram, `v8.getHeapStatistics()`, and `process.memoryUsage.rss()`, which measured 7 to 18 µs on the development machine, depending on load. The interval timer is unreferenced, so it never keeps a finished process alive. The histogram samples every 50 ms; on a loaded machine this cost about 0.6 ms of CPU per second while idle, compared with 3 ms at Node's 10 ms default, and it still resolves delays well below the 250 ms threshold.
- **Delay beyond the probe period.** Every histogram value includes the 50 ms sampling period, which the watchdog subtracts. After a stall inside a JavaScript callback, Node runs the overdue watchdog tick before the histogram's own timer records the stall, and resetting the histogram then discards that sample. The tick's own lateness measures the same stall, so it joins the window maximum.
- **Sustained, not transient.** A window is delayed when its 99th-percentile delay reaches `eventLoopDelayMs`. A delay is recorded when consecutive delayed windows have lasted `sustainedMs`, or when one stall alone lasts that long. Isolated spikes from startup, a large paste, or one long render stay unrecorded.
- **Heap near the limit.** `used_heap_size / heap_size_limit` is compared with `heapFraction` on each tick. At 85% there is still headroom before an out-of-memory crash, and the threshold sits far above healthy use: a recorded 2000-turn replay retained about 82 MiB of a limit of several GiB.
- **Rate-limited, durable records.** Each kind records at most once per `recordIntervalMs`, which bounds a record file to about 24 lines per hour. Records are appended synchronously, so one written just before a crash is on disk. A tick catches every error and logs it, because an uncaught exception in a timer would make [fail-loud](../../boot/app-boot/README.md) exit the process.
- **No terminal output.** The terminal UI owns stdout, and stderr shares its screen, so records go to a file and the Cordis logger only.
- **Fatal reports need the command-line flag.** Node applies the runtime `process.report.excludeEnv` and `excludeNetwork` only when the fatal error happens inside a JavaScript context. An out-of-memory error raised elsewhere falls back to the command-line settings; a composition booted through the Loader reproduced this and wrote environment variables despite the runtime setting. The plugin therefore arms reports only when `--report-exclude-env` is in `process.execArgv` or `NODE_OPTIONS`, sets the four report fields, and restores their previous values on disposal.
- **Snapshots need Node's diagnostic directory.** `v8.setHeapSnapshotNearHeapLimit()` writes to `--diagnostic-dir`, or to the working directory when it is unset, and no runtime API changes that. The plugin arms it only when that directory resolves to `directory`, so a snapshot never lands in the workspace.

### Source map

| File | Role |
|---|---|
| [`src/index.ts`](src/index.ts) | Plugin entry: `Config` schema, Node process bindings, and `apply` |
| [`src/watchdog.ts`](src/watchdog.ts) | Sampling, thresholds, rate limit, record files, forensics arming, and Node option parsing |
| — | No runtime invariant companion is published; the watchdog reads process-level measurements and exposes no package-owned event or snapshot that an independent companion could check. |

</details>

-----

<a id="further-exploration"></a>
## Further Exploration

Read these pages when the package-level contract is not enough. They move from the composition that mounts the watchdog to the Node facilities it configures.

- [bake-base bundle](../../bundle/base/README.md) — the shared row every profile inherits.
- [App boot fail-loud](../../boot/app-boot/README.md#use-this-package) — why a throwing timer callback would end the process.
- [Generated configuration catalog](../../../docs/config-catalog.md#bake-runtime-watchdog) — every accepted config field and its source declaration.
- [Node.js diagnostic report](https://nodejs.org/api/report.html) — the report's contents and the `--report-exclude-*` flags.
- [runtime-diagnostics group map](../README.md) — the sibling invariant checks.

-----

<a id="model-experience"></a>
## Model Experience

None, as the watchdog writes only diagnostic files and logger warnings and registers no prompt, tool, or session event.

#### KV Cache effect

None; sampling and records never touch a model request.

## Known Limitations and Deferred Work

<a id="known-limitations-and-deferred-work"></a>

These limits describe what the watchdog does not capture or control. They are current package constraints, not a task backlog.

- **Forensics depend on launch flags** — Bake's launchers (`bake`, `bake.cmd`, and the development launcher) pass `--report-exclude-env --report-exclude-network --diagnostic-dir=$DSH_HOME/diagnostics`, so they arm fatal-error reports; heap snapshots still need `heapSnapshots` above 0. The npm `dsh` bin and direct `node apps/cli/lib/bin.js` entry restart Node with these arguments when needed. Custom embedders must supply them themselves.
- **An armed heap snapshot stays armed** — V8 has no way to cancel `setHeapSnapshotNearHeapLimit()`, so disposal or a configuration reload leaves it armed until the process exits.
- **Nothing prunes the directory** — record files and reports accumulate across processes. Each record file stays small under the rate limit and each report is tens of kilobytes, but every snapshot is heap-sized.
- **Growth below the threshold is not recorded** — steady heap growth under `heapFraction` leaves no record, and resident memory outside the V8 heap has no threshold; it appears only inside records.
- **Short stalls are not recorded** — a single stall shorter than `sustainedMs` that does not repeat stays out of the records.
- **Mount one instance** — `process.report` is process-global, so a second instance would overwrite the first one's settings and restore them out of order.

<a id="dev-note"></a>
### Dev Note

<details>
<summary>Working context for maintainers — click to expand</summary>

This Dev Note is working context for maintainers: open questions and directions that are not decided. It is explicitly non-authoritative — shipped behavior, limits, and accepted rationale live in the sections above and the package code.

#### Open questions

None currently recorded.

</details>
