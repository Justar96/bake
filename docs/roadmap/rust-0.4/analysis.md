# Rust migration: codebase analysis

## Summary

Bake's migration difficulty is concentrated in behavioral compatibility: reconstructable model requests, immutable session generations, scoped plugin lifetimes, platform confinement, provider-specific replay, and terminal ownership. Rust can replace the implementation, but none of those obligations disappears with the language change. This analysis explains the ordering and test requirements in the [roadmap](README.md).

The source baseline is `ae5eb51ab61f2266b0fc2ff52f2f14b9f3a9a917` on `origin/develop`, inspected on 2026-10-07. Findings combine tracked-file inventory, profile composition, implementation reads, existing test inspection, and the focused executions recorded in [verification](verification.md#planning-session-evidence). This is an architecture and migration analysis, not a line-by-line audit or a claim of complete test coverage.

## Table of Contents

- [Measured inventory](#measured-inventory)
- [Execution and ownership](#execution-and-ownership)
- [Migration obligations](#migration-obligations)
- [Subsystem disposition](#subsystem-disposition)
- [Existing evidence and gaps](#existing-evidence-and-gaps)
- [Decisions before implementation](#decisions-before-implementation)
- [Dev Note](#dev-note)

## Measured inventory

The inventory uses `git ls-files` at the baseline commit. Source counts include tracked `.ts`/`.tsx` files below `src/`; the native row also includes `.c`, `.cc`, `.h`, and `.rs`. Line counts are physical lines, including comments and generated content. Test counts include `.test.ts`, `.spec.ts`, `.e2e.ts`, and their TSX equivalents; they count files, not test cases or executed coverage. Vendored source, dependencies, build output, and session snapshots are outside these figures.

| Area | Source files | Source lines | Test files |
|---|---:|---:|---:|
| Shared `packages/` | 676 | 158,150 | 600 |
| CLI | 16 | 2,107 | 39 |
| Terminal app, UI, harness | 79 | 21,980 | 94 |
| Native support | 4 | 646 | 0 by the TypeScript suffix rule |
| Repository scripts | Not measured as `src/` | Not measured as `src/` | 61 |
| Evals | Not measured as `src/` | Not measured as `src/` | 4 |

There are **157 direct shared package manifests**, **771 application/runtime source files**, and **182,237 application/runtime source lines**. Five additional native `*.test.js` files live under [native/system/test](../../../native/system/test/). No tracked `.rs` or `Cargo.toml` exists at this baseline. These measurements show migration breadth; they do not justify a lines-per-day schedule or translating every package into a crate.

Large owners deserve decomposition by behavior. Examples include [tool dispatch](../../../packages/core/tools/src/index.ts), [JSONL persistence](../../../packages/session/session-persistence-jsonl/src/index.ts), [Session](../../../packages/core/session/src/index.ts), [TUI controller](../../../apps/tui/packages/app/src/controller.ts), and [provider catalog](../../../packages/llm/llm-pi-ai/src/catalog.ts). The 5,605-line Cordis API catalog is generated source, so treating all source lines as handwritten implementation would overstate that work.

## Execution and ownership

The [base bundle](../../../packages/bundle/base/cordis.patch.yml) composes persistence, LLM services, settings, credentials, tool/policy infrastructure, subagents, goals, code mode, and web capabilities. The [headless overlay](../../../packages/bundle/headless/cordis.patch.yml), [terminal overlay](../../../apps/tui/packages/app/cordis.built.patch.yml), and [Desktop overlay](../../../packages/bundle/desktop/cordis.patch.yml) specialize that composition. Agent presets add scoped services and tools. Package presence alone does not prove that a feature is active in every profile.

The Node launcher resolves profile bundles and ordered patches before Cordis activates services. [Profile code](../../../packages/boot/app-boot/src/profile.ts) accepts both `bake.profile` and earlier `dsh.profile` metadata. Existing bundle YAML contains `!!js`, service access, environment expressions, and platform checks. Therefore a generic Rust YAML decoder would not be a compatible profile loader.

The [agent driver](../../../packages/core/agent-loop/src/agent.ts) claims inbox input, runs pre-step middleware, prepares the model route, admits durable prompt/input, logs request metadata, assembles the request, streams the model response, executes tools, and advances the turn. Route admission ordering prevents cancellation or preparation failures from recording input that was never admitted. The [tool scheduler](../../../packages/core/agent-loop/src/tool-calls.ts) runs parallel-safe calls concurrently but commits their results and added context in model order; exclusive calls are barriers, and calls can be reclassified before dispatch.

The [Session](../../../packages/core/session/src/index.ts) and [projection registry](../../../packages/session/session-projection/src/index.ts) own durable state. The terminal owns drafts, navigation, and live display state. It reads authoritative runtime projections for activity, inbox, permissions, token usage, and context pressure. Replacing Ink does not authorize a second conversation reducer in the frontend.

## Migration obligations

### Persisted data is a protocol

The [writer constant](../../../packages/core/session/src/types.ts) is currently Session format 3. The adjacent format packages retain v0→v1→v2→v3 conversion. [Retired vocabulary](../../../packages/session/session-format-catalog/src/retired-vocabulary.ts) still admits historical records for removed products, including workflow/team/web-related events. Deleting their declarations because Rust has no producer would break old sessions.

[Generation storage](../../../packages/session/session-persistence-jsonl/src/generation.ts) validates and publishes versioned successors while preserving predecessors. [Format helpers](../../../packages/session/session-persistence-jsonl/src/format.ts) define header omission rules, filenames, and path encoding. [Zstandard handling](../../../packages/session/session-persistence-jsonl/src/zstd.ts) has framing and truncation semantics beyond “decompress this file.” A Rust port must compare decoded rows and admission failures, not require compressed bytes to match a different compressor.

[Writer leases](../../../packages/session/session-persistence-jsonl/src/lease.ts) use POSIX flock and Windows LockFileEx, with stable-file identity checks. A generic file-lock crate is acceptable only if it interoperates with these exact locks. Fresh Rust-versus-Rust lock tests alone cannot prove coexistence with an installed 0.3 process.

### JavaScript semantics can change model input

Rust strings count bytes by default; existing code uses both byte limits and JavaScript string lengths. Scope owners must identify whether each bound means UTF-8 bytes, UTF-16 code units, Unicode scalar values, graphemes, or terminal cells. Tool truncation, schema serialization, and compaction summaries can change model behavior even when ASCII tests pass.

Other hazards include safe-integer limits, negative zero rejection, missing fields versus null, dictionary iteration order, stable tool ordering, and lossless JSON validation. Serde defaults do not establish parity. Preserve the existing wire representation and compare exact model-visible strings; normalize only documented nondeterminism.

### Lifecycle is a correctness requirement

Cordis provides scoped registrations, service availability, ordered events, and effect disposal. The replacement needs equivalent ownership rules, not a global bag of callbacks. Every agent, process, request, watcher, timer, VM, and subscription needs one owner and a completion signal. Cancellation without joining owned work can leave a child writing after the parent has closed its session or released the terminal. The source obligations are summarized in [defensive patterns](../../defensive-patterns.md).

Rust can organize these guarantees around fewer owners and explicit interfaces. For example, one agent task can append an inbox event, update its inbox, and then publish the resulting view. Keeping that sequence within the owner prevents the frontend from observing a partially updated view.

Keep durable event order separate from task completion order. A Rust channel does not by itself preserve the scheduler's policy checks, exclusive barriers, or model-ordered commits. Crash recovery must distinguish a call that never started from one with unknown external effects; automatically retrying both could duplicate writes.

### Provider migration exceeds three HTTP clients

[pi-ai provider construction](../../../packages/llm/llm-pi-ai/src/provider.ts) explicitly exposes three generic route protocols, but catalog routes reuse the installed provider's own implementation. That includes authentication and transports which a base URL plus API key cannot express. The [adapter](../../../packages/llm/llm-pi-ai/src/adapter.ts), [replay conversion](../../../packages/llm/llm-pi-ai/src/replay.ts), [payload processing](../../../packages/llm/llm-pi-ai/src/payload.ts), and [login](../../../packages/llm/llm-pi-ai/src/login.ts) also carry behavior.

The support inventory must distinguish configured wire overrides from catalog providers, and API-key login from OAuth/device/browser flows. Reasoning signatures, tool schemas, in-history prompt/tool changes, cache fields, image limits, proxy routing, and retry classification require fixtures. A generic SDK with a familiar provider name is not enough to claim those behaviors match.

### Native execution remains platform-specific

[Sandbox selection](../../../packages/sandbox/sandbox-local/src/index.ts) chooses Linux bubblewrap then Landlock, macOS Seatbelt, and Windows restricted-token/ACL execution. The API reports enforcement completeness. Windows job ownership, handle release, path aliases, PowerShell behavior, POSIX process groups, and inherited descriptors remain separate implementation work.

The existing native system package already supplies C primitives. Preserve its licenses and independently test its behavior even if Rust later replaces the bindings. Avoid coupling the language migration to a speculative new sandbox design.

### Code mode intentionally executes JavaScript

The current [code-mode package](../../../packages/ptc-runtime/ptc-runtime-codemode/package.json) uses pi-codemode, and its [worker](../../../packages/ptc-runtime/ptc-runtime-codemode/src/worker.ts) controls message/output limits around a QuickJS implementation. The native host must preserve the model-facing `run_code` language, tool bindings, result ordering, cancellation, and resource limits. Removing Node does not require removing a confined JavaScript interpreter.

MCP similarly delegates to an SDK and registers server-qualified tools with scoped lifetimes. Port the [client behavior](../../../packages/mcp/mcp-client/src/index.ts), including server instructions and dynamic tool schemas, rather than substituting a handshake-only client.

### Cordis extensions need a product decision

The [plugin manager](../../../packages/boot/plugin-manager/README.md), [Cordis host runner](../../../packages/extensions/cordis-host-runner/README.md), and [Cordis tool](../../../packages/extensions/tool-cordis/README.md) expose JavaScript package and runtime operations. Arbitrary plugins can depend on Cordis services and lifecycle behavior that cannot be transparently represented by a small JSON-RPC adapter.

The product owner decided on 2026-10-07 that 0.4 ships Rust only ([D5](scope-00/support.md#decision-register)): no Node/TypeScript compatibility runtime ships, so custom Cordis/JavaScript profiles need migration. The old whole-profile runtime stays runnable in development as the comparison oracle. The owner also decided that 0.4.0 requires a native plugin API ([D6](scope-00/support.md#decision-register)); MCP servers and hook processes alone do not satisfy it. Its form is not chosen; design it around a named consumer. Before release, the Cordis preset, tools, and plugin manager each need a migration or native replacement, or an explicit support-change decision. Never feed an obsolete Cordis API catalog to the model while executing an unrelated native interface.

### Bake Desktop is a retained consumer

The [Desktop transport](../../../packages/bundle/desktop/src/transport.ts) supports both newline-delimited stdio and Electron's `process.parentPort`. Its [protocol declaration](../../../packages/bundle/desktop/src/protocol.ts) is shared with a separate repository. A native executable cannot be substituted for a JavaScript Electron utility-process entry without coordinating the launch path or providing a tested adapter.

Preserve `dsh --profile desktop`, the root-agent lifecycle, permission tiers, approval withdrawal, stream messages, and trace relationships. The removed upstream Desktop application is unrelated to this retained Bake integration. Access to the separate Desktop checkout is a qualification dependency; this analysis did not test that external consumer.

### Terminal behavior is larger than widgets

[TUI design](../../../apps/tui/DESIGN.md) and [layout design](../../../apps/tui/DESIGN-LAYOUT.md) specify inline and alternate-screen behavior, bounded live output, Unicode input, authoritative projections, transcript replay, mouse selection, and restoration. Patched Ink, string-width, slice-ansi, and node-pty dependencies indicate behavior that needs explicit native test cases.

The existing [PTY driver](../../../apps/tui/scripts/pty-smoke.ts) exercises real built profiles, not isolated widget snapshots. It assumes Node/profile/replay setup, so native launch support must be added without changing the success conditions. Native Windows terminal coverage must be supplied separately from this POSIX driver.

### Upgrade and rollback depend on archive layout

The [updater launch check](../../../packages/boot/updater/src/verify.ts) looks for `apps/cli/lib/bin.js` and launches it with Node. Merely replacing the release archive with a Rust executable would fail this check on old installs. The route is open, and it must end in Rust-only shipped artifacts ([D14](scope-00/support.md#decision-register)). Pick and test it before native packaging is finalized.

The [manifest](../../../packages/boot/updater/src/manifest.ts) is signed over its exact bytes and names one product version with platform artifacts. The release workflow and updater do not currently implement independent 0.3/0.4 channels. Native rollback must verify credentials, profile manifests, settings, session generations, attachment references, and indexes as well as the executable pointer.

### Home defaults have two layers

The [release launcher](../../../scripts/release/bake) selects Bake's home and exports both environment spellings. The shared [home helper](../../../packages/util/home-paths/src/index.ts) retains a legacy `~/.dsh` library default and legacy model-visible labels. A Rust port must reproduce the product launch behavior and compatibility aliases deliberately; copying the library default alone could point Bake at upstream user data. Compare real launcher runs in isolated homes, including blank environment values, instead of inferring the behavior from one helper's name or documentation.

## Subsystem disposition

This map assigns every direct shared package group to a migration owner. It identifies intended work, not approval to delete packages. Scope 00 must expand it to individual packages, optional entry points, and actual consumers.

| Existing groups or area | Disposition | Owning scopes |
|---|---|---|
| `core`, `util` | Port public values/state/policy contracts; consolidate implementation utilities | 01–02, 05–09 |
| `session`, `session-query`, `storage` | Preserve formats, generations, replay, query and cache behavior | 02–03; telemetry in 13 |
| `subprocess`, `sandbox`, `terminal` | Native platform backends and resource ownership | 04, 14 |
| `boot`, `settings`, `credentials`, `identity` | Native configuration, auth storage, lifecycle, identity, updater | 05, 07, 13, 16 |
| `llm` | Replace SDK-backed behavior with qualified adapters | 06–07 |
| `fs`, `shell`, `interaction`, `spill`, `guard` | Port tools, permissions, questions, errors, progress and bounds | 04, 08, 15 |
| `preset`, `context`, `skill`, `attachment`, `compaction` | Preserve composition and model-facing context | 05, 10 |
| `subagent`, `goal`, `jobs`, `schedule` | Port durable orchestration and restart behavior | 11 |
| `mcp`, `web`, `hooks`, `ptc-runtime`, `extensions` | Native integrations; Cordis extensions need migration or an approved support change | 12 |
| `api`, `client`, `host`, `typert` | Trace retained consumers; preserve required wire/API behavior; replace TS-specific generation where justified | 13 |
| `bundle`, CLI | Native profiles and supported launch contracts, including Desktop | 09, 13 |
| `runtime-diagnostics`, `feedback`, Session telemetry | Preserve opt-outs, local feedback, bounded shutdown, identity/redaction; replace V8-specific measurements with native measurements | 13, 17 |
| TUI app/UI/harness | Port product behavior; adapt recorded fixtures and preview tools where still useful | 14–15 |
| `test-support`, scripts, evals | Retain language-independent oracles; add native drivers and reviewed generators | 00–01, all scopes |
| `native`, `vendor`, licenses | Keep proven support until replacement is qualified; retain attribution | 04, 16–17 |
| Removed upstream products | Do not restore Web, upstream Desktop, ACP, Python SDK, docs site, or upstream automation | Excluded |
| Frozen Session/eval/history records | Reuse read-only or copy into private test roots; never rewrite for a passing test | All scopes |

The retained `web` group supplies agent web tools; it is not the removed web application. `api`/`client`/`host`/`typert` presence alone does not justify reviving that application or deleting transport consumers.

## Existing evidence and gaps

The current repository has strong reusable oracles: pure Session properties, request reconstruction, real writer contention, compressed-generation compatibility, protocol fixtures, model-surface snapshots, built-profile tests, terminal PTYs, and paired agent-loop evals. [Testing policy](../../testing.md), the [runtime config](../../../vitest.config.ts), [integration config](../../../vitest.e2e.config.ts), and [preflight implementation](../../../scripts/preflight.ts) determine what runs; inherited prose may mention removed gates.

Important gaps for the migration are cross-language fixtures/comparison, Rust process launch in the eval and PTY drivers, native/provider catalog qualification, old-updater/new-archive compatibility, actual Electron launch verification, and a complete per-OS native terminal matrix. The [performance diagnostic](../../../apps/tui/packages/app/performance/README.md) explicitly has no calibrated CI timing threshold and excludes several interactive workloads. It is a starting instrument, not release performance proof.

Tests that only assert a mocked return value cannot prove sandbox denial, process cleanup, disk durability, or a successful coding task. Tests that pass on Linux cannot qualify Windows locks, ACLs, ConPTY, or held-executable update behavior. Live-model evals measure task outcomes and cost; they cannot replace deterministic request/permission/session equivalence tests.

## Decisions before implementation

Scope 00 must settle these points in a concrete support matrix. Recommended defaults make progress possible without pretending incompatible APIs are interchangeable.

| Decision | Recommended default | Evidence needed before acceptance |
|---|---|---|
| Native completion | Native standard, minimal, and code-mode agent paths plus terminal/headless/Desktop; explicit disposition for the Cordis preset and custom plugins | Per-profile command/tool/config/provider inventory |
| Session version | Preserve current format 3 unless a separately justified structural change is required | Cross-runtime write/read/reconstruction and historical migration tests |
| Extension strategy | Built-ins in Rust; ship Rust only, with migration for custom Cordis/JavaScript profiles (decided); keep TypeScript as the development oracle; require a native plugin API in 0.4.0 (decided), designed around a named consumer | Real plugin lifecycle and migration fixtures; support-change decision if parity is narrowed |
| Desktop integration | Coordinate native child spawning or test an Electron port-to-stdio adapter | Separate Desktop consumer test on supported hosts |
| Provider breadth | Inventory catalog routes as well as three generic wire overrides; preserve claimed login flows | Fixture and live-smoke matrix with no implicit unsupported routes |
| 0.3 maintenance | Bug fixes only; define post-0.4 maintenance PRs and separate update channel before cutover | Branch-policy and signed-channel release rehearsal |
| Performance | Compare identical workloads/process trees on fixed hosts; freeze budgets before native results | Repeatable TypeScript measurements with confidence/noise characterization |

## Dev Note

No specific Rust dependency version, FFI design, OAuth library, JavaScript engine binding, or terminal renderer has been qualified here. The roadmap deliberately schedules those experiments before their dependent behavior ports. Read [verification](verification.md) for the exact limits of the planning-session test evidence.
