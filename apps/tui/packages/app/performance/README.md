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
- [Renderer memory](#renderer-memory)
- [Dev Note](#dev-note)

<a id="run"></a>

## Run

Use a checkout with built shared libraries, the Bun version pinned in the root `package.json`, Node, and macOS or Linux. See the [development guide](../../../../../docs/development.md). Run from the repository root, with other CPU-heavy validation stopped:

```sh
bun apps/tui/scripts/tui.ts perf --workload fresh --workload typical --samples 3 --output /tmp/bake-bun-native-production.json
```

The coordinator and native PTY run on Bun; the shared application bundler supplies the measured artifacts. Fixture authoring and every measured Harness process run on the selected Node binary. Each invocation creates private bundles, profiles, workspaces, and session stores; it cleans those resources after success or failure. The built `dsh --profile tui` launch uses the application's actual composition patch and the Harness replay adapter. No installed user profile changes.

`--workload` selects a workload and may repeat; omitting it runs all six workloads. `--node` selects the measured runtime. `--mode production` is the default and matches the built application. `--mode development` selects an unminified development baseline with development React/Ink:

```sh
bun apps/tui/scripts/tui.ts perf --mode development --workload fresh --workload typical --samples 3 --output /tmp/bake-bun-native-development.json
```

`--app-artifacts <directory>` measures saved `index.js` and `startup.js` application bundles with the current fixture writer. The driver copies the bundles into its private application directory so dependencies resolve normally. Supply artifacts built for the selected mode and keep their optional `metadata.json` alongside them to record provenance. Without this option, the driver builds the application from the checkout.

`--cpu-profile <directory>` enables Node CPU profiling for each sample. Profiled timings include profiling overhead and should remain separate from ordinary samples. A timeout, crash, missing historical answer, repeated historical answer, missing final delta, or unclean exit fails the sample. The diagnostic continues remaining samples, updates the JSON report after each one, and exits nonzero if any failed. Ctrl-C or SIGTERM interrupts observations, drains the current process, and stops remaining samples with a nonzero exit and an interrupted report. Inspect each failure before comparing medians.

<a id="measurement-reference"></a>

## Measurement reference

The report's artifact hashes identify the private bundle, startup module, fixture writer, preload, composition, and dependency lock. `applicationSource` distinguishes checkout builds from supplied artifacts and preserves their optional provenance. Only checkout builds report `sourceSha256`; supplied binaries retain their own artifact hashes. Keep built shared libraries stable throughout a comparison; they remain external imports.

| Measurement | Included endpoint and limitations |
|---|---|
| `initialInputMs` | First probe write through its composer echo, after paste-mode registration and before waiting for complete history replay. |
| `firstInputMs` | Spawn through the first observed composer echo. `historyMarkersAtFirstInput` records how much history has arrived at this endpoint. |
| `readyMs` | Spawn through the first composer echo and every expected history marker, including a final tool result when present. Fixture generation and bundling are excluded. |
| `idleInputMs` | Five writes through corresponding composer echoes; the summary takes the median of each sample's maximum. |
| `liveInputMs` | Draft input after the first response delta, through its composer echo. |
| `firstDeltaMs` | Prompt submission through the first visible synthetic delta. |
| `streamMs` | Prompt submission through the final delta and subsequent idle status. Includes intentional 10 ms replay pacing. |
| Memory | `readyMemory` and `settledMemory` capture main-isolate heap before/after forced GC at readiness and after streaming, with session state reachable. Summary `retainedHeapMiB` and `settledRetainedHeapMiB` use the respective post-GC values; worker heaps are excluded. |
| `peakRssMiB` | Main Node process lifetime maximum observed after streaming, including worker threads; excludes the Bun coordinator and separate descendant processes. |
| Output | `initialBytes`, `idleBytes`, and `streamBytes` count PTY bytes through readiness, subsequent idle typing, and streaming. `historyMarkerOccurrences` counts history markers beyond the bounded capture tail. |

The driver latches paste-mode registration as bytes arrive, including split escape sequences, so registration survives eviction from the bounded output tail. First input can precede complete replay; readiness additionally requires every history marker. Marker counts and identities remain observable throughout the sample.

The terminal viewport is 120 columns by 40 rows. Timing uses the coordinator's monotonic clock and a 2 ms observation poll; PTY byte arrival is not terminal-emulator paint or keyboard hardware latency. Every sample uses a fresh process and private files; filesystem caches are not flushed. The 1 GiB V8 heap cap is a diagnostic resource constraint, not a product limit. GC sampling is outside timed input intervals and can affect later allocation behavior. These measurements exclude real inference/network latency, interactive session-picker listing, repeated navigation, resizing, compaction, and image admission.

<a id="workloads-and-assertions"></a>

## Workloads and assertions

The [fixture author](history.ts) constructs fixed Session events through Harness APIs. Each historical turn contains a multilingual prompt, reasoning, text, and compact stream deltas. The default mix adds a tool call and 128 lines of result text every fourth turn; `tools` adds four calls per turn. Tools are historical synthetic records, not executions. A separate tiny replay fixture supplies one 100-delta response so replay-adapter setup does not parse the measured history a second time.

| Workload | Completed turns | Tools | History markers | Purpose |
|---|---:|---:|---:|---|
| `fresh` | 0 | 0 | 0 | Profile startup baseline |
| `small` | 50 | 12 | 50 | Short-session resume |
| `typical` | 500 | 125 | 501 | Mixed long-history resume |
| `tail` | 2000 | 500 | 2001 | Large-history transient-allocation pressure |
| `tools` | 500 | 2000 | 501 | Dense tool calls and results |
| `large-output` | 12 | 3 | 193 | 64 KiB assistant answers |

Each ordinary answer ends in a unique history marker. Each `large-output` answer contains sixteen 4096-byte sections with one marker per section, covering all 64 KiB. If the last turn has tools, its final tool result adds one marker after the result text, so readiness includes trailing tool output. Every expected marker must appear exactly once across resume, typing, and streaming.

These are synthetic workload labels, not claims about observed user-session distributions. Reports include exact event, delta, compact-record, and tool counts; UTF-8 byte totals for prompts, answers, reasoning, and tool output; maximum answer size; marker counts; and persisted byte size. Every successful sample must observe the final model delta, return to idle, and exit cleanly. [Fixture tests](../tests/performance.spec.ts) verify deterministic replay and failure aggregation; [PTY lifecycle tests](../tests/performance-terminal.test.ts) cover readiness after output-tail eviction, failed exits, fatal signals, cancellation, and teardown after timeout.

<a id="renderer-memory"></a>

## Renderer memory

The [renderer regression](../../ui/tests/memory.spec.ts) runs installed production React and Ink in a fresh Node process with a 192 MiB heap cap. It checks memory after explicit GC with the renderer alive and again after cleanup. Output goes to a counting sink with a 1024-character tail, so test capture cannot retain the rendered history.

```sh
NODE_ENV=production node --expose-gc --max-old-space-size=192 apps/tui/packages/ui/tests/fixtures/ink-memory.mjs
```

| Field | Measurement contract |
|---|---|
| Operation | Replace one real Ink `Text` 3000 times, awaiting each render flush; revisit the first value and clean up. |
| Workload | Unique frame marker plus 16 KiB of fixed synthetic tool-like text, truncated in a 100-column terminal. No network, model, Session log, or saved transcript. |
| Memory | GC after warmup, each 1000 updates, and cleanup. Retained heap growth must remain below 16 MiB; timing has no pass threshold. |
| Behavior | Final and revisited text reach the terminal. Existing component and PTY scenarios own full application rendering and terminal restoration. |
| Negative control | Unpatched Ink 7.1.1 fails the same memory assertion after the first 1000 updates. |

[Three samples per implementation](results/ink-cache-memory.json), collected on Linux x64 with Node 26.7.0 on 2026-09-26, give median retained heap of 115.7 MiB before versus 16.6 MiB after the cache patch at 3000 updates. Median elapsed time, including forced GC and cleanup, is 1783 ms versus 1428 ms. This measures renderer cache retention and local rendering cost; it does not establish model latency or a bound on durable session history. Each cache keeps at most 1000 entries and 1,048,576 UTF-16 code units of keys and string values. Large individual values remain renderable without being cached.

The cache-patch checkout, before bounded history replay, also completed [three built-profile samples each](results/ink-cache-profile-linux.json) for fresh, 500-turn, and 2000-turn histories, including streaming, input, and clean exit. Median retained heap was 54.6, 65.0, and 87.5 MiB, respectively. The 2000-turn median peak RSS was 985.2 MiB, showing substantial transient replay cost in that implementation. These Linux results are not a before/after comparison with the historical macOS results below. The diagnostic accepts Bun 1.4.2's Linux PTY close status only after a verified successful child exit; live read failures, failed exits, and signals remain errors, covered by the [driver tests](../tests/performance-terminal.test.ts).

<a id="dev-note"></a>

## Dev Note

The historical Bun-native comparison uses the same source hash, viewport, fixtures, and three samples per workload: [development](results/bun-native-development.json) and [production](results/bun-native-production.json). Production reduces the plugin from 104,323 to 58,820 bytes. The 500-turn readiness median drops from 6197 to 5114 ms (17.5%), and peak RSS from 909.4 to 804.8 MiB (11.5%). Retained heap is 58.1 versus 56.4 MiB; live input is 2.14 versus 2.26 ms, near the 2 ms observation interval. Fresh readiness is 1321 versus 1281 ms. These measurements support production React selection, not a claim that Bun executes Harness faster. The [native-driver tail sample](results/bun-native-tail.json) failed at 2000 turns under the 1 GiB cap; the driver recorded Node's SIGABRT and heap-exhaustion message.

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

The historical 500-turn fixture contains 3251 durable events, 20,500 deltas, 125 tools, and 3,561,500 persisted bytes. The historical 2000-turn fixture contains 13,001 events, 82,000 deltas, 500 tools, and 14,264,383 bytes. A separate [CPU profile summary](results/cpu-profile-summary.json) contains substantial ANSI diff, garbage collection, Yoga, Ink output, and development React work. It covers the whole process rather than isolating resume phases.

<a id="history-replay-measurements"></a>

### History replay measurements

The current [history presenter](../../ui/src/replay.ts) limits each batch by presentation lines, terminal rows, and text size; a single oversized line can progress alone. During backlog replay, [Scrollback](../../ui/src/scrollback.tsx) waits for each render flush before admitting the next batch and preserves ordered output through one persistent Ink `Static` instance. Complete history remains in the session; the batching limits presentation work. Compare implementations with the same six workloads, seed artifact, driver, runtime, and dependencies, using first-input and complete-replay endpoints separately. The historical results above retain their original fixtures and endpoints.

[Before](results/history-replay-before.json) and [after](results/history-replay-after.json) each contain three successful samples for all six workloads, measured on Linux x64 with Node 26.7.0 and Bun 1.4.2 on 2026-09-26. Both application builds use one frozen source snapshot; the baseline restores whole-history Static admission and uncached line wrapping. Their effective source hashes differ only in `app.tsx` and `line.tsx`. Seed, startup, composition, and lock hashes match. The candidate artifact is identical to the built application used by the 26 passing PTY scenarios. The [isolation record](results/history-replay-comparison.json) also verifies 988 unchanged external runtime files.

| Workload | First input, ms before → after | Complete replay, ms before → after | Peak RSS, MiB before → after |
|---|---:|---:|---:|
| `fresh` | 777 → 756 | 777 → 756 | 220.1 → 221.2 |
| `small` | 938 → 879 | 938 → 879 | 230.5 → 236.5 |
| `typical` | 1933 → 1050 | 1933 → 1888 | 428.9 → 331.4 |
| `tail` | 5306 → 1207 | 5306 → 4889 | 986.8 → 377.5 |
| `tools` | 4115 → 1164 | 4115 → 3951 | 804.7 → 365.9 |
| `large-output` | 1274 → 926 | 1274 → 1281 | 324.5 → 270.8 |

The 2000-turn session accepts input 77% earlier and uses 62% less peak RSS; complete replay is 8% faster. Tool-heavy history accepts input 72% earlier and uses 55% less peak RSS; complete replay is 4% faster. The large-answer completion median is essentially unchanged. Fresh and short histories show no comparable memory reduction. These are local medians without calibrated timing budgets; model and network latency are excluded.

A probe sent during replay takes 87–133 ms in the resumed large workloads, while idle and streaming input remains about 35–39 ms. The unbatched renderer's first probe arrives only after history has already blocked startup. Tail retained heap after forced GC is 87.4 → 81.8 MiB, so most of the memory improvement is temporary layout allocation. Session state and saved terminal output still grow with history. Startup output grows about 3–7% on larger workloads. First-delta medians increase from 78.5 to 103.0 ms for fresh sessions and 78.4 to 95.1 ms for typical histories; these local submission-to-delta costs include the synthetic adapter.

The batch admits at most 512 presentation lines, 1024 terminal rows, and 128 Ki UTF-16 text units, with one oversized line allowed alone. A weak cache shares each immutable line's placement and wrapping between measurement and rendering; a width change replaces its sole entry. [Exploratory samples](results/history-replay-tuning.json) show the smaller batches considered during tuning; they precede the frozen comparison and are not pooled with it.

[Cursor tests](../../ui/tests/replay.test.ts) check linear traversal, append ordering, complete large answers, and the three admission bounds. [Ink tests](../../ui/tests/scrollback.spec.tsx) hold stdout delivery to check input and cancellation while replay waits, then verify ordered output, resize, navigation, and child inspection with draft restoration. Temporarily removing all three admission bounds makes the multiline-answer regression fail because all 1000 lines print before the first flush barrier; restoring them passes. The 203 focused Node tests, 17 Bun tests, types, layout checks, and 26 built-profile PTY scenarios passed. No real model call or prolonged production session is part of this comparison.
