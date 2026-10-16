# Rust migration: verification and scope evidence

## Summary

Each [roadmap scope](README.md#linear-development-sequence) closes only when an independent observer can reproduce its behavior, failure handling, and cleanup from identified commits and artifacts. This document defines that evidence and the comparison rules. Future test requirements below are planned work; the [foundation qualification status](#foundation-qualification-status), [planning-session evidence](#planning-session-evidence), and the [qualification ledger](ledger/README.md) distinguish observed checks from missing evidence.

**Status: reconciled with [D22](scope-00/support.md#decision-register), [D24](scope-00/support.md#decision-register), [D25](scope-00/support.md#decision-register), [D32](scope-00/support.md#decision-register), [D33](scope-00/support.md#decision-register), and [D34](scope-00/support.md#decision-register) on 2026-10-14.** The reference for ported behavior is Pi's official source at a recorded revision, and for the three retained Bake contracts it is their TypeScript source. 0.4 has no TypeScript runtime arm, Session-log compatibility, or 0.3 parity bar. Where a row below for scopes 04 to 17 still names 0.3 behavior, the [Pi-first scope plan](pi-first-plan.md#scope-plan) governs until that scope's first PR rewrites it.

## Table of Contents

- [Evidence required for every scope](#evidence-required-for-every-scope)
- [Reference sources and comparison](#reference-sources-and-comparison)
- [Acceptance matrix](#acceptance-matrix)
- [Foundation qualification status](#foundation-qualification-status)
- [Platform requirements](#platform-requirements)
- [Evals and model-facing fidelity](#evals-and-model-facing-fidelity)
- [Performance and soak](#performance-and-soak)
- [Reliable test construction](#reliable-test-construction)
- [Planning-session evidence](#planning-session-evidence)
- [Dev Note](#dev-note)

## Evidence required for every scope

The [qualification ledger](ledger/README.md) holds one machine-readable record per attempt, including failed attempts. `bun run verify-rust-migration-ledger` checks their structure and internal consistency offline; it does not accept a scope. Link the relevant records from each scope's PR. Existing eval records retain their own schema and immutable-history policy.

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

## Reference sources and comparison

Expected results come from a source independent of the Rust code under test. The same Rust implementation must not generate both its input and its expected result.

| Behavior | Reference | How it reaches a Rust test |
|---|---|---|
| Ported from Pi ([D22](scope-00/support.md#decision-register)) | Pi's official release at the revision the PR records | Pi's tests, ported with the code, and golden files that Pi's own code writes or reads (for example `rust/crates/bake-coding-agent/tests/fixtures/pi-session/generate.mjs`) |
| Retained Bake contracts ([D32](scope-00/support.md#decision-register): the CLIProxyAPI route, the sandbox, approvals, and permission presets of [D24](scope-00/support.md#decision-register), and the `~/.bake` home read as [D25](scope-00/support.md#decision-register) describes) | The cited TypeScript source and its tests | Rust fixture tests derived from that source; the TypeScript arm stays in TypeScript tests and never changes product source |
| Bake's own additions (writer exclusion, durable writes, the system prompt of [D33](scope-00/support.md#decision-register), and the terminal) | The owning design document | An independent test specification, real OS effects, and the paired eval records the [eval policy](../../../evals/README.md) requires |

No TypeScript runtime runs as a comparison arm for Rust behavior, and no Bake Session log is read, written, or compared ([D32](scope-00/support.md#decision-register)). The synthetic TypeScript/Rust harness, conformance tables, and cross-runtime checks were deleted; their [ledger](ledger/README.md) records stay as history.

A test compares more than a final assistant reply:

| Observation | Comparison rule |
|---|---|
| System prompts, context strings, tool names/descriptions/results/errors | Exact bytes after narrowly defined fixture substitutions |
| Model request structure | Exact values and ordered arrays, omitted/null distinction, tool order, request count; compare provider-neutral and encoded provider forms separately |
| Provider wire body | Preserve prompt/schema order where cache or protocol behavior depends on it; parse JSON only where object-key order is demonstrably irrelevant |
| Session entries | Exact entry type, data, order, and `id`/`parentId` links; generated ids normalized by one stable bijection across the entire run |
| Session file bytes | Lines a writer leaves match the bytes Pi writes for the same entries; a file Pi wrote reads as Pi reads it |
| Files and commands | File bytes, permissions where applicable, protected-fixture hashes, real check exit, timeout and signal recorded separately |
| Terminal | Emulator cells, cursor position, terminal modes, scrollback markers, input submission and exits; identical ANSI instruction sequences are unnecessary |
| Cleanup | All owned processes/tasks/connections terminate, locks release, no callback mutates state after close |

Normalize generated UUIDs, fixture-root paths, captured clock values, and allocated loopback ports only through an allowlist. Keep identity relationships, event ordering, timeout decisions, and model-visible text intact. Do not sort arrays, drop unknown fields, erase errors, normalize all whitespace, or ignore “extra” model calls to make a comparison pass. Compare durations as measurements, not by rewriting them to fixed values.

Expected filesystem outcomes and scenario predicates come from an independent test specification. Keep a ported Pi test beside any golden file derived from it until the golden file covers the same behavior.

A model-visible change is measured through the native `bake-rs` eval arm ([eval policy](../../../evals/README.md#native-bake-rs-arm)). Do not declare evals unnecessary because a feature has no live consumer yet; keep it experimental and the scope open until a composed arm exercises it.

## Acceptance matrix

Each row supplements the detailed scope specification. The final column is a required falsification or fault case, not a test reported as already run. The [crash-recovery matrix](durable-execution.md#crash-recovery-matrix) adds kill-and-restart cases to scopes 02–13 as each begins.

| Scope | Required observable evidence | Required failure/control |
|---|---|---|
| 00 | Every shipped behavior and existing test has a disposition (keep, replace with Pi's, or retire by decision under [D22](scope-00/support.md#decision-register)); reproducible baseline artifacts and results | Remove a supported profile/tool from the inventory and make the inventory check fail |
| 01 | Clean locked native builds on Linux, macOS, and Windows and native eval fixture reports | A fake arm that tampers with evaluator-owned tests, claims success without an edit, or exits with failure is rejected |
| 02 | Pi's session entry tree, branching, and context rebuilt from a session file ([D32](scope-00/support.md#decision-register)) | Pi's session-manager tests, ported, including a malformed or truncated entry |
| 03 | Pi's session files under the Bake home, resume, writer exclusion, and durable appends and rewrites | A writer killed mid-append resumes; a second Rust writer is excluded and takes over after the holder exits or is killed; the [crash-recovery matrix](durable-execution.md#crash-recovery-matrix) rows for session writes |
| 04 | Real filesystem confinement and no surviving owned descendants on each OS | Escape attempt, missing backend, ignored termination, and timeout with exit 0 |
| 05 | Layer precedence, retained native profiles, scope visibility, credential storage and disposal | Failed registration/reload; excluded custom Cordis profiles and presets, nonempty legacy built-in layers, user `!!js`, and npm profile installs refuse before execution or file changes; secret prompt cancellation and attempted secret leakage |
| 06 | Pi's stream assembly, finish/failure mapping and usage | Split chunk, malformed stream, abrupt EOF, consumer exception, cancellation at every stream phase |
| 07 | Approved provider/auth matrix, sanitized request fixtures and live route smokes | Expired credentials, failed refresh, malformed tool arguments, idle timeout, invalid route override |
| 08 | Exact tool schemas/results and independently checked coding effects | Stale edit, denied call, out-of-order completion, exclusive barrier, late progress |
| 09 | Native edit/check/resume, every request reconstructable from the session file, and paired eval | Cancel before admission and during tools; crash after effect but before result commit |
| 10 | Exact composed context, image admission, reconstructable compaction and long-session task | Missing attachment, failed summary, orphaned tool pair, canceled checkpoint, byte-limit edge |
| 11 | Durable child/job/goal/schedule results and shutdown | Parent/child race, duplicate wakeup, clock jump, router failure, restart at settlement |
| 12 | MCP/hooks/web/VM, the native plugin API with a named consumer, and native extensions through real runtime | Disconnect, invalid schema, private-address redirect, infinite loop, memory limit, denied nested tool, plugin API version mismatch |
| 13 | Built CLI flags/exits, public transport behavior and actual Desktop launch | Invalid envelope, dropped client, withdrawn approval, busy session, incompatible Electron entry |
| 14 | Inline/fullscreen terminal cells, Unicode input, modes and restoration | Resize mid-stream, panic, hangup, malformed escape/input, constrained dimensions |
| 15 | Complete PTY workflow map and all retained preset model surfaces | Cancel navigation/login/question; retain draft; deny approval; external-editor failure |
| 16 | Signed fresh install, update between 0.4 releases, no-Node native launch and rollback; a 0.4 install leaves a 0.3 install and its data working | Bad signature/hash, candidate launch failure, disk-full, interrupted switch, locked Windows file |
| 17 | Complete ledger, final artifacts/platforms, paired evals, performance, and retained-contract evidence | Revert the default; a 0.3 install beside 0.4 still resumes its own sessions; reject release with missing required evidence |

## Foundation qualification status

Scopes 00 to 03 as of candidate `e32476594bace82b57980fe8e1d57e48e3c4c53c` (the merge of PR #215), checked on 2026-10-14. *Required* restates the acceptance row; *implemented* names what the candidate's code does; *observed* links the ledger record of what ran. The macOS runs tested git stash snapshots of the candidate plus uncommitted edits, not the candidate itself. No reviewer has accepted any of these scopes, and passing checks alone do not close one.

| Scope | Required | Implemented at the candidate | Observed | Missing |
|---|---|---|---|---|
| 00 | Every shipped behavior dispositioned; reproducible baseline; owner decisions answered | Inventory and its check (`bun run verify-rust-migration-inventory`), the 2026-10-07 TypeScript baseline | [Scope 00 README](scope-00/README.md#what-is-implemented-and-what-is-missing); no new record | Owner decisions D7, D8 beyond CLIProxyAPI, D9, D10 timing, D11, D12, D13's window, and D14; frozen performance budgets; minimum host versions |
| 01 | Clean locked native builds, lints, and tests on three OSes; eval fixture arm with tamper controls | `rust/` workspace on Rust 1.99.0, `bun run check:rust`, the native fixture adapter, and the keyless `bake-rs` arm | [Native checks at e32476594b](ledger/scope-01/2026-10-14-foundation-native-checks.json): local native preflight failed twice on a port-reuse race in a CLIProxyAPI test, and a stepwise rerun passed; CI passed on Linux and Windows. macOS on Intel x86_64: [first run](ledger/scope-01/2026-10-14-macos-native-r1.json) failed on the first fix, and the [second run](ledger/scope-01/2026-10-14-macos-native-r2.json) passed all three native gates with the final refused-connection test | A committed candidate that carries the fix, with a clean native gate on Linux, macOS, and Windows; arm64 hosts; Windows PTY; two macOS PTY cases that need Linux `/proc` |
| 02 | Pi's entry tree, branching, context rebuild; Pi's tests; malformed and truncated entries | `bake-coding-agent::session` ported from Pi v1.1.0, whose session sources and tests are unchanged in v1.2.0 | [Pi session model](ledger/scope-02/2026-10-14-pi-session-model.json) | The durable-execution unfinished-work projection; property cases over truncated files; macOS on a committed candidate |
| 03 | Pi's session files under the Bake home; resume; writer exclusion on three OSes; a writer killed mid-append resumes | Pi's file operations and listing, a `.lock` sidecar with `File::try_lock`, synced appends that start on a new line, and synced rename rewrites | [Pi session storage](ledger/scope-03/2026-10-14-pi-session-storage.json) | A process killed during an append (tests write the torn bytes instead); the crash-recovery matrix; group append. On macOS the session tests ran only on an [uncommitted snapshot](ledger/scope-01/2026-10-14-macos-native-r2.json) |

The scope 02 and 03 records before 2026-10-10 qualify the deleted Bake-format code ([D32](scope-00/support.md#decision-register)) and say nothing about the Pi port.

## Platform requirements

The existing [release target list](../../../packages/boot/updater/src/manifest.ts) is the minimum native artifact matrix. Qualification requires actual executions on the named targets; cross-compilation alone proves compilation.

| Target | Required native evidence |
|---|---|
| Linux x64 and arm64 | Install/start/update/rollback; session writer exclusion between processes; process trees; PTY; enforcing sandbox, with bubblewrap and Landlock backend coverage on capable hosts |
| macOS x64 and arm64 | Install/start/update/rollback; session writer exclusion between processes; Seatbelt effects; PTY/raw modes and signals |
| Windows x64 | Install/start/update/rollback with held files; session writer exclusion between processes, with session files left readable; restricted-token/ACL effects; job descendants; PowerShell; ConPTY and console restoration |

Scope 00 records minimum OS/kernel/libc/deployment versions from the release artifacts and supported hosts. Do not infer musl distribution support from the presence of a musl-built helper. Match the intended Rust linking strategy to those minimums and test archive execution there.

Run required backend tests on a capable host. An unavailable kernel feature can justify a skip on a general CI worker, but qualification still needs a linked run where enforcement is exercised. Preserve current documented partial-enforcement semantics rather than reporting every backend as equivalent. Add Windows terminal tests instead of using the existing POSIX-only PTY skip as acceptance.

## Evals and model-facing fidelity

Follow the existing [eval policy](../../../evals/README.md). For a model-visible PR, build a clean base and candidate, run the standard task suite against the standard and extended model sets with three trials, interleave arms, and commit the sanitized record under `evals/agent-loop/versions/unreleased/<YYYY-MM-DD>-<topic>/`. Use the model roster owned by that README rather than duplicating a second roster here. Preserve old records; a wrong record is superseded.

The PR-base comparison checks each incremental change. A comparison against the frozen 0.3 agent may inform scopes 09 and 17, as the [native arm record](../../../evals/agent-loop/versions/unreleased/2026-10-13-native-arm/REPORT.md) does, but under [D32](scope-00/support.md#decision-register) it sets no parity bar. Keep the same route, effort, credentials, task fixtures, composition, and provider settings across each pair. Add extended long-session, delegation, background, or attachment cases for their owning scopes; they supplement the standard suite.

The existing evaluator flags token increases with the 95% interval above zero, request increases over 10% with that interval above zero, at least two additional failed samples for a model, or increased tool errors. Fix a flag or record its measured cause and acceptance in the eval note and PR. Deterministic changes to prompts/schemas/results still require review even when noisy live counts show no statistically clear change.

The Rust first-request fixtures (`rust/crates/bake-cli/tests/fixtures/first-request/`) pin the full first request of each preset; the TypeScript [model-surface snapshots](../../testing.md#pin-the-model-surface) pin only the frozen 0.3 agent. Add per-protocol wire fixtures because a matching neutral request can still encode differently. Credential-dependent smokes remain separate from keyless replay and paired task evaluation; none substitutes for the others.

## Performance and soak

Rust is not itself evidence of lower memory or faster startup. Begin with the current [terminal performance driver](../../../apps/tui/packages/app/performance/README.md), whose six workloads cover fresh, short, typical, long, tool-dense, and large-output history. Its recorded measurements include Node-specific heap statistics and exclude several interactive paths; extend the instrumentation before comparing implementations.

**Proposed qualification method:** use the same machines, terminal geometry, fixtures, external response pacing, and compiler profile. Capture 20 interleaved fresh-process samples per implementation/workload, report p50/p95 and variability, and separate cold/warm filesystem-cache conditions. Compare process-tree peak RSS and idle CPU, startup to first usable input, complete history readiness, input-to-echo under streaming, replay throughput, and shutdown latency. A Rust allocation counter must not be compared with a Node post-GC heap figure as though they measure the same memory.

Scope 00 sets absolute latency/memory budgets from the baseline; freeze them before candidate qualification. Suggested initial non-regression bounds are at most 10% degradation in median/p95 latency and process-tree peak RSS, assessed with noise and confidence intervals rather than single runs. These are proposals to calibrate, not current CI thresholds or promised speedups. Security, correctness, and process/terminal leaks have zero acceptable regression.

Add repeated navigation/resize, compaction, attachment admission, session-query growth, approval waits, update checks, and subagent/background activity. A proposed two-hour mixed-workload soak must retain expected history, finish all owned work, release descriptors and process handles, and distinguish retained session data from unexpected memory growth. Pause performance measurement while unrelated builds or test loads run. Keep raw reports and exact machine/build metadata so another run can reproduce the comparison.

## Reliable test construction

Apply [testing policy](../../testing.md) and [defensive patterns](../../defensive-patterns.md). Every fixture allocates private temporary roots and namespaces; servers bind loopback port zero and report readiness before use. Protect fixture-owned check files by hashing them before and after the agent task.

Use barriers and explicit lifecycle signals for races. Register cleanup immediately; restore environment/cwd/clock exactly; stop new callbacks before canceling; await every child exit, watcher close, and task join. Exercise independent processes concurrently for shared-lock and host-resource behavior. Fixed sleeps and passing repeated runs do not prove a race is controlled.

Use property tests for pure admission/reduction, bounded fuzzing for JSONL/SSE/terminal/protocol parsers, and deterministic fault injection for persistence/publication/process ownership. Choose iteration/time budgets in the owning PR and record them. A test failure is investigated before adding retries, widening timeouts, weakening assertions, or serializing a whole suite.

Scope 01's opt-in [native workspace](../../../rust/README.md) provides locked builds, Rust formatting/lints, unit tests, and preview PTY checks. The synthetic TypeScript/Rust comparison harness that scope 01 first built was deleted under [D32](scope-00/support.md#decision-register); the [ledger](ledger/README.md) keeps its records. [Keyless eval-launch checks](../../../evals/README.md#launcher-contract-checks) pin the Bake and pi launch configuration. The [native eval fixture adapter](../../../evals/README.md#native-fixture-adapter) accepts a compiled fake arm's correct edit and rejects tampered tests, false success claims, and unsuccessful processes. The [native `bake-rs` arm](../../../evals/README.md#native-bake-rs-arm) runs `bake-rs -p` against a keyless fake server in `bun run test:rust:eval` and against live models in paired records. The [qualification ledger](ledger/README.md) records partial evidence and failed attempts. Exact Cargo/test-wrapper commands belong to the workspace README and its CI. Keep current Bun/Node checks until the consumers they protect are retired through scope 17.

## Planning-session evidence

These 2026-10-07 runs validate selected TypeScript oracles and the documentation work, not Rust behavior; since [D32](scope-00/support.md#decision-register) those oracles are no longer comparison arms. Host: Linux x86_64, Bun 1.4.2, Node v26.10.0. Source baseline: `ae5eb51ab61f2266b0fc2ff52f2f14b9f3a9a917`. The repository's CI uses Node 24, so this local run does not replace CI or the release matrix.

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
