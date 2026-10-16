# Native plugin design for the Rust harness

## Summary

This page proposes the native plugin API that [D6](scope-00/support.md#decision-register) requires for 0.4.0, and the inspection and management capabilities that replace the retired Cordis tooling under [D7](scope-00/support.md#decision-register). Trusted code compiled into `bake-rs` implements a Rust trait. Third-party plugins are Rust crates compiled to `wasm32-wasip2` and run as WASI 0.2 components in Wasmtime, with no ambient authority: the only effects they have are a few bounded host calls. Both kinds go through one ordered dispatcher whose model-visible effects are written to the session file in Pi's existing entry types. OS tools and MCP servers stay separate processes under [D24](scope-00/support.md#decision-register); they are not a way for a plugin to run native code. 0.4.0 has no native-process or dynamic-library plugin adapter.

**Status: proposed design, 2026-10-17.** Nothing here is implemented, built, measured, or qualified. This is a proposal under D6 and D7 for review, not an accepted decision; the names and technical exclusions are this design's own choices, which review may change. Facts in [Sources](#sources) come from the linked primary sources or local code. Everything after that section is proposed behavior, and each protection it describes is an obligation the [qualification gates](#implementation-choices-and-qualification-gates) must prove.

## Table of Contents

- [Sources](#sources)
- [Architecture](#architecture)
- [Contribution contracts](#contribution-contracts)
- [Host calls and concurrency](#host-calls-and-concurrency)
- [Containment and limits](#containment-and-limits)
- [Inspection and management](#inspection-and-management)
- [Named consumer and tests](#named-consumer-and-tests)
- [Implementation slices](#implementation-slices)
- [Implementation choices and qualification gates](#implementation-choices-and-qualification-gates)
- [Dev Note](#dev-note)

## Sources

| Source | Revision or date | Finding used here |
|---|---|---|
| Pi [`extensions/runner.ts`](https://github.com/earendil-works/pi/blob/71023aec1546d6d677a65669c5665c0927ce0dd0/packages/coding-agent/src/core/extensions/runner.ts), [`extensions/types.ts`](https://github.com/earendil-works/pi/blob/71023aec1546d6d677a65669c5665c0927ce0dd0/packages/coding-agent/src/core/extensions/types.ts), [`agent-session.ts`](https://github.com/earendil-works/pi/blob/71023aec1546d6d677a65669c5665c0927ce0dd0/packages/coding-agent/src/core/agent-session.ts) | v1.3.0, `71023aec1546d6d677a65669c5665c0927ce0dd0` | `emitToolCall` returns the first `block`; a throwing handler makes `agent-session.ts` block the call ("Extension failed, blocking execution"). `tool_result` handlers chain, and a failure is reported and skipped. `input` transforms chain and `handled` stops the chain. `context` handlers chain over a snapshot. Handlers may mutate tool arguments in place |
| Pi [`examples/extensions/protected-paths.ts`](https://github.com/earendil-works/pi/blob/71023aec1546d6d677a65669c5665c0927ce0dd0/packages/coding-agent/examples/extensions/protected-paths.ts) | v1.3.0, same revision, MIT | A `tool_call` veto on `write` and `edit` to listed paths |
| Pi [`chord/docs/spec.md`](https://github.com/earendil-works/pi/blob/71023aec1546d6d677a65669c5665c0927ce0dd0/packages/chord/docs/spec.md) | v1.3.0, same revision, MIT | Ordered lifecycle, and forced disposal that rejects new calls, aborts in-flight calls, then cleans up. Its dependency-injection framework and its version rule are not adopted |
| Zed [developing extensions](https://zed.dev/docs/extensions/developing-extensions), [capabilities](https://zed.dev/docs/extensions/capabilities) | Read 2026-10-16 | A production Rust host loads Rust extensions compiled to `wasm32-wasip2` and grants `process:exec`, downloads, and npm installs as user-restrictable capabilities |
| Wasmtime [security](https://docs.wasmtime.dev/security.html), [interrupting execution](https://docs.wasmtime.dev/examples-interrupting-wasm.html), [WASIp2 plugins](https://docs.wasmtime.dev/wasip2-plugins.html), [`WasiCtxBuilder`](https://docs.rs/wasmtime-wasi/latest/wasmtime_wasi/struct.WasiCtxBuilder.html), [`StoreLimitsBuilder`](https://docs.rs/wasmtime/latest/wasmtime/struct.StoreLimitsBuilder.html), [`ResourceLimiter`](https://docs.rs/wasmtime/latest/wasmtime/trait.ResourceLimiter.html), [`WasmList`](https://docs.rs/wasmtime/latest/wasmtime/component/struct.WasmList.html), [`WasmStr`](https://docs.rs/wasmtime/latest/wasmtime/component/struct.WasmStr.html), [`Config`](https://docs.rs/wasmtime/latest/wasmtime/struct.Config.html), [`Component`](https://docs.rs/wasmtime/latest/wasmtime/component/struct.Component.html) | Read 2026-10-16 and 2026-10-17 | A guest reaches only what the host links. The default WASI context has no preopens, environment, or arguments, closed stdin, real clocks, unmetered random generators, and denied socket addresses and name lookup; a builder setting caps one random call's length (64M by default) but no total. `StoreLimits` counts instances, memories, and tables per `Store`, but its memory and table-element caps apply to each memory or table individually; a custom `ResourceLimiter` sees every growth request, and it does not limit Wasmtime's own or the embedder's allocations. `WasmList` and `WasmStr` are lazy views of guest memory returned from a `TypedFunc`; `bindgen!` bindings cannot use them. Fuel and epoch deadlines interrupt guest code only, not a host function the guest awaits. `Component::deserialize` is `unsafe` because it trusts its bytes |
| Wasmtime [v49.0.3](https://github.com/bytecodealliance/wasmtime/releases/tag/v49.0.3) | Published 2026-10-14 | Latest release when checked. Its `Cargo.toml` declares `rust-version` 1.96.0, below Bake's pinned 1.99.0. That is declared compatibility only: no build, dependency, or `wasm32-wasip2` target check has run |
| Component model [WIT](https://component-model.bytecodealliance.org/design/wit.html) | Read 2026-10-16 | Versioned packages, typed records, variants, and resources. Record field shapes participate in type matching, so adding a field changes the type |
| Nushell [plugin protocol](https://www.nushell.sh/contributor-book/plugin_protocol_reference.html), [Extism](https://extism.org/docs/overview) | Read 2026-10-16 | Comparisons only: a native-process protocol, and a bytes-in, bytes-out layer over Wasmtime. Neither becomes a dependency |
| Rust reference, [ABI](https://doc.rust-lang.org/reference/abi.html) | Read 2026-10-16 | The Rust ABI is unspecified across compilers, so a `dylib` plugin would be tied to Bake's exact toolchain |

The Bake home is resolved by `resolve_bake_home` in [`home.rs`](../../../rust/crates/bake-coding-agent/src/home.rs): `BAKE_HOME`, then `DSH_HOME`, then `~/.bake`; this page writes it `$BAKE_HOME`. `bake-sandbox` confines file writes by its confined tree; reads, network, and process visibility stay outside its vocabulary ([`rust/crates/bake-sandbox/src/lib.rs`](../../../rust/crates/bake-sandbox/src/lib.rs)). A native plugin process could therefore read the home's credentials and open sockets, which is why this design excludes one.

## Architecture

```text
 compiled-in Rust ─┐                        ┌─► Pi session file
 (Plugin trait)    ├─► dispatcher ─► agent ─┤   (custom, custom_message,
 Wasm component  ──┘   (ordered,     loop   │    context_edit, toolResult)
 (bake:plugin WIT)      snapshot)           └─► tools ─► D24 engine ─► sandbox
        │
        └─ host calls only: log · append-entry · state · session-info
```

| Class | Mechanism | Trust |
|---|---|---|
| Built-in | A Rust trait in `bake-coding-agent`, registered at compile time | Bake's own code |
| Third-party | The WIT package `bake:plugin@0.1.0`, whose world imports `host` and exports the `plugin` interface, on WASI 0.2, guest target `wasm32-wasip2`, one Wasmtime `Store` and one component instance per plugin (which may contain several core instances) | None ambient; only the host calls below |
| OS tools, MCP | Unchanged separate processes, scopes 04 and 12 | D24 confinement and approvals |
| Native process or `dylib` | Not in 0.4.0 | No qualified read, network, and process sandbox exists |

Guest exports are synchronous WIT functions driven by Wasmtime's host-side async calls; WASI 0.3 and async guest exports are outside this design. Both classes share the dispatcher and lifecycle code.

## Contribution contracts

Plugins run in configured `order`, then by id, over a registry snapshot taken at the turn boundary. No contribution can forge a system message, an assistant message, or a tool result, or separate a tool call from its result.

| Contribution | Order and composition | Failure (trap, timeout, invalid return) | What the session file holds |
|---|---|---|---|
| `on-tool-call` veto: `none` or `block {reason, terminate}`, for the tool names its manifest lists | Before the tool runs; the first block wins, as in Pi. It returns no allow and no argument rewrite, because the persisted assistant `toolCall` must stay what ran. The D24 checks inside the tool still apply | Blocks the call, as Pi does. While the plugin is failed, restarting, out of restarts, or unloadable at startup, every call to a listed tool is blocked with an error result naming the plugin; only the user disabling it removes the veto | A `custom` entry `bake.plugin.decision` before the denial `toolResult`; the denial text is the persisted result |
| `on-tool-result` patch: `content` (text only) and `is-error` | Chains in order, as in Pi; it cannot change the call id or tool name | That plugin is skipped | The final patched `toolResult` itself, with `details.bakePlugins` naming each patching plugin and version |
| `on-context` contribution | Before a request, at a point where no tool call is open. Allowed: replace the text of a user, custom, or tool-result entry (role, id, and call id kept); omit a user or custom entry; append one custom message. System, assistant, and tool-result omission are refused | Contribution dropped | Pi `context_edit` and `custom_message` entries appended before the request, so rebuilding the request needs no plugin. Unlike Pi's per-request `context` hook, these edits persist |
| `on-input`: `continue`, `transform(text)`, or `handled` | Chains; `handled` stops, as in Pi | Input passes through | A `custom` entry `bake.plugin.input` records each non-`continue` decision: the plugin, its version, the decision, and the original input. After a transform the final text is the persisted user message; after `handled` no user message is written, and the entry alone records that the input was consumed |
| Plugin tools | Ordinary tools named `<plugin>_<tool>`, which cannot shadow a built-in; other plugins' vetoes apply, and the plugin's own do not. Each declares a [replay class](durable-execution.md#2-record-intent-before-effect-and-classify-replay), defaulting to `report`; 0.4 never automatically reruns it | An error `toolResult` | The `toolResult` |
| Commands | `/name` with text output to the user only | Error shown | Nothing model-visible |
| Lifecycle observers: session start and shutdown, turn start and end, tool end, agent settled | Delivered in order through a bounded per-plugin queue (64 events); overflow drops the oldest and is counted. Enqueueing does not wait for the guest; foreground calls take priority over queued observers, and every observer call has the same deadline as other guest calls | The instance fails | Nothing, or `custom` entries through `append-entry`. An observer cannot reach model context or model state, directly or through another contribution |

Bounds before the model sees anything: a reason or patch of at most 16 KiB, an inserted message of at most 8 KiB, and 32 KiB of tool descriptions and schemas per plugin. Every limit on this page is a starting value to measure.

**Return shape.** Canonical ABI lifting allocates on the host before any contract check runs, and guest memory size does not bound it: a list of string descriptors that all point at one string lifts to many host copies, and nested lists multiply that. Post-lift checks therefore cannot prevent an allocation spike. The WIT has no list of strings or records and no nested lists: each export returns a flat variant whose payload is at most one `string`, with JSON carried as one string the host parses to limited depth and size. Lifting one string can still copy up to the guest's memory, so the spike must show host allocation per call stays within a fixed bound for hostile but valid returns. If the generated bindings cannot enforce the bound before allocating, the interface narrows further: `WasmStr` or `WasmList<u8>` through a `TypedFunc`, which read lengths before copying, are candidates to evaluate, as is a pull interface in bounded chunks; failing those, the plugin host moves into a separate process. Until measured, this is an obligation, not a protection.

**Versioning.** Record fields, variant cases, and function signatures participate in component type matching, so adding a field, case, or parameter is not automatically compatible. The manifest pins the exact API version (`api = "0.1.0"`), and the component must import `bake:plugin/host@0.1.0` and export `bake:plugin/plugin@0.1.0` exactly. The host supports a listed set of exact versions, each with its own generated bindings and an explicit adapter onto the one internal contract, as Zed keeps one binding module per version. The two directions differ: for imports, the host keeps linking every supported older host interface, so a new host function is a new version that old guests never see; for exports, the host calls only the functions of the guest's version, so a new required hook changes the exported interface version and the adapter supplies its default for old guests. A version outside the set is refused with a message naming both versions. Each supported version has a compatibility fixture built with that version's SDK, and dropping one is a breaking host release.

## Host calls and concurrency

The first API imports four host calls, which limits plugin tools to computation and metadata and hooks to vetoes and transformations. There is no exec, fetch, filesystem, or tool reentry, so plugins cannot automate the host. Adding one is a later capability design in which the D24 engine evaluates each call as the matching built-in tool, with its approval prompt.

| Import | Bound |
|---|---|
| `log(level, text)` | 4 KiB a line, 64 KiB a call, and 1 MiB a session per plugin, to Bake's diagnostics; past the total, lines are dropped and counted |
| `append-entry(type, json)` | A Pi `custom` entry `bake.plugin.<id>.<type>`, never in model context; at most 64 KiB and 16 per call, and 1 MiB and 256 entries a session per plugin |
| `state-get`, `state-set` | Host-owned private key-value state for the plugin: 1 MiB stored, 256 keys, and 4 MiB written a session; the guest never sees a path |
| `session-info()` | A read-only copy of the model id, preset name, and session id |

Each guest call carries an owner (the plugin), a generation (incremented by every load or restart), and a cancellation scope (the call's token and deadline). A host call whose generation is stale, whose gate is closed, or whose token is cancelled returns an error and has no effect. Six rules prevent deadlock and late effects:

- No session or registry lock is held while guest code runs: the dispatcher copies the views and `session-info` a call needs before entering the guest.
- `append-entry` buffers into the call, and the dispatcher commits the buffer after the guest returns. A host call never waits on the session actor, which may itself be awaiting this guest.
- `state-set` writes into the call's overlay, which `state-get` reads first. No session, registry, or state lock is held across an `.await`: the host takes the state mutex only to copy or swap in-memory values. Exclusive ownership of an instance still spans its guest call.
- No host call enters any guest, and calls to an instance are serialized by its mutex, so an instance is never re-entered.
- The dispatcher commits a call's entries and state overlay only if, at commit time, the generation is current, the gate is open, and the token is not cancelled. The state file write is atomic, owned, and awaited. Cancellation cannot undo an effect: an entry or state write committed before a cancellation stays, and `plugin_inspect` reports it.
- On Esc, a timeout, or shutdown, the host drops the guest future, cancels and awaits its host futures, discards the uncommitted buffer and overlay, and fails the instance. The `Store` is dropped only after the plugin's `WorkGate` ([`owned_work.rs`](../../../rust/crates/bake-coding-agent/src/owned_work.rs)) is closed and its permits are released, so no host call runs after the plugin stops.

Fuel and epoch deadlines bound guest instructions only. The host deadline around each call is what bounds a host call the guest awaits.

## Containment and limits

- **WASI context:** no preopens, environment, arguments, or working directory; stdin closed; socket addresses and name lookup denied, keeping the defaults; clocks allowed. Random needs Bake's own metered generators in place of the defaults, counting bytes against 64 KiB a call and 1 MiB a session per plugin; the builder's per-call length cap alone sets no total. Stdout and stderr go to bounded 64 KiB ring buffers that `plugin_inspect` reports, with terminal control sequences escaped before display. There is no terminal access.
- **Imports:** before compiling, a validating parse of the component lists its imports. Anything outside `wasi:{cli,io,clocks,random,filesystem,sockets}@0.2.x`, all linked to the empty context, and the exact supported `bake:plugin/host` versions is refused.
- **Before compile:** a manifest of at most 64 KiB; an id matching `[a-z0-9-]{1,32}`; at most 32 tools and 16 commands; a description of at most 2 KiB; each JSON Schema at most 16 KiB and 16 levels deep, with no remote `$ref`; a component of at most 32 MiB.
- **At run time:** a WASI component may contain several core modules and instances (the adapter is one), so the limits are aggregate per `Store`. Tentatively: 32 instances, 8 memories, 16 tables, 64 MiB of linear memory summed over all memories, and 10,000 table elements summed over all tables, with growth failures trapping. `StoreLimits` supplies the counts, but its memory and element caps are per memory and per table, so the sums need Bake's own `ResourceLimiter` that adds each growth request to a running total. The final values come from counting what the guest SDK's components actually instantiate. Each call also has fuel, an epoch deadline, and a host deadline over the whole call.
- **After lifting:** returns are checked against the contract bounds, with JSON parsed to limited depth and size. This rejects bad values; it cannot bound the allocation that lifting already made ([return shape](#contribution-contracts)).
- **Faults:** a trap, an exceeded limit, a missed deadline, or an invalid return fails the instance. It restarts at most three times a session, then stays failed until restarted by hand; a failed veto still blocks its listed tools.
- **Compilation:** fuel does not bound compiling. It runs on one owned blocking thread at a time, behind a bounded queue, and shutdown awaits it. A deadline only discards a late result; it cannot stop the thread, so it does not make teardown bounded. The spike measures compile time and memory for the worst input the caps permit; before shipping, either those bounds are small enough or compilation and execution move together into a qualified, killable Wasm-host helper. Such a helper still accepts only components and exposes the same host-call contract; it is not a native plugin adapter and transfers no serialized native artifact back to the parent. Compiled code is cached in memory only, keyed by component hash and engine configuration. 0.4 has no persistent compiled cache and never deserializes a native artifact; a measured compile cost does not justify `unsafe` deserialization.

## Inspection and management

D7 asks for useful native inspection and management. This design proposes these names and shapes:

| Need | Model tool (optional) | CLI and terminal |
|---|---|---|
| What the runtime provides | `plugin_inspect {action: "runtime"}`: active tools with their owner (built-in or plugin), and hooks in dispatch order | `bake plugin list`, `/plugins` |
| A plugin's state | `plugin_inspect {action: "plugin", id}`: state, versions, hash, contributions, limits used, captured output, last error, restarts | `bake plugin inspect <id>` |
| The API | `plugin_inspect {action: "api"}`: the WIT text and schemas of supported versions, generated from the bindings | `bake plugin api` |
| Changes | `plugin_manage {action: enable \| disable \| restart \| install \| remove, id, path?}` | `bake plugin enable`, `disable`, `restart`, `install <dir>`, `remove`, `doctor` |

Neither tool is in a default set; the existing `--tools` selection enables them, and no preset is added. `plugin_inspect` only reads. Every `plugin_manage` call requests approval through the D24 engine unless the session is in `danger-full-access`, the rule the 0.3 `plugin_manager` applies to every action ([`tools.ts`](../../../packages/boot/plugin-manager/src/tools.ts), `approveEscalation` with `requestedMode: 'danger-full-access'`). There is no unconfined override. Exposing either tool to the model needs a paired eval record.

Installation runs no script. It opens `plugin.toml` and `plugin.wasm` without following symlinks, refuses anything but a regular file, reads each once within its byte cap, and runs the pre-compile checks on those bytes in memory. It then writes them to `$BAKE_HOME/plugins/store/<sha256>/` through a temporary directory and an atomic rename, never to a path the plugin or user names, and the copy is never modified. Enabling records the id and that content hash; loading reads only the hashed copy and rechecks its hash, so the source directory changing after validation affects nothing. The state root, `$BAKE_HOME/plugins/state/<id>/`, is derived from the validated id, never from user input.

## Named consumer and tests

`rust/plugins/guard`, a Wasm component built with the guest SDK:

- **Veto:** a port of Pi's protected-paths example, on `write` and `edit`. It is a convenience, not a confidentiality boundary: `bash` and `read` still reach those paths. D24 remains the enforcement floor, and a plugin veto can only add a denial.
- **Metadata tool and command:** `guard_status` and `/guard` report its configured paths and counters.
- **Counters:** block counts per turn in plugin state, with a `turn-end` summary entry.
- **Result patch:** collapses runs of blank lines in `bash` results.

It is not a secret redactor: a fail-open result hook cannot promise redaction. It is off by default. Tests use the real session, D24 engine, and session file:

1. **Lifecycle:** load, disable, restart, failure, and shutdown with a pending entry buffer; a failed or exhausted veto plugin blocks its listed tools until disabled, including after a process restart.
2. **Mismatches and caps:** API version or hash mismatches, an unknown import, and each manifest and schema cap, all refused before compiling.
3. **Limits:** a guest loop, memory and table growth, oversized returns, and output floods.
4. **Cancellation:** Esc and session disposal mid-hook leave no host call after stop and no uncommitted entry.
5. **Serialization:** a hook and a guest tool on one instance serialize without deadlock, and a plugin's own tool skips its own veto.
6. **Ordering:** first block wins; a failure blocks a call; patches chain; a context contribution that targets a system, assistant, or tool-result omission is refused.
7. **Crash reconstruction:** kill the process after a decision entry, after a context contribution, and mid guest tool. Each reopened request matches the request rebuilt without plugins, and an unanswered call gets Pi's synthetic result.

Separate hostile fixtures attempt ambient access (read `$BAKE_HOME` credentials, create a file, connect to a listening test port, read the environment or stdin, start a process) and abuse granted host calls (state, entry, log, and random floods within and across calls, aliased-string returns, oversized arguments, stale-generation calls, forged context edits, and a source directory swapped or symlinked during install).

## Implementation slices

Plugins stay disabled by default, so each slice reverts alone, and the model surface is unchanged until slice 6.

1. **Spike** (`rust/Cargo.toml`, `bake-coding-agent`): add Wasmtime with `component-model`, `async`, and `cranelift` and the `wasm32-wasip2` target. Measure binary size, clean build time, compile time and memory for the worst permitted component, host allocation for hostile valid returns, the guest SDK's core instance, memory, and table counts, and per-hook latency, against Pulley as the no-JIT option. Rollback: drop the dependency.
2. **Dispatcher** (`bake-agent/src/hook.rs`, `types.rs`; `bake-coding-agent/src/agent_session.rs`): the built-in trait, ordered composition into `BeforeToolCall`, `AfterToolCall`, and `TransformContext`, and the input path, with order tests ported from Pi's runner.
3. **Persistence** (`agent_session.rs`, `session/`): decision, `context_edit`, `custom_message`, and input entries, with the crash-reconstruction tests.
4. **Wasm host** (`bake-coding-agent/src/plugins/`): the engine, epoch ticker, limits, empty WASI context, import check, per-instance mutex, `WorkGate`, generations, and lifecycle under `owned_work.rs`.
5. **Guest SDK and consumer** (`rust/crates/bake-plugin-sdk`, guest-only, the one new crate; `rust/plugins/guard`; fixtures under `bake-coding-agent/tests/`): the consumer and hostile suites.
6. **Management** (`tools/`, `cli/`, `bake-tui` with `bake-tui-view/src/copy.rs`): `plugin_inspect`, `plugin_manage`, `bake plugin`, and `/plugins`, with a paired eval record.
7. **Documentation:** the `rust/README.md` rows, the roadmap Dev Note, the migration inventory, `doc-sync`, and the A19 and A20 evidence.

## Implementation choices and qualification gates

This design makes these choices; review may change them without blocking the slices:

- **Exclusions:** 0.4.0 has no native-process or `dylib` plugins and no plugin imports for exec, fetch, or the filesystem. An accepted design records them in the register.
- **Surface:** `plugin_inspect` and `plugin_manage` are optional tools selected through the existing `--tools`, with the `bake plugin` CLI and `/plugins`; no preset exposes them and no new permission gate is added.

The plugin slice of scope 12, rows A19 and A20, qualifies only when slices 1 to 6 pass on Linux and Windows CI and on Intel macOS through `preflight:mac`, with the A19 and A20 evidence. Apple Silicon is checked only by the release builds. These gates must pass first, each by measurement:

- Host allocation per call stays within a fixed bound for hostile valid returns, or the interface is narrowed or isolated as [return shape](#contribution-contracts) describes.
- The aggregate limiter enforces the memory, instance, and table totals, sized from measured guest SDK components.
- Compile time and memory for the worst permitted input are acceptable, or compilation is isolated.
- The quotas hold against hostile fixtures across many calls and a whole session.
- A paired eval record exists for any model-visible tool.

MCP, web, code mode, and hook processes remain separate scope 12 work and are not qualified by this slice.

## Dev Note

This page owns the proposed plugin architecture and its Pi v1.3.0 provenance (`71023aec1546d6d677a65669c5665c0927ce0dd0`: the extension runner, types, and session, the protected-paths example, and the Chord spec). The [support register](scope-00/support.md#decision-register) owns D6 and D7; an accepted design moves its decisions there. Adopted Pi code keeps its MIT notice.
