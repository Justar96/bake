# Rust migration: verification and scope evidence

## Summary

Each [roadmap scope](README.md#linear-development-sequence) closes only when an independent observer can reproduce its behavior, failure handling, and cleanup from identified commits and artifacts. This document defines that evidence and the comparison rules. Future test requirements below are planned work; only [planning-session evidence](#planning-session-evidence) records checks executed for this roadmap.

## Table of Contents

- [Evidence required for every scope](#evidence-required-for-every-scope)
- [Comparison architecture](#comparison-architecture)
- [Acceptance matrix](#acceptance-matrix)
- [Platform requirements](#platform-requirements)
- [Evals and model-facing fidelity](#evals-and-model-facing-fidelity)
- [Performance and soak](#performance-and-soak)
- [Reliable test construction](#reliable-test-construction)
- [Planning-session evidence](#planning-session-evidence)
- [Dev Note](#dev-note)

## Evidence required for every scope

Scope 01 must implement a machine-readable evidence ledger and validation for it. Use one record per scope qualification attempt, with links from the scope's PR; failed attempts remain visible. Existing eval records retain their own schema and immutable-history policy. The following fields are a proposed record shape, not an existing command or verifier.

```yaml
scope: "09"
status: evidence-review
base_commit: <exact PR-base SHA>
candidate_commit: <exact candidate SHA>
oracle_commit: <frozen TypeScript SHA>
fixture_digest: <hash of shared inputs and expected external outcomes>
artifact_digests: []
support_entries: []
commands: [] # argv, working directory, OS/arch, tool versions, exit code, counts
results: [] # behavior ID, pass/fail/skip, test owner, report location
negative_controls: [] # deliberate defect and observed failing assertion
missing_evidence: [] # unavailable platform, credentials, consumer, or case
eval_records: []
performance_records: []
rollback_result: <observed outcome or not applicable with reason>
review: <reviewer and accepted evidence reference>
```

Every scope must supply:

1. **Behavior inventory:** existing entry points and expected success/failure outcomes, including optional configurations it affects. Link each to native tests and the old implementation or an explicit approved change.
2. **Deterministic tests:** unit/property cases for pure logic, plus an integration case through the real composed runtime for product-visible behavior. A test suite's filename count is not coverage.
3. **Independent observations:** inspect requests, durable events, files, process exits, terminal modes, or protocol messages. Do not accept a model's claim that a task or check succeeded.
4. **Negative control:** demonstrate that a relevant wrong implementation or malformed input fails the intended top-level check. Record the temporary mutation and failure; do not leave it in production code.
5. **Resource and failure proof:** cancellation, timeouts, partial startup, malformed input, and teardown where the scope owns those risks. Race tests must force the ordering under test.
6. **Exact provenance:** commits, fixture hashes, artifact hashes, platform/toolchain, commands, test counts, failures, warnings, and skipped tests. Logs with transcripts/credentials stay private; committed summaries must be sanitized.
7. **Rollback:** demonstrate recovery at the scope's actual storage/install boundary, or explain why the scope only reads immutable inputs.
8. **Relevant release gates:** current Bake preflight for PRs, native checks once implemented, PTYs for terminal changes, and paired evals for model-visible changes.

“Complete” requires all required results passing or a separately approved change to the support contract. A skipped required platform, unavailable credential, unknown consumer, or unimplemented comparator is missing evidence. A rerun that passes after a load-related failure remains reported as a warning until explained.

## Comparison architecture

Use two independently built executables and a third test driver. Both arms receive identical fixtures in separate private homes/workspaces, the same deterministic external responses, and the same operation sequence. Observe what each arm sends and changes; never run them as competing writers to one workspace except in an intentional locking test.

The driver must compare more than the final assistant reply:

| Observation | Comparison rule |
|---|---|
| System prompts, context strings, tool names/descriptions/results/errors | Exact bytes after narrowly defined fixture substitutions |
| Model request structure | Exact values and ordered arrays, omitted/null distinction, tool order, request count; compare provider-neutral and encoded provider forms separately |
| Provider wire body | Preserve prompt/schema order where cache or protocol behavior depends on it; parse JSON only where object-key order is demonstrably irrelevant |
| Durable events | Exact type/data/order/references; IDs normalized by one stable bijection across the entire run |
| Historical generation | Original bytes unchanged; decoded logical data and admission/refusal match; no requirement for new compressed bytes to match another compressor |
| Files and commands | File bytes, permissions where applicable, protected-fixture hashes, real check exit, timeout and signal recorded separately |
| Terminal | Emulator cells, cursor position, terminal modes, scrollback markers, input submission and exits; identical ANSI instruction sequences are unnecessary |
| Cleanup | All owned processes/tasks/connections terminate, locks release, no callback mutates state after close |

Normalize generated UUIDs, fixture-root paths, captured clock values, and allocated loopback ports only through an allowlist. Keep identity relationships, event ordering, timeout decisions, and model-visible text intact. Do not sort arrays, drop unknown fields, erase errors, normalize all whitespace, or ignore “extra” model calls to make a comparison pass. Compare durations as measurements, not by rewriting them to fixed values.

Generate or extract neutral fixtures using the real TypeScript runtime. Expected filesystem outcomes and scenario predicates come from an independent test specification. The same new Rust implementation must not generate both its own input and expected result. Retain the current original test alongside an extracted fixture until its behavior has equivalent coverage.

For model-visible scopes before the complete native loop exists, scope 01's eval-arm adapter must exercise the changed component in a representative composed arm. If that is not yet possible, keep the implementation experimental and the scope open; do not declare evals unnecessary because the final binary is unfinished.

## Acceptance matrix

Each row supplements the detailed scope specification. The final column is a required falsification or fault case, not a test reported as already run.

| Scope | Required observable evidence | Required failure/control |
|---|---|---|
| 00 | Every shipped behavior and existing test has a disposition; reproducible baseline artifacts and results | Remove a supported profile/tool from the inventory and make the inventory check fail |
| 01 | Clean native builds and two-arm fixture/eval driver reports | Alter prompt, ordering, permission result, and final file independently; each is rejected |
| 02 | Historical logical replay, surfaces, headers, inbox, forks and reconstructable requests | Reject unknown required event, invalid reference, and sequence corruption |
| 03 | Both write/read directions, immutable generations, real leases and recovered tails | Crash during append/publication; TS/Rust writer contention; damaged compressed frame |
| 04 | Real filesystem confinement and no surviving owned descendants on each OS | Escape attempt, missing backend, ignored termination, and timeout with exit 0 |
| 05 | Layer precedence, profile migration, scope visibility, credential storage and disposal | Failed registration/reload, unsupported `!!js`, secret prompt cancellation and attempted secret leakage |
| 06 | Equivalent stream assembly, finish/failure mapping and usage | Split chunk, malformed stream, abrupt EOF, consumer exception, cancellation at every stream phase |
| 07 | Approved provider/auth matrix, sanitized request fixtures and live route smokes | Expired credentials, failed refresh, malformed tool arguments, idle timeout, invalid route override |
| 08 | Exact tool schemas/results and independently checked coding effects | Stale edit, denied call, out-of-order completion, exclusive barrier, late progress |
| 09 | Native edit/check/resume, event trace parity and paired eval | Cancel before admission and during tools; crash after effect but before result commit |
| 10 | Exact composed context, image admission, reconstructable compaction and long-session task | Missing attachment, failed summary, orphaned tool pair, canceled checkpoint, byte-limit edge |
| 11 | Durable child/job/goal/schedule results and shutdown | Parent/child race, duplicate wakeup, clock jump, router failure, restart at settlement |
| 12 | MCP/hooks/web/VM and approved extension path through real runtime | Disconnect, invalid schema, private-address redirect, infinite loop, memory limit, denied nested tool |
| 13 | Built CLI flags/exits, public transport behavior and actual Desktop launch | Invalid envelope, dropped client, withdrawn approval, busy session, incompatible Electron entry |
| 14 | Inline/fullscreen terminal cells, Unicode input, modes and restoration | Resize mid-stream, panic, hangup, malformed escape/input, constrained dimensions |
| 15 | Complete PTY workflow map and all preset model surfaces | Cancel navigation/login/question; retain draft; deny approval; external-editor failure |
| 16 | Signed fresh install, old-to-new update, no-Node native launch and data rollback | Bad signature/hash, candidate launch failure, disk-full, interrupted switch, locked Windows file |
| 17 | Complete ledger, final artifacts/platforms, paired evals, performance and migration rehearsal | Revert default and resume with retained 0.3; reject release with missing required evidence |

## Platform requirements

The existing [release target list](../../../packages/boot/updater/src/manifest.ts) is the minimum native artifact matrix. Qualification requires actual executions on the named targets; cross-compilation alone proves compilation.

| Target | Required native evidence |
|---|---|
| Linux x64 and arm64 | Install/start/update/rollback; flock interoperability; process trees; PTY; enforcing sandbox, with bubblewrap and Landlock backend coverage on capable hosts |
| macOS x64 and arm64 | Install/start/update/rollback; flock; Seatbelt effects; PTY/raw modes and signals |
| Windows x64 | Install/start/update/rollback with held files; LockFileEx; restricted-token/ACL effects; job descendants; PowerShell; ConPTY and console restoration |

Scope 00 records minimum OS/kernel/libc/deployment versions from the release artifacts and supported hosts. Do not infer musl distribution support from the presence of a musl-built helper. Match the intended Rust linking strategy to those minimums and test archive execution there.

Run required backend tests on a capable host. An unavailable kernel feature can justify a skip on a general CI worker, but qualification still needs a linked run where enforcement is exercised. Preserve current documented partial-enforcement semantics rather than reporting every backend as equivalent. Add Windows terminal tests instead of using the existing POSIX-only PTY skip as acceptance.

## Evals and model-facing fidelity

Follow the existing [eval policy](../../../evals/README.md). For a model-visible PR, build a clean base and candidate, run the standard task suite against the standard and extended model sets with three trials, interleave arms, and commit the sanitized record under `evals/agent-loop/versions/unreleased/<YYYY-MM-DD>-<topic>/`. Use the model roster owned by that README rather than duplicating a second roster here. Preserve old records; a wrong record is superseded.

The PR-base comparison checks each incremental change. A second comparison against the frozen 0.3 oracle checks cumulative native drift at scopes 09 and 17. The two comparisons answer different questions. Keep the same route, effort, credentials, task fixtures, composition, and provider settings across each pair. Add extended long-session, delegation, background, or attachment cases for their owning scopes; they supplement the standard suite.

The existing evaluator flags token increases with the 95% interval above zero, request increases over 10% with that interval above zero, at least two additional failed samples for a model, or increased tool errors. Fix a flag or record its measured cause and acceptance in the eval note and PR. Deterministic changes to prompts/schemas/results still require review even when noisy live counts show no statistically clear change.

[Model-surface snapshots](../../testing.md#pin-the-model-surface) pin provider-neutral requests. Add per-protocol wire fixtures because a matching neutral request can still encode differently. Credential-dependent smokes remain separate from keyless replay and paired task evaluation; none substitutes for the others.

## Performance and soak

Rust is not itself evidence of lower memory or faster startup. Begin with the current [terminal performance driver](../../../apps/tui/packages/app/performance/README.md), whose six workloads cover fresh, short, typical, long, tool-dense, and large-output history. Its recorded measurements include Node-specific heap statistics and exclude several interactive paths; extend the instrumentation before comparing implementations.

**Proposed qualification method:** use the same machines, terminal geometry, fixtures, external response pacing, and compiler profile. Capture 20 interleaved fresh-process samples per implementation/workload, report p50/p95 and variability, and separate cold/warm filesystem-cache conditions. Compare process-tree peak RSS and idle CPU, startup to first usable input, complete history readiness, input-to-echo under streaming, replay throughput, and shutdown latency. A Rust allocation counter must not be compared with a Node post-GC heap figure as though they measure the same memory.

Scope 00 sets absolute latency/memory budgets from the baseline; freeze them before candidate qualification. Suggested initial non-regression bounds are at most 10% degradation in median/p95 latency and process-tree peak RSS, assessed with noise and confidence intervals rather than single runs. These are proposals to calibrate, not current CI thresholds or promised speedups. Security, correctness, and process/terminal leaks have zero acceptable regression.

Add repeated navigation/resize, compaction, attachment admission, session-query growth, approval waits, update checks, and subagent/background activity. A proposed two-hour mixed-workload soak must retain expected history, finish all owned work, release descriptors and process handles, and distinguish retained session data from unexpected memory growth. Pause performance measurement while unrelated builds or test loads run. Keep raw reports and exact machine/build metadata so another run can reproduce the comparison.

## Reliable test construction

Apply [testing policy](../../testing.md) and [defensive patterns](../../defensive-patterns.md). Every fixture allocates private temporary roots and namespaces; servers bind loopback port zero and report readiness before use. Protect fixture-owned check files by hashing them before and after the agent task.

Use barriers and explicit lifecycle signals for races. Register cleanup immediately; restore environment/cwd/clock exactly; stop new callbacks before canceling; await every child exit, watcher close, and task join. Exercise independent processes concurrently for shared-lock and host-resource behavior. Fixed sleeps and passing repeated runs do not prove a race is controlled.

Use property tests for pure admission/reduction, bounded fuzzing for JSONL/SSE/terminal/protocol parsers, and deterministic fault injection for persistence/publication/process ownership. Choose iteration/time budgets in the owning PR and record them. A test failure is investigated before adding retries, widening timeouts, weakening assertions, or serializing a whole suite.

Scope 01's opt-in [native workspace](../../../rust/README.md) provides locked builds, Rust formatting/lints, unit tests, and preview PTY checks. Shared conformance fixtures and the native eval arm remain open. Exact Cargo/test-wrapper commands belong to the workspace README and its CI. Keep current Bun/Node checks until the consumers they protect are retired through scope 17.

## Planning-session evidence

These runs validate selected TypeScript oracles and the documentation work, not Rust compatibility. Host: Linux x86_64, Bun 1.4.2, Node v26.10.0. Source baseline: `ae5eb51ab61f2266b0fc2ff52f2f14b9f3a9a917`. The repository's CI uses Node 24, so this local run does not replace CI or the release matrix.

| Executed command | Observed result |
|---|---|
| `bun install --frozen-lockfile` | Passed; installed dependencies and normal Git hooks |
| `bun run build` | Passed; host native addon, shared runtime, and production TUI built |
| Focused runtime command below | 7 files, 85 tests passed |
| `bun run test:integration apps/cli/tests/profiles/model-surface.expected.e2e.ts` | 1 file, 2 tests passed; built headless and Desktop first-request snapshots |
| `bun run doc-sync` | 23 entry/terminal Markdown files passed |
| Direct `findViolations`/`anchorCache` check from `scripts/verify-md-links.ts`, run with `bun -e` over the three roadmap files and `docs/development.md` | 4 additional files, zero broken paths or anchors; the normal docs command does not scan this new directory |
| `bun run lint` | Exit 0; zero errors and 136 existing unused-disable warnings in unchanged source/test files |
| `git diff --check` | Passed for tracked edits; new Markdown also checked for whitespace and one final newline |

The focused runtime command was:

```sh
bun run test:runtime \
  packages/core/agent-loop/tests/request-reconstruction.spec.ts \
  packages/core/agent-loop/tests/tool-order.spec.ts \
  packages/core/agent-loop/tests/shutdown-drain.spec.ts \
  packages/session/session-persistence-jsonl/tests/lease.two-process.spec.ts \
  packages/session/session-persistence-jsonl/tests/zstd.compat.spec.ts \
  packages/bundle/desktop/tests/transport.spec.ts \
  packages/llm/llm-pi-ai/tests/sse.spec.ts
```

Logs for this local run are ignored artifacts: `.preflight/rust-roadmap-build.log`, `.preflight/rust-roadmap-runtime.log`, `.preflight/rust-roadmap-model-surface.log`, and `.preflight/rust-roadmap-lint.log`. Both test invocations reported a Vite configuration deprecation notice about `vite-tsconfig-paths`; no test failed or skipped in these selections.

The full runtime suite, full preflight, PTY suite, live provider calls, paired paid-model evals, macOS/Windows tests, final release artifacts, and external Bake Desktop consumer were not run in this planning session. There was no Rust implementation in its source baseline. Scope 00 still owes the comprehensive frozen baseline; these focused checks do not close it.

## Dev Note

Document/link/lint validation is reported with the roadmap change. Future qualification records must name their own exact commits and run results; do not reuse this planning-session table as migration proof.
