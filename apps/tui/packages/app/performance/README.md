---
description: "Measure built TUI startup, long-session resume, input latency, and process memory with synthetic histories."
---

# Terminal performance diagnostic

English | [中文](README.zh.md)

## Summary

Measure a fresh TUI process, resume mixed histories, and type during a paced response without an API key or user data. Reports retain individual samples, per-workload medians, and failures. This local diagnostic has no calibrated timing thresholds and does not qualify CI performance.

## Table of Contents

- [Run](#run)
- [Measurement reference](#measurement-reference)
- [Workloads and assertions](#workloads-and-assertions)
- [Dev Note](#dev-note)

<a id="run"></a>

## Run

Use a checkout with built upstream libraries, Bun 1.4.3, Node, and macOS or Linux. See the [build prerequisites](../../../PLAN.md#132-build-from-a-clean-checkout). Run from the repository root, with other CPU-heavy validation stopped:

```sh
./tui/scripts/tui.ts perf --workload fresh --workload typical --samples 3 --output /tmp/bake-bun-native-production.json
```

The coordinator and native PTY run on Bun; the shared application bundler supplies the measured artifacts. Fixture authoring and every measured Harness process run on the selected Node binary. Each invocation creates private bundles, profiles, workspaces, and session stores; it cleans those resources after success or failure. The built `dsh --profile tui` launch uses the application's actual composition patch and the Harness replay adapter. No installed user profile changes.

`--workload` selects a workload and may repeat. `--node` selects the measured runtime. `--mode production` is the default and matches the built application. `--mode development` selects an unminified development baseline with development React/Ink:

```sh
./tui/scripts/tui.ts perf --mode development --workload fresh --workload typical --samples 3 --output /tmp/bake-bun-native-development.json
```

`--cpu-profile <directory>` enables Node CPU profiling for each sample. Profiled timings include profiling overhead and should remain separate from ordinary samples. A timeout, crash, missing historical answer, repeated historical answer, missing final delta, or unclean exit fails the sample. The diagnostic continues remaining samples, updates the JSON report after each one, and exits nonzero if any failed. Ctrl-C or SIGTERM interrupts observations, drains the current process, and stops remaining samples with a nonzero exit and an interrupted report. Inspect each failure before comparing medians.

<a id="measurement-reference"></a>

## Measurement reference

The report's artifact hashes identify the private bundle, startup module, fixture writer, preload, composition, and dependency lock. Keep upstream built libraries stable throughout a run; they remain external imports.

| Measurement | Included endpoint and limitations |
|---|---|
| `readyMs` | Spawn through paste-mode registration and observed composer echo, including history replay. Fixture generation and bundling are excluded. |
| `idleInputMs` | Five writes through corresponding composer echoes; the summary takes the median of each sample's maximum. |
| `liveInputMs` | Draft input after the first response delta, through its composer echo. |
| `firstDeltaMs` | Prompt submission through the first visible synthetic delta. |
| `streamMs` | Prompt submission through the final delta and subsequent idle status. Includes intentional 10 ms replay pacing. |
| Memory | Main-isolate heap before/after forced GC at readiness and after streaming, with session state reachable. Summary retained heap uses readiness after GC; worker heaps are not included. |
| Peak RSS | Main Node process lifetime maximum, including worker threads; excludes the Bun coordinator and separate descendant processes. |
| Output | Total PTY bytes for startup, idle typing, and streaming; history marker counts cover output beyond the bounded capture tail. |

The terminal viewport is 120 columns by 40 rows. Timing uses the coordinator's monotonic clock and a 2 ms observation poll; PTY byte arrival is not terminal-emulator paint or keyboard hardware latency. Every sample uses a fresh process and private files; filesystem caches are not flushed. The 1 GiB V8 heap cap is a diagnostic resource constraint, not a product limit. GC sampling is outside timed input intervals and can affect later allocation behavior. These measurements exclude real inference/network latency, interactive session-picker listing, repeated navigation, compaction, and image admission.

<a id="workloads-and-assertions"></a>

## Workloads and assertions

The [fixture author](history.ts) constructs fixed Session events through Harness APIs. Each historical turn contains a multilingual prompt, reasoning, text, and compact stream deltas; every fourth turn adds a tool call and 128 lines of result text. Tools are historical synthetic records, not executions. A separate tiny replay fixture supplies one 100-delta response so replay-adapter setup does not parse the measured history a second time.

| Workload | Completed turns | Purpose |
|---|---:|---|
| `fresh` | 0 | Profile startup baseline |
| `small` | 50 | Short-session resume |
| `typical` | 500 | Mixed long-history resume |
| `tail` | 2000 | Large-history transient-allocation pressure |

These are synthetic workload labels, not claims about observed user-session distributions. Reports include exact event, delta, tool, and persisted-byte counts. Every historical answer must appear exactly once across resume, typing, and streaming. Every successful sample must observe the final model delta, return to idle, and exit cleanly. [Fixture tests](../tests/performance.spec.ts) verify deterministic replay and failure aggregation; [PTY lifecycle tests](../tests/performance-terminal.test.ts) cover failed exits, fatal signals, cancellation, and teardown after timeout.

<a id="dev-note"></a>

## Dev Note

The current Bun-native comparison uses the same source hash, viewport, fixtures, and three samples per workload: [development](results/bun-native-development.json) and [production](results/bun-native-production.json). Production reduces the plugin from 104,323 to 58,820 bytes. The 500-turn readiness median drops from 6197 to 5114 ms (17.5%), and peak RSS from 909.4 to 804.8 MiB (11.5%). Retained heap is 58.1 versus 56.4 MiB; live input is 2.14 versus 2.26 ms, near the 2 ms observation interval. Fresh readiness is 1321 versus 1281 ms. These measurements support production React selection, not a claim that Bun executes Harness faster. The [native-driver tail sample](results/bun-native-tail.json) still fails at 2000 turns under the 1 GiB cap; the driver records Node's SIGABRT and heap-exhaustion message.

A [single Node 24 production sample](results/bun-native-node24.json) completed 500-turn resume in 5540 ms with 857.5 MiB peak RSS and 2.27 ms live-input observation. Built replay, navigation, attachments, and terminal restoration also pass on Node 24 and 26. The earlier script-based measurements below retain their own artifacts and driver context; their samples are not pooled with the Bun-native comparison.

Local evidence from macOS x64 on 2026-09-22: [Node 26 development](results/node26-development.json), [Node 26 production](results/node26-production.json), and [Node 24 development](results/node24-development.json). JSON files retain artifact hashes and unchanged numeric samples; native failure stacks are omitted. These are diagnostic observations, not portable performance budgets. Each Node 26 row below reports medians from three successful samples; the tail reports failures instead.

| Mode / turns | Ready, ms | Maximum idle input, ms | Live input, ms | Retained heap, MiB | Peak RSS, MiB |
|---|---:|---:|---:|---:|---:|
| Development / 0 | 1231 | 4.90 | 2.08 | 46.1 | 219.8 |
| Development / 50 | 1879 | 2.21 | 2.10 | 48.2 | 297.8 |
| Development / 500 | 5962 | 5.06 | 2.08 | 58.1 | 908.8 |
| Production / 0 | 1223 | 4.87 | 2.18 | 45.9 | 215.1 |
| Production / 50 | 1652 | 2.17 | 2.14 | 47.8 | 267.2 |
| Production / 500 | 4946 | 4.72 | 2.21 | 56.4 | 800.0 |
| Either mode / 2000 | Heap exhaustion in 3/3 | — | — | — | — |

The single Node 24.21.0 development sample reached readiness in 1294 ms for a fresh session and 6649 ms for 500 turns, with 1000 MiB peak RSS for the latter. Its 2000-turn sample also exhausted the heap before input registration. This is a runtime compatibility observation, not a repeated Node-version comparison.

The 500-turn fixture contains 3251 durable events, 20,500 deltas, 125 tools, and 3,561,500 persisted bytes. The 2000-turn fixture contains 13,001 events, 82,000 deltas, 500 tools, and 14,264,383 bytes. A separate [CPU profile summary](results/cpu-profile-summary.json) contains substantial ANSI diff, garbage collection, Yoga, Ink output, and development React work. It covers the whole process rather than isolating resume phases.

The layout follow-up is to bound initial history rendering allocations while preserving ordered, complete scrollback and one emission per answer. The current whole-history replay has a much larger transient footprint than its retained heap. Production bundling is a measured candidate, with about 17% lower 500-turn readiness time and 12% lower peak RSS here; it does not resolve the tail failure. The shared application build adopts production compilation and runtime selection. Layout code remains outside this diagnostic. Re-run the same workloads after a rendering change before claiming long-history qualification.
