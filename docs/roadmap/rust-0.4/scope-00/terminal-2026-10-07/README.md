# Terminal shutdown measurements: 2026-10-07

## Summary

All 18 synthetic terminal samples completed with the new shutdown timer, and the clean checkout passed its build and 19 focused diagnostic tests. This record extends [scope 00](../README.md) with shutdown latency and per-workload distributions. It leaves the [initial comparison record](../baseline-2026-10-07/README.md) intact. These are Linux diagnostic observations, not performance budgets or Rust qualification.

## Table of Contents

- [Source and checks](#source-and-checks)
- [Shutdown observations](#shutdown-observations)
- [Reproduce and verify](#reproduce-and-verify)
- [Remaining evidence](#remaining-evidence)

## Source and checks

The measured source is `c45b16127d3803cc2f656fce2be052225c86d058`, based on the merged scope-00 PR at `39ee17ecbd434cdaa1acdf032e2fdb5c0e3446d9`. The additional changes affect the diagnostic, its tests, and roadmap documentation. They do not change the measured application source. The report identifies a clean checkout, Bun 1.4.2, Node v26.10.0, production bundles, and a 120-column by 40-row PTY. [Host metadata](host.json) records the Linux x64 machine and CPU.

| Evidence | Observed result |
|---|---|
| [Source provenance](source.json) | Clean pinned source; `sourceDigest` is `31e4d756539098852cad9bba30621c8dea03885febf462c1896d33bf670f92dd` |
| [Frozen install](install.json) | Passed |
| [Build](build.json) | Passed |
| [Terminal diagnostic tests](terminal-tests.json) | 12 passed |
| [Report and fixture tests](report-tests.json) | 7 passed |
| [Terminal diagnostic command](terminal-performance.json) | All six workloads, three samples each; 18 completed, no failures or interruption |

The source record identifies inputs and intentionally keeps `qualification: not-evaluated`. Separate command records report the executed outcomes. Their source checks and raw-log digests passed verification after the run.

## Shutdown observations

`shutdownMs` starts when the coordinator sends the second Ctrl-C and ends when Bun observes the Node process exit. It excludes the first interrupt's confirmation wait and includes process-exit notification latency. It does not measure descendant-process drain or the final PTY close. A successful sample requires a clean exit; an exit observed before measurement starts, a nonzero status, or a fatal signal cannot contribute a duration.

Each sample uses a fresh process and private synthetic history. Filesystem caches are warm; no model or network latency is included. No other repository build or test job ran during measurement. Workloads ran in the same fixed order within each of three iterations. This is one TypeScript source, without an interleaved comparison arm.

| Workload | Completed | Minimum, ms | Median, ms | p95 and maximum, ms | MAD, ms |
|---|---:|---:|---:|---:|---:|
| Fresh | 3 | 45.10 | 47.43 | 49.57 | 2.13 |
| Small | 3 | 48.11 | 49.75 | 52.99 | 1.63 |
| Typical | 3 | 54.24 | 54.38 | 55.58 | 0.14 |
| Tail | 3 | 57.08 | 59.90 | 82.92 | 2.82 |
| Tools | 3 | 57.46 | 57.50 | 57.56 | 0.04 |
| Large output | 3 | 49.00 | 49.94 | 49.96 | 0.02 |

The nearest-rank p95 is the maximum with three samples. MAD is the unscaled median absolute deviation. Neither statistic establishes a confidence interval or a portable latency bound. The [complete report](performance.json) retains individual samples, other latency distributions, workload dimensions, main-process memory, and artifact hashes. Its SHA-256 is `b3e116da616da635064e7770db9ceb820c7c7ece547aa6ba81e220677b6dd52f`.

## Reproduce and verify

Use a clean worktree at the exact commit above. Run `bun install --frozen-lockfile`, `bun run build`, and the argument lists in the command records through the [baseline runner](../../../../../scripts/rust-migration-baseline.ts). The terminal command is `bun apps/tui/scripts/tui.ts perf --samples 3 --output <ignored-report-path>`; choose an output outside committed evidence. Keep all raw observations, including failures.

The original clean worktree is under `.preflight/rust-next/baseline`; raw records and logs are under `.preflight/rust-next/evidence/`. The baseline runner's `check` command verified source plus install, build, terminal tests, report tests, and terminal-performance results there. Checking a command record requires its original raw log beside it. A fresh checkout must rerun commands to recreate those logs. Preserve this committed evidence and put subsequent runs in new records.

## Remaining evidence

Scope 00 still needs the [full performance qualification](../../verification.md#performance-and-soak): interleaved comparison arms, more samples, confidence analysis, cold-cache conditions, process-tree memory, idle CPU, interactive agent/resize workloads, and frozen budgets. Node 24 and the other release targets remain unqualified by this record. Terminal restoration needs its own PTY assertions; a clean process exit alone does not prove it. Product support decisions remain open in the [decision register](../support.md#decision-register).
