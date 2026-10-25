# Rust migration: verification and scope evidence

## Summary

Each [roadmap scope](README.md#linear-development-sequence) closes only when an independent observer can reproduce its behavior, failure handling, and cleanup from identified commits and artifacts. This document defines that evidence and the comparison rules. Future test requirements below are planned work; the [foundation qualification status](#foundation-qualification-status), [planning-session evidence](#planning-session-evidence), and the [qualification ledger](https://github.com/Justar96/bake/blob/97ce6abba01fddb5ed532b60a2e25a1e134b5244/docs/roadmap/rust-0.4/ledger/README.md) distinguish observed checks from missing evidence.

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

The [qualification ledger](https://github.com/Justar96/bake/blob/97ce6abba01fddb5ed532b60a2e25a1e134b5244/docs/roadmap/rust-0.4/ledger/README.md) holds one machine-readable record per attempt, including failed attempts. `bun run verify-rust-migration-ledger` checks their structure and internal consistency offline; it does not accept a scope. Link the relevant records from each scope's PR. Existing eval records retain their own schema and immutable-history policy.

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
| Bake's own additions (writer exclusion, durable writes, the system prompt of [D33](scope-00/support.md#decision-register), and the terminal) | The owning design document | An independent test specification, real OS effects, and the paired eval records the [eval policy](https://github.com/Justar96/bake/blob/97ce6abba01fddb5ed532b60a2e25a1e134b5244/evals/README.md) requires |

No TypeScript runtime runs as a comparison arm for Rust behavior, and no Bake Session log is read, written, or compared ([D32](scope-00/support.md#decision-register)). The synthetic TypeScript/Rust harness, conformance tables, and cross-runtime checks were deleted; their [ledger](https://github.com/Justar96/bake/blob/97ce6abba01fddb5ed532b60a2e25a1e134b5244/docs/roadmap/rust-0.4/ledger/README.md) records stay as history.

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

A model-visible change is measured through the native `bake` eval arm ([eval policy](https://github.com/Justar96/bake/blob/97ce6abba01fddb5ed532b60a2e25a1e134b5244/evals/README.md#native-bake-arm)). Do not declare evals unnecessary because a feature has no live consumer yet; keep it experimental and the scope open until a composed arm exercises it.

## Acceptance matrix

Each row supplements the detailed scope specification. The final column is a required falsification or fault case, not a test reported as already run. The [crash-recovery matrix](durable-execution.md#crash-recovery-matrix) adds kill-and-restart cases to scopes 02–13 as each begins.

| Scope | Required observable evidence | Required failure/control |
|---|---|---|
| 00 | Every shipped behavior and existing test has a disposition (keep, replace with Pi's, or retire by decision under [D22](scope-00/support.md#decision-register)); reproducible baseline artifacts and results | Remove a supported profile/tool from the inventory and make the inventory check fail |
| 01 | Clean locked native builds on Linux, macOS, and Windows and native eval fixture reports | A fake arm that tampers with evaluator-owned tests, claims success without an edit, or exits with failure is rejected |
| 02 | Pi's session entry tree, branching, and context rebuilt from a session file ([D32](scope-00/support.md#decision-register)) | Pi's session-manager tests, ported, including a malformed or truncated entry |
| 03 | Pi's session files under the Bake home, resume, writer exclusion, and durable appends and rewrites | A writer killed mid-append resumes; a second Rust writer is excluded and takes over after the holder exits or is killed; the [crash-recovery matrix](durable-execution.md#crash-recovery-matrix) rows for session writes |
| 04 | Real filesystem confinement and no surviving owned descendants on each OS; Windows AppContainer with explicit read/network grants ([D24](scope-00/support.md#windows-confinement)) | Escape attempt, missing backend, ignored termination, timeout with exit 0, and granted/denied Windows reads and network access |
| 05 | Layer precedence, retained native profiles, scope visibility, credential storage and disposal | Failed registration/reload; excluded custom Cordis profiles and presets, nonempty legacy built-in layers, user `!!js`, and npm profile installs refuse before execution or file changes; secret prompt cancellation and attempted secret leakage |
| 06 | Pi's stream assembly, finish/failure mapping and usage | Split chunk, malformed stream, abrupt EOF, consumer exception, cancellation at every stream phase |
| 07 | The CLIProxyAPI route over its three protocols ([D8](scope-00/support.md#decision-register)), sanitized request fixtures and a live route smoke | Expired credentials, failed refresh, malformed tool arguments, idle timeout, invalid route override |
| 08 | Exact tool schemas/results and independently checked coding effects | Stale edit, denied call, out-of-order completion, exclusive barrier, late progress |
| 09 | Native edit/check/resume, every request reconstructable from the session file, and paired eval | Cancel before admission and during tools; crash after effect but before result commit |
| 10 | Exact composed context, image admission, reconstructable compaction and long-session task | Missing attachment, failed summary, orphaned tool pair, canceled checkpoint, byte-limit edge |
| 11 | Durable child/job/goal/schedule results and shutdown | Parent/child race, duplicate wakeup, clock jump, router failure, restart at settlement |
| 12 | MCP/hooks/web/VM, the native plugin API with a named consumer, its native inspection and management capabilities ([D7](scope-00/support.md#decision-register)), and native extensions through real runtime | Disconnect, invalid schema, private-address redirect, infinite loop, memory limit, denied nested tool, plugin API version mismatch |
| 13 | Built CLI flags/exits and public transport behavior against each retained consumer; no Desktop launch in 0.4.0 ([D9](scope-00/support.md#decision-register)) | Invalid envelope, dropped client, withdrawn approval, busy session |
| 14 | Inline/fullscreen terminal cells, Unicode input, modes and restoration | Resize mid-stream, panic, hangup, malformed escape/input, constrained dimensions |
| 15 | Complete PTY workflow map and all retained preset model surfaces | Cancel navigation/login/question; retain draft; deny approval; external-editor failure |
| 16 | Signed fresh install, update between 0.4 releases, no-Node native launch and rollback; a 0.4 install leaves a 0.3 install and its data working | Bad signature/hash, candidate launch failure, disk-full, interrupted switch, locked Windows file |
| 17 | Complete ledger, final artifacts/platforms, paired evals, performance, and retained-contract evidence | Revert the default; a 0.3 install beside 0.4 still resumes its own sessions; reject release with missing required evidence |

## Foundation qualification status

Scopes 00 to 03 as of candidate `e32476594bace82b57980fe8e1d57e48e3c4c53c` (the merge of PR #215), checked on 2026-10-14. *Required* restates the acceptance row; *implemented* names what the candidate's code does; *observed* links the ledger record of what ran. The macOS runs tested git stash snapshots of the candidate plus uncommitted edits, not the candidate itself. No reviewer has accepted any of these scopes, and passing checks alone do not close one. Later records narrow the scope 01 gap below: at committed candidate `632f76e485`, the [interactive controls record](https://github.com/Justar96/bake/blob/97ce6abba01fddb5ed532b60a2e25a1e134b5244/docs/roadmap/rust-0.4/ledger/scope-15/2026-10-15-interactive-controls.json) passed the native preflight gates on Linux and Intel macOS and the Cargo tests on native Windows, all x64. This table covers scopes 00 to 03 only; evidence for scopes 04 to 15 is in their [ledger records](https://github.com/Justar96/bake/blob/97ce6abba01fddb5ed532b60a2e25a1e134b5244/docs/roadmap/rust-0.4/ledger/README.md) and the [roadmap Dev Note](README.md#dev-note).

| Scope | Required | Implemented at the candidate | Observed | Missing |
|---|---|---|---|---|
| 00 | Every shipped behavior dispositioned; reproducible baseline; owner decisions answered | Inventory and its check (`bun run verify-rust-migration-inventory`), the 2026-10-07 TypeScript baseline | [Scope 00 README](scope-00/README.md#what-is-implemented-and-what-is-missing); no new record | Owner decisions D10 removal timing and D13's maintenance window; frozen performance budgets; minimum host versions. D32 settles D11 and D14's transition, D34 supersedes D12, and D7 to D9 were decided on 2026-10-17. Scope 16 still qualifies native packaging and publication channels |
| 01 | Clean locked native builds, lints, and tests on three OSes; eval fixture arm with tamper controls | `rust/` workspace on Rust 1.99.0, `bun run check:rust`, the native fixture adapter, and the keyless `bake` arm | [Native checks at e32476594b](https://github.com/Justar96/bake/blob/97ce6abba01fddb5ed532b60a2e25a1e134b5244/docs/roadmap/rust-0.4/ledger/scope-01/2026-10-14-foundation-native-checks.json): local native preflight failed twice on a port-reuse race in a CLIProxyAPI test, and a stepwise rerun passed; CI passed on Linux and Windows. macOS on Intel x86_64: [first run](https://github.com/Justar96/bake/blob/97ce6abba01fddb5ed532b60a2e25a1e134b5244/docs/roadmap/rust-0.4/ledger/scope-01/2026-10-14-macos-native-r1.json) failed on the first fix, and the [second run](https://github.com/Justar96/bake/blob/97ce6abba01fddb5ed532b60a2e25a1e134b5244/docs/roadmap/rust-0.4/ledger/scope-01/2026-10-14-macos-native-r2.json) passed all three native gates with the final refused-connection test | A committed candidate that carries the fix, with a clean native gate on Linux, macOS, and Windows; arm64 hosts; Windows PTY; two macOS PTY cases that need Linux `/proc` |
| 02 | Pi's entry tree, branching, context rebuild; Pi's tests; malformed and truncated entries | Pi session model; at `32394c11f5`, provider-visible unanswered calls derived from projected context | [Pi session model](https://github.com/Justar96/bake/blob/97ce6abba01fddb5ed532b60a2e25a1e134b5244/docs/roadmap/rust-0.4/ledger/scope-02/2026-10-14-pi-session-model.json); [unanswered calls](https://github.com/Justar96/bake/blob/97ce6abba01fddb5ed532b60a2e25a1e134b5244/docs/roadmap/rust-0.4/ledger/scope-02/2026-10-17-unanswered-tool-calls.json): 188 session tests on Linux and Intel macOS, every-byte and line-boundary cases, two rejected controls; [native-host follow-up](https://github.com/Justar96/bake/blob/97ce6abba01fddb5ed532b60a2e25a1e134b5244/docs/roadmap/rust-0.4/ledger/scope-02/2026-10-17-unanswered-native-hosts.json): the unchanged 13 projection cases pass on Linux, Intel macOS, and Windows at candidate `1e94aba7e2` | Raw-path unfinished work, open children and pending inbox; arm64 hosts and remaining Pi session integration cases |
| 03 | Pi's session files under the Bake home; resume; writer exclusion on three OSes; a writer killed mid-append resumes | Pi's file operations and listing, a `.lock` sidecar with `File::try_lock`, synced appends that start on a new line, and synced rename rewrites | [Pi session storage](https://github.com/Justar96/bake/blob/97ce6abba01fddb5ed532b60a2e25a1e134b5244/docs/roadmap/rust-0.4/ledger/scope-03/2026-10-14-pi-session-storage.json); [session group append](https://github.com/Justar96/bake/blob/97ce6abba01fddb5ed532b60a2e25a1e134b5244/docs/roadmap/rust-0.4/ledger/scope-03/2026-10-17-session-group-append.json) (partial) | On Linux, a writer is killed inside the `write` of an append, a group line, and a migration's temporary file, and at each step of a rewrite (`session::manager::{kernel_write_tests, rewrite_tests}`; [writer kills](https://github.com/Justar96/bake/blob/97ce6abba01fddb5ed532b60a2e25a1e134b5244/docs/roadmap/rust-0.4/ledger/scope-03/2026-10-19-session-writer-kills.json), partial); the rewrite kill tests also pass on Intel macOS. On Linux, a seccomp filter fails appends, group lines, and a migration's temporary file with `EIO` (`seccomp_write_tests`, the writer's handling of the returned error, not a failing device), a size-capped tmpfs in the test's own user namespace fills up with `ENOSPC` (`full_filesystem_tests`), and power loss is simulated by replaying the writer's logged operations at byte granularity, with zero-filled tails and unsynced directory entries (`power_loss_tests`); the kernel cases skip with a note where unavailable and are required under `BAKE_REQUIRE_KERNEL_FAULTS=1`. Missing for 0.4.0: a physical power cut, which the simulation does not prove, and the crash-recovery matrix's child-agent row. The in-place rewrite, which a kill or power loss can leave empty or truncated, keeps Pi's behavior for 0.4.0 ([D49](scope-00/support.md#decision-register)), pinned by `a_hard_linked_migration_killed_in_place_can_leave_a_truncated_file`. 0.4.x ([D47](scope-00/support.md#decision-register)): a kill inside `write` on macOS and Windows, and the rewrite kill tests on Windows (compile-checked only). Group append ([rule 4](durable-execution.md#4-commit-related-entries-as-a-recoverable-group)) has passed on Linux and Intel macOS at committed candidate `41e5e867a3` and in native Windows CI on a synthetic merge with the same tree. At `e32476594b`, the macOS session tests ran only on an [uncommitted snapshot](https://github.com/Justar96/bake/blob/97ce6abba01fddb5ed532b60a2e25a1e134b5244/docs/roadmap/rust-0.4/ledger/scope-01/2026-10-14-macos-native-r2.json) |

The scope 02 and 03 records before 2026-10-10 qualify the deleted Bake-format code ([D32](scope-00/support.md#decision-register)) and say nothing about the Pi port.

## Platform requirements

The existing [release target list](https://github.com/Justar96/bake/blob/97ce6abba01fddb5ed532b60a2e25a1e134b5244/packages/boot/updater/src/manifest.ts) is the minimum native artifact matrix. Qualification requires actual executions on the named targets; cross-compilation alone proves compilation.

| Target | Required native evidence |
|---|---|
| Linux x64 and arm64 | Install/start/update/rollback; session writer exclusion between processes; process trees; PTY; enforcing sandbox, with bubblewrap and Landlock backend coverage on capable hosts |
| macOS x64 and arm64 | Install/start/update/rollback; session writer exclusion between processes; Seatbelt effects; PTY/raw modes and signals |
| Windows x64 | Install/start/update/rollback with held files; session writer exclusion between processes, with session files left readable; restricted-token/ACL effects; job descendants; PowerShell; ConPTY and console restoration |

Scope 00 records minimum OS/kernel/libc/deployment versions from the release artifacts and supported hosts. Do not infer musl distribution support from the presence of a musl-built helper. Match the intended Rust linking strategy to those minimums and test archive execution there.

Run required backend tests on a capable host. An unavailable kernel feature can justify a skip on a general CI worker, but qualification still needs a linked run where enforcement is exercised. Preserve current documented partial-enforcement semantics rather than reporting every backend as equivalent. Add Windows terminal tests instead of using the existing POSIX-only PTY skip as acceptance.

## Evals and model-facing fidelity

Follow the existing [eval policy](https://github.com/Justar96/bake/blob/97ce6abba01fddb5ed532b60a2e25a1e134b5244/evals/README.md). For a model-visible PR, build a clean base and candidate, run the standard task suite against the standard and extended model sets with three trials, interleave arms, and commit the sanitized record under `evals/agent-loop/versions/unreleased/<YYYY-MM-DD>-<topic>/`. Use the model roster owned by that README rather than duplicating a second roster here. Preserve old records; a wrong record is superseded.

The PR-base comparison checks each incremental change. A comparison against the frozen 0.3 agent may inform scopes 09 and 17, as the [native arm record](https://github.com/Justar96/bake/blob/97ce6abba01fddb5ed532b60a2e25a1e134b5244/evals/agent-loop/versions/v0.4.0/2026-10-13-native-arm/REPORT.md) does, but under [D32](scope-00/support.md#decision-register) it sets no parity bar. Keep the same route, effort, credentials, task fixtures, composition, and provider settings across each pair. Add extended long-session, delegation, background, or attachment cases for their owning scopes; they supplement the standard suite.

The existing evaluator flags token increases with the 95% interval above zero, request increases over 10% with that interval above zero, at least two additional failed samples for a model, or increased tool errors. Fix a flag or record its measured cause and acceptance in the eval note and PR. Deterministic changes to prompts/schemas/results still require review even when noisy live counts show no statistically clear change.

The Rust first-request fixtures (`rust/crates/bake-cli/tests/fixtures/first-request/`) pin the full first request of each preset; the TypeScript [model-surface snapshots](../../testing.md#pin-the-model-surface) pin only the frozen 0.3 agent. Add per-protocol wire fixtures because a matching neutral request can still encode differently. Credential-dependent smokes remain separate from keyless replay and paired task evaluation; none substitutes for the others.

## Performance and soak

Rust is not itself evidence of lower memory or faster startup. Measure with the [D18 performance harness](https://github.com/Justar96/bake/blob/97ce6abba01fddb5ed532b60a2e25a1e134b5244/scripts/rust-perf/README.md) (`scripts/rust-perf.ts`). It runs a built v0.3.8 checkout and a native release `bake` as fresh processes in the same 120×40 PTY, by one external method, on the six workloads of the 0.3 [terminal performance driver](https://github.com/Justar96/bake/blob/97ce6abba01fddb5ed532b60a2e25a1e134b5244/apps/tui/packages/app/performance/README.md): `fresh` (no history), `small` (50 turns), `typical` (500), `tail` (2000), `tools` (500 turns with four tool calls each), and `large-output` (12 turns of 64 KiB answers). It observes both arms only through their terminal output and `/proc`, so both are measured at the same endpoints with no in-process instrumentation; the driver's Node-specific heap statistics are not a comparison metric, and a Rust allocation counter is never compared with a Node post-GC heap figure.

Each workload is seeded as the 0.3 Session log that 0.3's fixture writer persisted, byte for byte, and as the equivalent Pi session. One loopback OpenAI Responses model streams 100 deltas 10 ms apart to both arms. The native arm runs `bake --inline`, which writes resumed history to the terminal as 0.3 does; the 0.3 arm runs `node apps/cli/lib/bin.js --profile tui` in its checkout. Every sample is a fresh process with a freshly seeded session and a new home and workspace. It records startup to the composer echo of a typed probe, full-history readiness (that echo and every history marker), replay to the last marker, input-to-echo while an answer streams, idle CPU over 5 s, shutdown from SIGTERM to exit, and peak memory as the sum over every process the tree held of its largest `VmHWM`, read every 10 ms, which bounds the simultaneous peak from above; the sampled `VmRSS` tree peak is reported without a budget. Times are taken when the PTY output chunk arrives, not at paint. The observer reads about 160-200 MB/s, near Bun's PTY floor, so output-heavy workloads include the read cost, and each record gives the bytes it read. A sample fails when the program is gone before SIGTERM, has not exited 10 s after it (its tree is then killed), exits with a status other than 0 or 143, leaves the termios flags or a tracked DEC private mode (auto-wrap, cursor visibility, mouse reporting, focus events, the alternate screen, or bracketed paste) changed, or shows a history marker other than exactly once; a failed sample is recorded and never summarized. Processes of the program still alive 250 ms after its exit are recorded as leaked and killed.

Twenty samples per implementation, workload, and cache mode run interleaved, with the arm order reversed on odd iterations. A cold sample starts from an empty `BAKE_HOME` and an empty private `TMPDIR`, where Node keeps its compile cache. A warm sample starts from a copy, at the same path, of a home that one unmeasured `fresh` launch prepared, with its own seeded session and the `TMPDIR` its arm's warm samples share, after a discarded warm-up launch. Neither mode drops the kernel page cache, which needs privileges. The harness refuses a native binary that is not a release build unless `--allow-non-release` is given, and such a report sets no budget. It refuses to start above a load-average threshold. Its report, under `.preflight/perf/`, holds the host's and builds' identity, every sample, and per cell the median, nearest-rank p95, MAD, and 95% bootstrap intervals.

Scope 00 freezes absolute latency and memory budgets from a v0.3.8 baseline before candidate qualification. `bun scripts/rust-perf.ts budgets` turns a baseline report into a proposal: for each workload and cache mode, each latency's and peak memory's median and p95 may degrade by at most 10%, and a candidate may leak no process and fail no sample; idle CPU and the sampled RSS peak are reported without a bound. A result near a bound is assessed against both runs' intervals, not a single run. The proposal is a file the owner approves, and approved values are then recorded in [D18](scope-00/support.md#decision-register) by hand; no budget is set yet, and none is a CI threshold or a promised speedup. Security, correctness, and process/terminal leaks have zero acceptable regression.

Run the baseline on a quiet Linux host before any candidate measurement. Check out and build v0.3.8 (`git worktree add --detach ../bake-v0.3.8 v0.3.8`, then `bun install --frozen-lockfile`, `bun run build`, and `bun run build:native-system` in that worktree), measure it with `bun scripts/rust-perf.ts --arm ts:../bake-v0.3.8 --max-load 1` (all six workloads, 20 samples, cold and warm), and propose budgets with `bun scripts/rust-perf.ts budgets --baseline <report> --out <proposal>`. The owner reviews the report, the proposal, and its warnings and approves the budgets; only then are they frozen in D18 and the candidate (`--arm native:rust/target/release/bake`, a release build, on the same host and method) measured against them.

Add repeated navigation/resize, compaction, attachment admission, session-query growth, approval waits, update checks, and subagent/background activity. A proposed two-hour mixed-workload soak must retain expected history, finish all owned work, release descriptors and process handles, and distinguish retained session data from unexpected memory growth. Pause performance measurement while unrelated builds or test loads run. Keep raw reports and exact machine/build metadata so another run can reproduce the comparison.

## Reliable test construction

Apply [testing policy](../../testing.md) and [defensive patterns](https://github.com/Justar96/bake/blob/97ce6abba01fddb5ed532b60a2e25a1e134b5244/docs/defensive-patterns.md). Every fixture allocates private temporary roots and namespaces; servers bind loopback port zero and report readiness before use. Protect fixture-owned check files by hashing them before and after the agent task.

Use barriers and explicit lifecycle signals for races. Register cleanup immediately; restore environment/cwd/clock exactly; stop new callbacks before canceling; await every child exit, watcher close, and task join. Exercise independent processes concurrently for shared-lock and host-resource behavior. Fixed sleeps and passing repeated runs do not prove a race is controlled.

Use property tests for pure admission/reduction, bounded fuzzing for JSONL/SSE/terminal/protocol parsers, and deterministic fault injection for persistence/publication/process ownership. Choose iteration/time budgets in the owning PR and record them. A test failure is investigated before adding retries, widening timeouts, weakening assertions, or serializing a whole suite.

Scope 01's opt-in [native workspace](../../development.md) provides locked builds, Rust formatting/lints, unit tests, and preview PTY checks. The synthetic TypeScript/Rust comparison harness that scope 01 first built was deleted under [D32](scope-00/support.md#decision-register); the [ledger](https://github.com/Justar96/bake/blob/97ce6abba01fddb5ed532b60a2e25a1e134b5244/docs/roadmap/rust-0.4/ledger/README.md) keeps its records. [Keyless eval-launch checks](https://github.com/Justar96/bake/blob/97ce6abba01fddb5ed532b60a2e25a1e134b5244/evals/README.md#launcher-contract-checks) pin the Bake and pi launch configuration. The [native eval fixture adapter](https://github.com/Justar96/bake/blob/97ce6abba01fddb5ed532b60a2e25a1e134b5244/evals/README.md#native-fixture-adapter) accepts a compiled fake arm's correct edit and rejects tampered tests, false success claims, and unsuccessful processes. The [native `bake` arm](https://github.com/Justar96/bake/blob/97ce6abba01fddb5ed532b60a2e25a1e134b5244/evals/README.md#native-bake-arm) runs `bake -p` against a keyless fake server in `bun run test:rust:eval` and against live models in paired records. The [qualification ledger](https://github.com/Justar96/bake/blob/97ce6abba01fddb5ed532b60a2e25a1e134b5244/docs/roadmap/rust-0.4/ledger/README.md) records partial evidence and failed attempts. Exact Cargo/test-wrapper commands belong to the workspace README and its CI. Keep current Bun/Node checks until the consumers they protect are retired through scope 17.

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
