# Rust 0.4 continuation checkpoint

## Summary

The Rust port is paused at the owner's request on 2026-10-17. The `rust/0.4-continuation` draft combines the remaining plugin, Windows qualification, and background job work on `develop` at `0ab01cf2f83be6ba7e9ba525a050f385658bae3a`. It preserves unfinished code so work can resume from one PR. It is not qualified for merge or release. The [Pi-first plan](pi-first-plan.md), [support register](scope-00/support.md), and [qualification ledger](ledger/README.md) remain authoritative; no roadmap scope is closed by this checkpoint.

## Table of Contents

- [Saved work](#saved-work)
- [Known failures and qualification gaps](#known-failures-and-qualification-gaps)
- [Resume order](#resume-order)
- [Dev Note](#dev-note)

## Saved work

The draft retains the source commits below, including the workers' incomplete edits. Previously merged work is already in its `develop` base: shared process ownership (#242), sandbox Job ownership (#245), native license packaging (#244), isolated evaluator configuration (#246), and the MCP stdio CLI and Session consumer (#247).

| Area | Saved source | Current state |
|---|---|---|
| Wasm plugin host | `be2065f9107b8a57361b8af4fa354ffaf4057e69` | Wasmtime 49.0.3 component host, WIT API, SDK, helper process ownership, resource limits, diagnostic command, and license packaging |
| Installed plugins and Session veto | `7c51e8245d26d3f2273c9231ad5a46afea7714a8` | Native install/check/enable/disable/list commands, immutable component storage, lazy Session tool veto, and trusted updates on the consumed file under the store lock; integrated with the MCP base, but their combined behavior is not qualified |
| AppContainer runner | `0697c33ee2b10acc10e8208bcff068ed36546eb8` | Explicit runner with private per-run profiles, read/network grants, and shared Job teardown; default confinement remains disabled (`Enforcement::Partial`) |
| ConPTY qualification | `20c5c14fb48ece989c70d544a6e0071a6427d5ca` | Real Windows console helper, independent pipe threads, startup failure controls, and Rust PTY runner integration; native qualification remains open |
| Background jobs | `5bd938da708e6bc03b725a8b1998612224ed3f2c` | Unfinished in-memory per-Session registry and process owner adapter under `bake-coding-agent/src/jobs`; no model tools, producers, result delivery, or durable recovery |
| Windows configuration trust | `05e3bd27921b7a9c300b08364afc6d0a1cce0013` | Unfinished standalone [research probe](../../../rust/probes/windows-config-trust/README.md); Windows `mechanisms.rs` is missing, and no native CI job is installed |

The retained product choices are CLIProxyAPI only for 0.4.0, no Desktop integration, a native plugin API replacing Cordis management, Rust-only distribution with 0.3 maintenance ending at 0.4.0, and AppContainer with explicit Windows read/network grants. D32 excludes TypeScript session migration or runtime interoperability. The separate D24 parameter-surface work in PR #221 is outside this consolidation.

## Known failures and qualification gaps

These results belong to the specified source revisions. Passing a source branch does not qualify the combined draft. The PR body records the final handoff checks.

| Evidence | Result and limit |
|---|---|
| Wasm host `be2065f910` | Linux full preflight: 34 passed, one actionlint skip, 411 s. Intel x86_64 Mac: four native gates passed, 378 s. [CI run](https://github.com/Justar96/bake/actions/runs/39044768019) records the source branch's Windows result. |
| Installed-plugin source before final trust/MCP integration | Linux: 34 passed, one skip, 370 s. Intel Mac: four native gates passed, 381 s. The later trust change passed 613 coding-agent library tests (three ignored), 23 Wasm Session tests, seven print tests, and Linux/Windows/Darwin cross-target Clippy before the final rebase. No paired live eval exists for this consumer. |
| AppContainer `0697c33ee2` | Linux: four native gates passed, 180 s. [Windows run](https://github.com/Justar96/bake/actions/runs/39044471477) failed with 38 enforcement tests passing and two failing. `read_only_reads_the_workspace_and_denies_effects` wrote the AppContainer TEMP file with exit 0 instead of denied exit 13. `read_only_profile_storage_denies_every_mutation` exited 13 before its expected `done` marker. Package, PTY, and eval gates were skipped after the Rust failure. Status: both fixed on `develop` by `bf7a6b362f` (squash `99145d6fbd`, #248). The read-only pass now replaces the package's explicit ACEs with one read grant, instead of adding a deny ACE that the native access check ignored. The profile-storage probe reports an unreadable ancestor instead of exiting 13 before `done`. These have not been re-run on Windows since the combined draft's local run. |
| AppContainer before the MCP rebase | Linux full preflight: 33 passed, two skipped, 317 s. Intel Mac: four native gates passed, 298 s. These do not establish Mac evidence for `0697c33ee2` or the combined draft. |
| ConPTY `20c5c14fb4` | Linux full preflight: 34 passed, one skip, 365 s; 53 Rust PTY cases passed. Intel Mac: three native gates passed, one failed, 311 s; 49 PTY cases passed and two session-sheet cases failed. The `/session` assertion requires both `Alpha work` and `Messages` in the visible screen; the captured long-path sheet showed rows 1–12 of 23 without `Messages`. Root cause is unresolved. [Windows/Linux CI run](https://github.com/Justar96/bake/actions/runs/39044826893) retains the source branch's evidence. |
| Jobs and Windows trust refactor | Interrupted before worker qualification. The trust probe is outside the main workspace; a workspace pass does not compile it. Its [research snapshot](../../../rust/probes/windows-config-trust/research/design.md) and earlier prototype are preserved for continuation. |
| Combined draft `bf7a6b362f` | Linux full preflight: 34 passed, `rust-package` failing only for a missing cargo-about and then passing with it. Intel x86_64 Mac: four native gates passed, 326 s. Local Windows (build 26300) `preflight --only native`: `rust` (AppContainer included), package, and eval passed; ConPTY PTY passed 20, skipped the two MCP cases, and failed 12, mostly paste, Unicode caret, and image-staging timeouts, one inline resize residue, and one cancelled `sleep` not seen among the session's processes. Fixed on the draft: a jobs test race (Windows CI 39045970553), skills tests reading the real `~/.agents/skills` through Windows' profile-relative temporary directory, the Mac session-sheet PTY cases (a long `/var/folders` path pushed `Messages` below the sheet's page), and read-only AppContainer storage (package deny ACEs were ignored natively; the pass now replaces the package's ACEs with a read grant). |
| Merged MCP consumer | [CI](https://github.com/Justar96/bake/actions/runs/39042900206) passed. The [paired eval correction record](../../../evals/agent-loop/versions/v0.4.0/2026-10-17-rust-mcp-stdio-cli-correction/REPORT.md) preserves 288 valid samples: five models succeeded in all 48 samples each; GPT failures increased from 4/24 to 8/24. All 12 failures were runaway whitespace tool arguments (10 edit, two bash); the accepted regression and its limits remain recorded. Keep both the original and correction records immutable. |

AppContainer remains unqualified. The runner now has defenses and Windows-only tests for each open proof, and every one passed on Windows in [CI 39206590298](https://github.com/Justar96/bake/actions/runs/39206590298), loopback as a named gap. That run also left three blockers: a read-only container can create keys in its own profile's registry storage, Git fails on NUL, and loopback denial is unproven. Before a grant that would propagate, the runner refuses a tree holding a hard link, a reparse point, or a descendant with another owner. It records each run so that the next run in its namespace reclaims a killed runner's profile. Tests cover named objects and the registry, objects created after resume, Git's `/dev/null` (`NUL`), and loopback, which counts as denied only on a refusal code: a timeout is a named gap, never a denial, whatever the TCP table and the listener show. Git is expected to stay blocked while a container cannot open `NUL` for reading and writing. A timeout is not evidence of denied network access. Keep the default backend disabled until these tests pass natively.

The Wasm helper has no OS filesystem/network confinement. Empty WASI imports, memory, fuel, and deadlines are separate limits. Await shutdown before releasing a caller's resource slot; drop alone only requests termination. The Mac parent-death check retains a PID/start-time check-to-signal race, and the allocator environment workaround has an unmeasured performance cost. Installed plugins still lack the remaining contribution APIs and broader interactive management. Existing Windows MCP configuration and plugin enablement remain refused pending owner/DACL trust qualification.

ConPTY still needs console default-handler exit coverage, VT input forwarding (the likely cause of the paste and Unicode failures above), and restoration of the output VT-processing flag. The two MCP lifecycle PTY cases explicitly skip on Windows until trusted configuration reads exist. Apple Silicon execution is unqualified.

## Resume order

Resume only after the owner requests it. Keep Codex as the orchestrator and use Pi workers with `cliproxyapi/claude-opus-5-5`, medium effort, as requested. Re-check Pi's latest official release when starting a scope; the last source review pinned v1.3.0 at `71023aec1546d6d677a65669c5665c0927ce0dd0`. Preserve Pi's notices and port its tests with adapted code.

1. Check out the draft branch and read its latest check results. Review the integration changes and finish the interrupted jobs and probe work before treating their APIs as established. Run the repository's checks without bypassing hooks.
2. Run the AppContainer enforcement suite natively, including the scope-04 hardening cases (hard links, owners, reparse points, killed-runner reclaim, named objects, registry, objects after resume, `NUL` and Git, loopback), which are compile-checked only. Resolve the Mac session-sheet failures and obtain native Windows ConPTY evidence. Repeat Intel Mac qualification on the combined source; qualify Apple Silicon through the release matrix.
3. Add a real combined MCP/Wasm fixture proving the installed plugin veto applies to MCP tool calls and that session replacement awaits both owners. Run the standard and extended six-model paired suite, three trials per case, against a clean built final PR base. Record and explain every flagged regression. Do not reuse the MCP eval as plugin evidence.
4. Finish the native Windows trust probe and review native positive and negative controls before enabling configuration reads. Use the [probe handoff](../../../rust/probes/windows-config-trust/README.md) for the missing module and prototype limitations.
5. Continue the [durable execution plan](durable-execution.md): jobs and producer delivery, child agents and routing, goals, schedules, and restart recovery. MCP HTTP admission, OAuth, resources/prompts, tool search, and code mode ([design](code-mode.md)) remain open; further plugin contributions are deferred beyond 0.4.0 ([D43](scope-00/support.md#decision-register)).
6. Complete native installers, signing, update/rollback, platform minimums, and scope 17's TypeScript retirement only after release qualification. No release tag or merge is authorized by the pause request.

## Dev Note

This page is a continuation snapshot, not an additional decision register. The handoff applies formatting and one missing `CliEnvironment.plugins` initialization in an MCP test after merging the plugin consumer. It does not finish the interrupted features. Detailed private logs, worker transcripts, raw eval output, worktree snapshots, and ignored worktree evidence are preserved locally under `.preflight/rust-orchestration/pause-handoff/`; none is needed to recover the committed source. GitHub run links above may expire their artifacts, so retain useful failure evidence in future qualification records without exposing credentials or transcripts.
