# TypeScript baseline: 2026-10-07

## Summary

Both pinned TypeScript revisions install with the frozen lockfile, build, pass 85 focused runtime tests, and complete 18 synthetic terminal samples on Linux x64. These records start [scope 00](../README.md); they do not close it or establish Rust parity. Source records identify inputs, command records report observed outcomes, and performance reports retain every sample and the measured artifact hashes.

## Table of Contents

- [Sources and results](#sources-and-results)
- [Terminal measurements](#terminal-measurements)
- [Reproduce and verify](#reproduce-and-verify)
- [Remaining evidence](#remaining-evidence)

## Sources and results

| Source | Source record | Install | Build | Runtime oracles | Terminal diagnostic |
|---|---|---|---|---|---|
| `v0.3.8`, `dcb26d756e008a3d17c057a6b153933c018c275d` | [source.json](v0.3.8/source.json) | [Passed](v0.3.8/install.json) | [Passed](v0.3.8/build.json) | [85 passed, 7 files](v0.3.8/runtime-oracles.json) | [18 passed](v0.3.8/terminal-performance.json) |
| `develop`, `ae5eb51ab61f2266b0fc2ff52f2f14b9f3a9a917` | [source.json](develop/source.json) | [Passed](develop/install.json) | [Passed](develop/build.json) | [85 passed, 7 files](develop/runtime-oracles.json) | [18 passed](develop/terminal-performance.json) |

Each command ran in a separate clean detached worktree and left its tracked source unchanged. The seven runtime files exercise request reconstruction, tool ordering, shutdown drain, two-process session leases, Zstandard compatibility, Desktop transport, and SSE parsing. The command records contain their exact argument lists. These focused tests do not cover every inventoried behavior.

The `v0.3.8` tree has no model-surface snapshots. Its source record explicitly lists both absent patterns and their reason. Only that exact commit receives the exemption; missing inputs in other revisions fail capture. The source records intentionally keep `qualification: not-evaluated` and point to missing evidence even when a separate command record passes.

## Terminal measurements

The existing [terminal diagnostic](../../../../../apps/tui/packages/app/performance/README.md) ran all six workloads with three samples each. The [host record](host.json) identifies Linux x64, an Intel Core i9-14900KS, 32 logical CPUs, and 202,261,168,128 bytes of RAM. Both arms used Bun 1.4.2, Node v26.10.0, production bundles, and a 120-column by 40-row PTY. Repository-owned builds and test suites had stopped before measurement.

Each sample launches a fresh Node process. Filesystem caches are warm, synthetic replay supplies model output, and histories contain no user data. The develop arm ran first, followed by the release arm; the arms were not interleaved. An earlier release run overlapped tooling checks and is excluded; its raw output remains in the ignored `v0.3.8/pilot/` directory. This is an initial diagnostic, not a statistically qualified version comparison or an accepted performance budget.

Readiness means the first composer echo and every expected history marker have arrived. RSS is the median of each sample's main Node process peak after streaming; it includes worker threads and excludes separate descendants and the Bun coordinator. Parenthesized ranges show the smallest and largest readiness sample, not a confidence interval.

| Workload | v0.3.8 readiness, ms: median (range) | Develop readiness, ms: median (range) | v0.3.8 peak RSS, MiB | Develop peak RSS, MiB |
|---|---:|---:|---:|---:|
| Fresh | 512.7 (512.2–524.1) | 514.1 (508.7–637.6) | 227.2 | 225.2 |
| Small | 636.5 (615.1–644.6) | 619.8 (618.9–622.9) | 232.9 | 234.7 |
| Typical | 1228.6 (1220.3–1272.4) | 1239.0 (1235.4–1249.1) | 302.1 | 304.3 |
| Tail | 3228.2 (3160.4–3738.5) | 3159.5 (3121.6–3211.7) | 374.4 | 375.6 |
| Tools | 2692.3 (2673.4–3600.4) | 2663.8 (2639.2–2754.6) | 354.1 | 374.5 |
| Large output | 802.9 (755.3–1023.6) | 759.7 (752.7–766.6) | 239.1 | 241.1 |

The [release report](v0.3.8/performance.json) and [develop report](develop/performance.json) contain all 36 samples, workload dimensions, input/stream timings, memory counters, and artifact hashes. Their SHA-256 digests are `9e169bc2b44fc1e70139e59d60a158a84aaea6bc3597b44948090c2c6dfaef68` and `1c9fffaec5938f5b76ff772b428b5d123dc05ae9239fc7b0e680209041da388d`, respectively. No sample failed or was interrupted; successful samples require a clean exit. Shutdown duration itself is not measured.

## Reproduce and verify

Use clean detached worktrees at the exact commits above. Install and build with `bun install --frozen-lockfile` and `bun run build`. The [baseline command](../../../../../scripts/rust-migration-baseline.ts) runs from the current checkout and accepts `--root` for either historical worktree. Capture its source with `capture --root <worktree> --out <ignored-record> --expect-commit <sha>`, then execute each command through `run --root <worktree> --record <ignored-record> --out <ignored-result> --id <name> -- <argv...>`. The JSON command records supply the executed argument lists; each `terminal-performance` record invokes `bun apps/tui/scripts/tui.ts perf --samples 3` with an output path under ignored `.preflight/`.

The original raw logs remain under `.preflight/rust-scope-00/evidence/{develop,v0.3.8}/`. `check --root <worktree> --record <source.json>` verifies source without those logs. Adding `--result <result.json>` also requires its original log beside it and checks the log digest, source identity, and outcome. A fresh checkout must rerun commands to obtain its own logs and results; a committed result alone does not supply a raw log. Keep committed evidence unchanged and put later measurements in a new dated record.

## Remaining evidence

- Twenty interleaved samples per arm/workload, cold-cache conditions, p50/p95 confidence analysis, and frozen budgets remain open.
- Process-tree peak RSS, idle CPU, shutdown timing, prolonged soak, navigation, resize, compaction, and attachment workloads are not measured here.
- Node 24, Linux arm64, macOS, Windows, minimum supported OS/libc versions, release archives, and the external Bake Desktop consumer remain unqualified. The baseline runner's descendant cleanup currently uses POSIX process groups; these Linux results do not prove Windows cleanup.
- Product support decisions remain open in [the decision register](../support.md#decision-register). No release routing or compatibility policy changes follow from these records.
- No live provider calls, paired model evals, or Rust execution occurred. This change only adds migration documentation and developer tooling.
