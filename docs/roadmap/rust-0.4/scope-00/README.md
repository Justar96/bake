# Scope 00: support decisions and comparison baseline

## Summary

Scope 00 fixes what the Rust migration must preserve and the TypeScript evidence it will compare against. It belongs to the [Bake 0.4 Rust migration roadmap](../README.md#00--support-decisions-and-baseline); the roadmap's [verification contract](../verification.md#evidence-required-for-every-scope) defines what closes it.

**State: In progress, 2026-10-07.** The inventory and baseline tooling are implemented on `feat/rust-scope-00-baseline`. Nothing here qualifies a Rust behavior. The comparison sources are `v0.3.8` (`dcb26d756e`, whose tree equals `develop` commit `a1ec50245e`) and `origin/develop` at `ae5eb51ab6`.

## Table of Contents

- [Deliverables and status](#deliverables-and-status)
- [What is implemented and what is missing](#what-is-implemented-and-what-is-missing)
- [Run the tooling](#run-the-tooling)
- [Exit criteria](#exit-criteria)
- [Dev Note](#dev-note)

## Deliverables and status

The scope has three deliverables, reviewed in the roadmap's PR order: support decisions first, then the inventory, then the baseline.

| Deliverable | Location | State |
|---|---|---|
| Support and release decisions, acceptance matrix | [support.md](support.md) | Draft written; owner decisions open in [its decision register](support.md#decision-register) |
| Machine-readable inventory of packages, profiles, tools, and tests with their owning scope | [inventory.json](inventory.json), generated and checked by [rust-migration-inventory.ts](../../../../scripts/rust-migration-inventory.ts) | Implemented, with tests for missing, stale, malformed, and unclassified entries |
| Frozen TypeScript baseline record: commits, build provenance, focused oracle results, startup/RSS/shutdown and terminal workloads | [2026-10-07 evidence](baseline-2026-10-07/README.md), produced with [baseline tooling](../../../../scripts/rust-migration-baseline.ts) | Both pinned sources rebuilt, 85 focused tests passed per source, and 36 terminal samples completed; full qualification remains open |

## What is implemented and what is missing

The inventory assigns owning scopes and dispositions to 167 packages, 808 test files, 188 recorded scenarios, all 42 PTY scenarios, three shipped profiles, four presets, 18 tool packages, and six model surfaces. A listed test records an available oracle, not a passing result. The inventory's `gaps` list names missing coverage.

**Implemented:**

- `package.json` names the `gen-rust-migration-inventory`, `verify-rust-migration-inventory`, and `migration:baseline` scripts.
- `scripts/preflight.ts` adds a static `verify-rust-migration-inventory` step that fails when Rust-migration ownership no longer covers the current packages, profiles, tools, and tests.
- `apps/tui/scripts/check-docs.ts` includes `docs/roadmap/` Markdown in `bun run doc-sync` link checking.
- The baseline tool captures source and dependency digests from a clean, pinned worktree. It records each executed command separately and rejects modified source, tampered records, and unsafe output paths. A source record never claims a build or test passed.
- The exact `v0.3.8` revision predates model-surface snapshots. Its source record names that absence; missing inputs in other revisions remain errors.

**Missing before scope 00 can close:**

- Complete baseline qualification beyond the [initial measurements](baseline-2026-10-07/README.md#remaining-evidence): interleaved samples, noise characterization, process-tree memory, shutdown timing, and frozen performance budgets.
- Minimum OS, kernel, libc, and macOS deployment versions for each release target.
- Owner answers to the decisions in [the decision register](support.md#decision-register), including the triage of unreleased `develop` work and the 0.3 support window.
- A test run against the external Bake Desktop consumer, or a named qualification owner for it.

## Run the tooling

Run these commands from the repository root:

```sh
bun run gen-rust-migration-inventory
bun run verify-rust-migration-inventory
bun test scripts/rust-migration-inventory.test.ts scripts/rust-migration-baseline.test.ts
bun run migration:baseline help
```

The inventory check is read-only. Generation requires explicit ownership for new package groups, profiles, presets, tool packages, and PTY scenarios. Review generated changes before committing them.

Capture source provenance from a separate clean worktree at the selected commit. `capture` writes a record, `run` verifies it before and after executing one command, and `check` verifies the record and optional command results. Keep raw command logs under ignored `.preflight/`; commit only reviewed, sanitized evidence. Check-result validation needs the original log beside the result. Exit 1 means an observed command did not pass; exit 2 means invalid inputs or a refused operation.

## Exit criteria

Scope 00 is complete when all of the following hold. Each item maps to the roadmap's [acceptance matrix](../verification.md#acceptance-matrix) row for scope 00.

1. Every row in [the support matrix](support.md#acceptance-matrix) has an owning scope, its current tests, and either a native-parity target, an approved compatibility adapter, or an approved support change.
2. `bun run verify-rust-migration-inventory` passes on a clean checkout and fails on the negative control.
3. The baseline rebuilds from the recorded commit in a clean checkout and reproduces its recorded results within the recorded noise.
4. The product owner has answered every **Owner decision** row in the register, or explicitly deferred it to a named later scope without blocking scope 01.

## Dev Note

The support register owns product decisions. Baseline records and their observed results own execution evidence; inventory counts alone cannot close this scope.
