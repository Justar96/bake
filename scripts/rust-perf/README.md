# Rust performance harness

`scripts/rust-perf.ts` measures the D18 terminal workloads by one external method for a native `bake` binary and for a built Bake 0.3 checkout, and proposes budgets from a baseline report. It reads `/proc`, so it runs on Linux only. It loads no code from `apps/` or `packages/` and needs no model key: both arms stream from a loopback model the harness owns.

```sh
cd rust && cargo build --locked --release -p bake-cli && cd ..
bun scripts/rust-perf.ts --arm ts:/path/to/bake-v0.3.8 --arm native:rust/target/release/bake --max-load 1
bun scripts/rust-perf.ts budgets --baseline .preflight/perf/<report>.json --arm ts:/path/to/bake-v0.3.8
```

The `ts` arm is a clean v0.3.8 checkout built with `bun install --frozen-lockfile`, `bun run build`, and `bun run build:native-system`; the harness runs `node <checkout>/apps/cli/lib/bin.js --profile tui --patch <file>` in it. `--workload` (repeatable) selects workloads, `--samples` the measured samples per arm, workload, and cache mode (20 by default), and `--mode cold`, `warm`, or `both` (the default) the cache conditions. Reports go to `.preflight/perf/` unless `--out` names another file; the harness refuses any path under `docs/`. The comment at the top of `scripts/rust-perf.ts` lists every option.

## Workloads

The six workloads are those of the 0.3 terminal performance driver: `fresh` (no history), `small` (50 turns), `typical` (500), `tail` (2000), `tools` (500 turns with four tool calls each), and `large-output` (12 turns of 64 KiB answers). [`workloads.ts`](workloads.ts) generates their prompts, reasoning, answers, tool calls, and results; each answer section ends in a unique history marker, and so does the last tool result when the last turn has tools. [`seed-session03.ts`](seed-session03.ts) encodes them as the 0.3 Session log that 0.3's own fixture writer persisted, byte for byte, and [`seed-pi.ts`](seed-pi.ts) as the Pi session the native arm opens with `--session`. The tests pin the hashes of both.

## A sample

Every sample is a fresh process in a private home, user home, workspace, and session copy, in a 120×40 PTY whose shell `exec`s the program after a handshake, so the termios flags are read before the program starts. The faux model ([`model.ts`](model.ts)) answers every OpenAI Responses request with 100 text deltas 10 ms apart (`--pace-ms`); the native arm reaches it through `models.json`, the 0.3 arm through an `llm-pi-ai` route in the invocation patch. The native arm runs `--inline`, which writes resumed history to the terminal as 0.3 does; a fullscreen frame shows only its last rows.

| Metric | Endpoint |
|---|---|
| `inputReadyMs` | Handshake through bracketed paste enabled with the termios flags changed. |
| `startupMs` | Handshake through the composer echo of `PERF_READY`, typed at input readiness: first usable input. |
| `replayMs` | Handshake through the last history marker's first appearance; null for `fresh`. |
| `readyMs` | Handshake through both the echo and every history marker: full-history readiness. |
| `firstDeltaMs` | Enter, which submits `PERF_READY`, through the first streamed delta. |
| `streamInputMs` | `PERF_LIVE_DRAFT` typed after the first delta, through its echo, which must arrive before the last delta. |
| `streamMs` | Enter through the settled turn after the last delta, including the fixed pacing. |
| `idleCpuMs` | CPU time of the process tree over 5 s after the turn settles (`--idle-ms`). |
| `shutdownMs` | SIGTERM through the observed exit; the sample fails unless the exit is 0 or 143 with the termios flags restored. |
| `peakRssMiB` | Largest sum of the tree's `VmRSS`, read every 10 ms from spawn to exit in a worker thread ([`proc-tree.ts`](proc-tree.ts)). Shared pages count once per process. |

Times come from the monotonic clock when the PTY output holding the observed text arrives, not from terminal paint. Output is ANSI-stripped incrementally and never emulated, so a backlog of history cannot delay an observation; neither arm queries the terminal. Every history marker must appear exactly once. Descendants alive after the program exits are recorded as leaked and killed. A failed sample is recorded with its error and never enters a distribution.

## Method

Iterations are outermost; within one, each workload and cache mode runs every arm once, the arm order reversed on odd iterations. A `cold` sample also gets an empty private `TMPDIR`, where Node keeps its compile cache. `warm` samples share one `TMPDIR` per arm, after one discarded warm-up launch per arm and workload. Dropping the kernel page cache needs privileges, so neither mode does it: seeded files and the binary may be cached in both, and the report says so.

The harness refuses to start when the 1-minute load average exceeds `--max-load` (by default a quarter of the CPUs, at least 1) and records each sample's load. Pass `--max-load 1` on a quiet host for a baseline. The report holds the host's kernel, CPU, governor, memory, and Bun version, each arm's build identity (the binary's SHA-256 and version, or the checkout's revision and launcher, application, and lock hashes), the harness's revision and source hash, the method, every sample, and per cell the minimum, median, nearest-rank p95, maximum, unscaled MAD, and 95% percentile-bootstrap intervals of the median and p95 (2000 resamples, fixed seed).

## Budgets

`budgets` reads a baseline report and writes a proposal ([`budgets.ts`](budgets.ts)) to `.preflight/perf/` or `--out`: for each workload and cache mode, each latency's and peak RSS's median and p95 may degrade by at most 10%, and a candidate may leak no process and fail no sample. Idle CPU is reported without a bound. The proposal carries the baseline's intervals and warns about failed or leaking cells and cells under 20 samples. It has `status: "proposed"` and no approval; the owner approves it before any candidate measurement, and only then are budgets recorded in the roadmap, by hand.
