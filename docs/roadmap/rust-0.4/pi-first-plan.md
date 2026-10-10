# Pi-first scope plan for the Rust harness

## Summary

This page re-plans roadmap scopes 02 to 17 around [D22](scope-00/support.md#decision-register) and [D32](scope-00/support.md#decision-register): the Rust implementation is ported primarily from Pi's latest official source, Bake's TypeScript runtime is ported selectively, and 0.4 is a new binary with no migration from a TypeScript install. For each scope it names the Pi owners to port, the Bake behavior that must survive, and the exit evidence that replaces 0.3 parity.

**Status: plan, 2026-10-09; D32 applied 2026-10-10.** Nothing here is implemented. The [scope specifications](README.md#scope-specifications) still hold the 0.3-parity text for scopes 02 to 17. Where they conflict with this page, this page governs, and each scope's first PR replaces its specification text.

## Table of Contents

- [Pinned Pi revision](#pinned-pi-revision)
- [Retained contracts](#retained-contracts)
- [Porting rules](#porting-rules)
- [Scope plan](#scope-plan)
- [Owner decisions](#owner-decisions)
- [Dev Note](#dev-note)

## Pinned Pi revision

Checked through the GitHub API on 2026-10-09.

| Reference | Revision | Use |
|---|---|---|
| [Latest release v1.1.0](https://github.com/earendil-works/pi/releases/tag/v1.1.0), published 2026-10-07 22:26 UTC | `abe508e1b89912adde45528136c3221eb69acdd7` | Default port source |
| [`main`](https://github.com/earendil-works/pi/commit/6fb2e7815167e6b19006fc526d1a5d0f5f998787), committed 2026-10-08 15:26 UTC | `6fb2e7815167e6b19006fc526d1a5d0f5f998787` | Unreleased changes, adopted only with a named reason |

Pi's packages at that main revision:
- `agent`: the loop and agent state;
- `ai`: the provider API and model discovery;
- `chord`: service composition, replicated state, RPC, and plugins;
- `coding-agent`: CLI, session services, and tools;
- `codemode`: confined JavaScript calling injected tools;
- `durable`: tasks and documents;
- `env`: remote execution environments;
- `mcp`: the MCP client;
- `protocol`, `client`, and `server`: transport-neutral remote sessions over CBOR;
- `telemetry`;
- `tui`: differential terminal rendering;
- `evals`.

Each scope re-checks these revisions when it starts, and records the revision it ports in its first PR. The older revisions in the [Pi reference](pi-reference.md) remain as history.

## Retained contracts

[D32](scope-00/support.md#decision-register) makes 0.4 a new binary that users install, with no migration from or interoperation with a TypeScript install. It keeps three Bake contracts, and every scope's exit evidence includes the parts it touches:

| Contract | What must hold | Evidence |
|---|---|---|
| CLIProxyAPI provider route | URL forms, catalog discovery with `client_version=pi`, per-model wire protocol (`openai-responses`, `openai-completions`, `anthropic-messages`), effort and limit backfill from vendor catalogs, login, refresh that keeps saved efforts, and logout ([setup](../../../apps/tui/packages/app/src/cliproxyapi.ts)) | Fixture tests from that file's behavior plus a live smoke test against a proxy |
| Sandbox, approvals, and permission presets ([D24](scope-00/support.md#decision-register)) | Bake's sandbox backends, approval prompts, and presets, ported into Pi's tool execution | Real denied effects on each OS (scope 04) |
| The `~/.bake` home ([D25](scope-00/support.md#decision-register)) | `BAKE_HOME`, then `DSH_HOME`, then `~/.bake`; the CLIProxyAPI route, key, and default model read from the existing settings and credentials, which stay unchanged | Fixture homes with 0.3 settings and credentials files |

Not retained: Bake Session logs of any format (0.4 writes Pi's session format), the model-visible surface (tools, schemas, results, prompts, and repair text follow Pi), the Desktop contract (D9), CLI flags and exits, and the settings and credentials file formats for new writes.

## Porting rules

1. **Start from Pi's owner, not Bake's package.** Port Pi's structure and code for the concern, then add only what a retained contract or an accepted Bake design rule needs.
2. **Pi's session is the log.** The native loop takes Pi's shape (explicit inputs, typed hook points, injected stream function) and writes Pi's session format; a resumed session rebuilds its context from that file as Pi's session manager does.
3. **Bake's design rules still apply:**
   - the [durable execution direction](durable-execution.md): owned tasks, intent before effect, replay classes, and one ownership tree;
   - [defensive patterns](../../defensive-patterns.md): awaited teardown and no late callbacks;
   - the [harness audit](#dev-note) recommendations: typed stages instead of open waterfalls, and one owner per agent.
4. **Where Pi has no equivalent, port Bake selectively.** Examples are the sandbox, approvals, subagents, goals, jobs, schedules, hooks, and the web tools. Keep the parts the model sees; redesign the rest within Pi's structure.
5. **Provenance.** Each PR records the Pi revision and files it adapts and keeps Pi's MIT notice. Each Bake-derived behavior names its TypeScript source. Once scope 09 lands, a change to anything the model sees needs an eval record of the Rust agent.

## Scope plan

| Scope | Port from Pi | Port selectively from Bake | Exit evidence |
|---|---|---|---|
| 02 and 03 Session and storage | `coding-agent/src/core/session-manager.ts` and its tests: the JSONL entry tree, branching, and resume | Session files under the Bake home | Pi's session tests, ported; resume after a killed writer |
| 04 Filesystem, processes, and sandbox | `coding-agent/src/core/{exec.ts, bash-executor.ts, tools/powershell.ts, tools/output-accumulator.ts, tools/file-mutation-queue.ts}` for spawning, output handling across chunk boundaries, and mutation ordering | The sandbox backends (bubblewrap, Landlock, Seatbelt, Windows restricted token), approval prompts, permission presets, and process-tree ownership ([D24](scope-00/support.md#decision-register)) | Denied effects and process-tree quiescence on each OS; Pi's chunk-boundary regressions as tests |
| 05 Composition, settings, credentials, and lifecycle | `chord` (services, plugins, lifetimes), `coding-agent/src/core/{settings-manager.ts, auth-storage.ts, model-registry.ts, resource-loader.ts, agent-session-services.ts}` | Ownership-tree teardown rules; CLIProxyAPI credential and route storage; read-only import of the CLIProxyAPI route, key, and default model per [D25](scope-00/support.md#decision-register) | Scoped teardown with no surviving task; a CLIProxyAPI login survives restart; existing route imported |
| 06 Provider-neutral streaming and failures | `ai/src/{types.ts, utils/event-stream.ts, utils/provider-retry.ts}` | Nothing beyond CLIProxyAPI's needs | Pi's stream assembly, cancellation, and retry classification tests, ported |
| 07 Provider protocols and authentication | `ai` providers for `openai-responses`, `openai-completions`, and `anthropic-messages`, and Pi's model discovery | All of CLIProxyAPI (retained); other routes per [D8](scope-00/support.md#decision-register) | CLIProxyAPI fixtures plus a live smoke test; wire fixtures for each protocol |
| 08 Tool dispatch and local coding tools | `agent/src/agent-loop.ts` prepare, execute, and finalize phases; `coding-agent/src/core/tools/{read,write,edit,edit-diff,bash,grep,find,ls,truncate}.ts` | Approval and denied-call results (D24) | Pi's tool tests, ported; denied calls under each preset |
| 09 Agent loop and headless slice | `agent/src/{agent-loop.ts, agent.ts}`, `coding-agent/src/core/{agent-session.ts, agent-session-runtime.ts, session-manager.ts}`, print mode | The CLIProxyAPI route and the `~/.bake` home | End-to-end edit, resume, and cancel against a fake and a live CLIProxyAPI model; an eval record of the Rust agent |
| 10 Context, skills, attachments, and compaction | `coding-agent/src/core/{compaction/, skills.ts, prompt-templates.ts, system-prompt.ts}` | Skills roots under the Bake home | Pi's compaction tests, ported; reconstruction after compaction; long-session eval |
| 11 Subagents, goals, background jobs, and schedules | `durable` task states and waiting policies as structure; `nested-tool-calls.ts` | Subagent, goal, job, and schedule tool surfaces and their logged events (`subagent/catalog`, goal events) | Restart and race proofs; no duplicate completion notice after restart |
| 12 MCP, web, hooks, code mode, and plugins | `mcp` (already the [MCP direction](pi-reference.md#native-mcp-direction)), `codemode`, `coding-agent/src/core/extensions/` and `chord` plugins for the [D6](scope-00/support.md#decision-register) API | `run_code` language and bindings ([D17](scope-00/support.md#decision-register)), web tool surfaces, hook decisions logged before effects, `mcp__<server>__<tool>` names | MCP conformance arm; containment tests; a named native plugin consumer |
| 13 CLI, transports, and Desktop | `coding-agent/src/{cli.ts, main.ts, modes/}`, and `protocol`, `client`, `server` for remote sessions | Desktop only if [D9](scope-00/support.md#decision-register) keeps it | Built entry points; remote-session protocol tests; the Desktop decision applied |
| 14 Terminal engine and rendering | `tui` differential rendering and components as reference | [D19](scope-00/support.md#decision-register) and the [terminal direction](terminal.md) still govern: Ratatui and Crossterm with a Bake-owned composer | Inline and fullscreen PTY cases, Unicode, scrollback, restoration |
| 15 Interactive workflows | `coding-agent` interactive mode, slash commands, keybindings | The model-visible effects of commands; transcript replay from the log | PTY scenario mapping for the commands 0.4 keeps |
| 16 Distribution, update, and rollback | None; Pi's package manager stays excluded by [D5](scope-00/support.md#decision-register) | Updater, signing, and channels ([D14](scope-00/support.md#decision-register)) | Signed install, update between 0.4 releases, and rollback on all five targets; a fresh install beside 0.3 leaves the 0.3 install working |
| 17 Qualification and release | `evals` as a reference for the eval runner | Eval procedure and the ledger | Retained-contract evidence complete; evals recorded |

Under D32, scopes 02 and 03 port Pi's session manager instead of Bake's Session format. The Bake-format readers, migrations, writer, fault and cross-runtime harnesses, and conformance tables built for them are deleted; the ledger keeps their records as history.

## Owner decisions

**D32, no TypeScript user migration (2026-10-10).** 0.4 is a new binary that users install. It writes Pi's session format, keeps the three contracts above, and drops Bake Session compatibility and the model-surface contract. It supersedes D4, D23, and D31.

Two earlier decisions were added to the [support register](scope-00/support.md#decision-register) with this plan and decided by request on 2026-10-09:

- **D24, sandbox and permissions.** Pi runs tools without a sandbox or Bake's approval policy. 0.4 keeps Bake's sandbox backends, approval prompts, and permission presets, ported selectively into Pi's structure in scope 04, so tool calls stay confined and approved as they are today.
- **D25, existing settings and credentials.** Their file formats are not retained. On first run, 0.4 imports the CLIProxyAPI route and key and the default model from `~/.bake/settings.yaml` and the credentials file, read-only, so the retained provider works without signing in again.

## Dev Note

The harness audit that motivated the porting rules was a session report, not a committed document. Its findings are carried here as rules 2 and 3, and in the [durable execution direction](durable-execution.md). Re-pin the Pi revision at the start of each scope.
