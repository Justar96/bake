# Native plugin design for the Rust harness

## Summary

This page proposes the native plugin API that [D6](scope-00/support.md#decision-register) requires for 0.4.0, and the inspection and management capabilities that replace the retired Cordis tooling under [D7](scope-00/support.md#decision-register). Trusted code compiled into `bake-rs` implements a Rust trait. Third-party plugins are Rust crates compiled to `wasm32-wasip2` and run as WASI 0.2 components in Wasmtime inside a trusted helper owned by Bake. The guest has no ambient authority: its only effects are a few bounded host calls. Both kinds go through one ordered dispatcher whose model-visible effects are written to the session file in Pi's existing entry types. OS tools and MCP servers stay separate processes under [D24](scope-00/support.md#decision-register); they are not a way for a plugin to run native code. 0.4.0 has no native-process or dynamic-library plugin adapter.

**Status: proposed design, 2026-10-17.** No Bake plugin runtime is implemented or qualified. An [isolated Linux experiment](#exploratory-linux-results) has exercised the proposed component boundary; it does not qualify Bake's lifecycle or supported platforms. This is a proposal under D6 and D7 for review, not an accepted decision; the names and technical exclusions are this design's own choices, which review may change. Facts in [Sources](#sources) come from the linked primary sources or local code. Apart from the measured observations in [Exploratory Linux results](#exploratory-linux-results), the rest is proposed behavior. Each protection is an obligation the [qualification gates](#implementation-choices-and-qualification-gates) must prove.

## Table of Contents

- [Sources](#sources)
- [Architecture](#architecture)
- [Contribution contracts](#contribution-contracts)
- [Host calls and concurrency](#host-calls-and-concurrency)
- [Containment and limits](#containment-and-limits)
- [Exploratory Linux results](#exploratory-linux-results)
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
| Wasmtime [v49.0.3](https://github.com/bytecodealliance/wasmtime/releases/tag/v49.0.3) | Published 2026-10-14 | Latest release when checked. Its `Cargo.toml` declares `rust-version` 1.96.0, below Bake's pinned 1.99.0. The isolated Linux experiment built a host and Rust `wasm32-wasip2` guest with 1.99.0; other hosts and the production integration remain untested |
| Wasmtime [`Store::set_hostcall_fuel`](https://docs.rs/wasmtime/49.0.3/wasmtime/struct.Store.html#method.set_hostcall_fuel) | v49.0.3, checked 2026-10-17 | Limits guest-to-host copying before allocation. Its budget resets for each transfer; retained host data still needs separate quotas. The experiment also checks exported return values |
| Component model [WIT](https://component-model.bytecodealliance.org/design/wit.html) | Read 2026-10-16 | Versioned packages, typed records, variants, and resources. Record field shapes participate in type matching, so adding a field changes the type |
| Nushell [plugin protocol](https://www.nushell.sh/contributor-book/plugin_protocol_reference.html), [Extism](https://extism.org/docs/overview) | Read 2026-10-16 | Comparisons only: a native-process protocol, and a bytes-in, bytes-out layer over Wasmtime. Neither becomes a dependency |
| Rust reference, [ABI](https://doc.rust-lang.org/reference/abi.html) | Read 2026-10-16 | The Rust ABI is unspecified across compilers, so a `dylib` plugin would be tied to Bake's exact toolchain |

The Bake home is resolved by `resolve_bake_home` in [`home.rs`](../../../rust/crates/bake-coding-agent/src/home.rs): `BAKE_HOME`, then `DSH_HOME`, then `~/.bake`; this page writes it `$BAKE_HOME`. `bake-sandbox` confines file writes by its confined tree; reads, network, and process visibility stay outside its vocabulary ([`rust/crates/bake-sandbox/src/lib.rs`](../../../rust/crates/bake-sandbox/src/lib.rs)). A native plugin process could therefore read the home's credentials and open sockets, which is why this design excludes one.

## Architecture

```text
 compiled-in Rust ─┐                        ┌─► Pi session file
 (Plugin trait)    ├─► dispatcher ─► agent ─┤   (custom, custom_message,
 Wasm helper     ──┘   (ordered,     loop   │    context_edit, toolResult)
 (component/WIT)       snapshot)           └─► tools ─► D24 engine ─► sandbox
        │
        └─ host calls only: log · append-entry · state · session-info
```

| Class | Mechanism | Trust |
|---|---|---|
| Built-in | A Rust trait in `bake-coding-agent`, registered at compile time | Bake's own code |
| Third-party | The WIT package `bake:plugin@0.1.0`, whose world imports `host` and exports the `plugin` interface, on WASI 0.2, guest target `wasm32-wasip2`, one trusted helper process per plugin, with one Wasmtime `Store` and component instance (which may contain several core instances) | None ambient; only the host calls below |
| OS tools, MCP | Unchanged separate processes, scopes 04 and 12 | D24 confinement and approvals |
| Native process or `dylib` | Not in 0.4.0 | No qualified read, network, and process sandbox exists |

Guest exports are synchronous WIT functions driven by Wasmtime's host-side async calls; WASI 0.3 and async guest exports are outside this design. Both classes share the dispatcher contract. A third-party plugin supplies a component, never a native executable. Bake starts its own helper through an internal entry point, with a scrubbed environment, an empty working directory, and owned pipes; only the helper compiles and executes the component.

The helper protocol is versioned separately from WIT. A handshake checks the protocol and Bake build. Each frame's kind and size are checked before allocation, decoding rejects malformed or trailing bytes, and one request is in flight at a time. Requests and replies carry an id and generation. The parent sends bounded session and state snapshots; the helper returns bounded staged effects. No compiled native artifact crosses the pipe.

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

**Return shape.** Canonical ABI lifting can allocate much more than guest memory: many string descriptors can point at the same guest bytes. Post-lift checks arrive too late to prevent that allocation. Configure `Store::set_hostcall_fuel` before entering the guest, with a copy budget sized to the largest legal argument or return plus its structural overhead. This is separate from CPU fuel. The experiment demonstrates rejection before large allocations for string and aliased-list export returns in Wasmtime 49.0.3; regression tests must preserve that behavior when upgrading. Per-call and per-session quotas still bound data the host retains across imports.

Keep the WIT shapes small: flat variants with at most one string payload, carrying size- and depth-limited JSON where needed. Use owned values through generated bindings. Borrowed `WasmStr` or `WasmList` views are not a fallback: the experiment observed guest post-return freeing the allocation before a view was consumed, and another guest call changed its contents. A guest-memory view must never become stored host state.

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

- No session or registry lock is held while guest code runs: the dispatcher copies the views, plugin state, and `session-info` into the request. The helper serves imports from that snapshot and the call's overlay; it never waits for the parent during a guest call.
- `append-entry` buffers into the call, and the dispatcher commits the buffer after the guest returns. A host call never waits on the session actor, which may itself be awaiting this guest.
- `state-set` writes into the call's overlay, which `state-get` reads first. No session, registry, or state lock is held across an `.await`: the host takes the state mutex only to copy or swap in-memory values. Exclusive ownership of an instance still spans its guest call.
- No host call enters any guest, and calls to an instance are serialized by its mutex, so an instance is never re-entered.
- The dispatcher commits a call's entries and state overlay only if, at commit time, the generation is current, the gate is open, and the token is not cancelled. The state file write is atomic, owned, and awaited. Cancellation cannot undo an effect: an entry or state write committed before a cancellation stays, and `plugin_inspect` reports it.
- On Esc or timeout, the parent rejects new calls to that generation, kills the helper before closing its control pipe, awaits process exit and pipe drains, discards uncommitted effects, and fails the instance. Shutdown first closes the plugin's `WorkGate` ([`owned_work.rs`](../../../rust/crates/bake-coding-agent/src/owned_work.rs)); it awaits all owned work, including a helper still starting. Restart creates a new helper and generation. Dropping an async guest future inside the helper drops its awaited host future in the tested bindings; host imports must own their work and must not spawn detached tasks.

Fuel and epoch deadlines bound guest instructions only. Every store must also configure async fuel or epoch yields so guest code cannot monopolize the executor. The parent's deadline covers the whole helper operation, including an awaited host call or compilation.

A control-pipe EOF watchdog handles parent death even while the main helper thread compiles. Its exit decision and any fatal reply share one gate, so a closed pipe cannot race a different reported exit. Linux parent-death signals depend on the spawning thread's lifetime; they are only an additional safeguard with an owned thread, never the sole mechanism. Windows also requires a kill-on-close Job. Each platform must prove parent-death cleanup and rejection of late effects. These are proposed product obligations, not guarantees of the private Linux helper.

## Containment and limits

- **WASI context:** no preopens, environment, arguments, or working directory; stdin closed; socket addresses and name lookup denied, keeping the defaults; clocks allowed. Random needs Bake's own metered generators in place of the defaults, counting bytes against 64 KiB a call and 1 MiB a session per plugin; the builder's per-call length cap alone sets no total. Stdout and stderr go to bounded 64 KiB ring buffers that `plugin_inspect` reports, with terminal control sequences escaped before display. There is no terminal access.
- **Imports:** before compiling, a validating parse of the component lists its imports. Anything outside `wasi:{cli,io,clocks,random,filesystem,sockets}@0.2.x`, all linked to the empty context, and the exact supported `bake:plugin/host` versions is refused.
- **Before compile:** a manifest of at most 64 KiB; an id matching `[a-z0-9-]{1,32}`; at most 32 tools and 16 commands; a description of at most 2 KiB; each JSON Schema at most 16 KiB and 16 levels deep, with no remote `$ref`; a component of at most 32 MiB.
- **At run time:** a WASI component may contain several core modules and instances (the adapter is one), so the limits are aggregate per `Store`. Tentatively: 32 instances, 8 memories, 16 tables, 64 MiB of linear memory summed over all memories, and 10,000 table elements summed over all tables, with rejected growth returning a trap from the custom limiter; returning `Ok(false)` would instead let the guest observe an ordinary growth failure. `StoreLimits` supplies the counts, fixed when the limiter is installed, but its memory and element caps are per memory and per table, so the sums need Bake's own `ResourceLimiter` that adds each growth request to a running total. The experiment's guest requires 3 instances, 1 memory, and 2 tables; the final values must cover every qualified SDK fixture. Each call also has fuel, an epoch deadline, and a host deadline over the whole call.
- **After lifting:** returns are checked against the contract bounds, with JSON parsed to limited depth and size. This rejects bad values; it cannot bound the allocation that lifting already made ([return shape](#contribution-contracts)).
- **Faults:** a trap, an exceeded limit, a missed deadline, or an invalid return fails the instance. A restart creates a fresh `Store` and component instance, since a trapped instance cannot be re-entered. It restarts at most three times a session, then stays failed until restarted by hand; a failed veto still blocks its listed tools.
- **Compilation:** fuel does not bound compiling, and a timeout cannot cancel an in-process compiler thread. Compilation and execution therefore share the helper. The parent admits one compile at a time behind a bounded queue and kills and reaps a helper that misses its deadline. A killable process still needs a qualified platform memory budget: guest limits do not bound compiler, JIT, WASI resource-table, or total process allocations. Compiled code is cached only in the helper's memory, keyed by component hash and engine configuration; killing it loses that cache. 0.4 has no persistent compiled cache and never deserializes a native artifact.

## Exploratory Linux results

A private experiment on 2026-10-17 built a Rust guest with `wit-bindgen` 0.63.0 and Wasmtime/WASI 49.0.3 on Rust 1.99.0. It used a shared Intel i9-14900KS Linux x86_64 host. The synchronous component call returned a typed veto and invoked a host log import. A second orchestrator run rebuilt the locked host and checked 13 assertions against its output. Neither run entered Bake's agent runtime.

| Observation | What it establishes |
|---|---|
| No preopens: absolute and relative sentinel reads failed; an owned loopback TCP listener accepted no guest connection; guest environment and arguments were empty | Those tested ambient-access paths were denied by the empty WASI context |
| A 4 MiB stderr flood filled a 64 KiB pipe and returned a write error | Bounded captured output; the proposed ring buffer still needs implementation |
| CPU fuel and epoch deadlines stopped infinite guest loops; later calls to the trapped instance failed | Guest-code interruption and the need to instantiate again; no evidence about awaiting host work |
| A custom 64 MiB aggregate limiter admitted only one of two requested 40 MiB memory growths | Aggregate linear-memory enforcement for those fixtures, not a total process-memory bound |
| A 2.5 MiB guest returned 100 aliases to a 1 MiB string, causing 100 MiB of host allocation under defaults; a 1 MiB copy budget rejected it before that allocation | The amplification is real and `set_hostcall_fuel` is a useful pre-allocation control |
| A 143 KB component compiled in roughly 123 ms; a 28 MB synthetic arithmetic component took roughly 7.8 s and 370 MiB of Rust heap | Compilation needs a separate availability boundary or tighter qualified bounds; these samples do not establish a worst case |

The stripped experiment host added about 17 MB over its empty control binary. That is not a forecast for Bake, whose existing dependencies overlap. Measurements count Rust heap, excluding mapped linear memory, JIT code, and allocator overhead; they are exploratory observations, not performance budgets.

The local harness, locks, and logs remain under `.preflight/rust-orchestration/research/plugin-spike/`; `plugin-spike-orchestrator-checks.json` records their hashes. They are private experiment artifacts, not reproducible qualification evidence in this checkout. The implementation must bring the relevant hostile fixtures into Cargo tests. That first experiment did not cover async host-call cancellation, owned teardown, hostile table growth, or compiler cancellation. Resource-handle limits, metered random, Windows, macOS, and arm64 remain unproven. A follow-up experiment exercised async bindings and a trusted Wasm-host helper. Its 18 tests passed again in an independent orchestrator run:

| Observation | Limit of the evidence |
|---|---|
| Dropping a guest future during a host import dropped the import's owned socket and prevented its later effect; a fresh store completed the positive control | The fixture spawned no detached work; the production ownership boundary remains to be tested |
| A host deadline ended the suspended call; configured fuel and epoch yields let another task run during a spinning guest | Fuel alone neither ends an awaited host call nor keeps an executor responsive |
| Two requested 6,000-element table growths under a 10,000-element aggregate budget admitted one; a 2 MiB import string was refused before the host import ran under a 1 MiB copy budget | These fixtures cover table totals and import copying, not total host allocations |
| A roughly 2-second compile continued after an in-process 400 ms timeout; the helper was killed and reaped about 1 ms after the same deadline | The compile-start marker, CPU ticks and running state are circumstantial evidence of interruption during compilation, not a compiler stack sample or a universal timing bound |
| Control-pipe EOF ended the helper after parent death; a Linux parent-death signal also killed a helper when only its spawning thread ended | A portable watchdog needs platform tests; a Tokio worker's lifetime cannot stand in for the parent process's lifetime |
| Concurrent tests found and corrected fatal-exit and kill-order races; 40 subsequent concurrent suite runs passed | Stress evidence supplements the explicit protocol and ownership tests |

The small guest's helper round trip took roughly 6–8 microseconds, startup about 1 ms, and load plus first call about 127 ms on that host. Its RSS high-water mark was about 22 MiB, excluding larger compilation peaks. Those observations support the helper as the proposed lifecycle boundary; they establish no production performance or memory budget.

The second harness and logs are under `.preflight/rust-orchestration/research/plugin-async/`. It still lacks a total memory cap, a pre-compilation import check, bounded stderr capture, and macOS or Windows evidence. Its process runs as the user: Wasmtime's empty WASI context confines guest authority, while the helper separates memory and lifetime. Neither process groups nor Jobs provide filesystem or network confinement.

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

Plugins stay disabled by default. Each contribution slice includes its persistence and failure tests; any enabled path that changes model input needs a paired eval record.

1. **Qualification harness** (`rust/Cargo.toml`, `bake-coding-agent`): bring the relevant private experiment fixtures into Cargo tests; add Wasmtime with `component-model`, `async`, and `cranelift` and the `wasm32-wasip2` target. Measure binary size, clean build time, compile time and memory for the worst permitted component, host allocation for hostile valid returns, the guest SDK's core instance, memory, and table counts, and per-hook latency, against Pulley as the no-JIT option. Rollback: drop the dependency.
2. **Dispatcher** (`bake-agent/src/hook.rs`, `types.rs`; `bake-coding-agent/src/agent_session.rs`): the built-in trait, ordered composition into `BeforeToolCall`, `AfterToolCall`, and `TransformContext`, and the input path, with order tests ported from Pi's runner.
3. **Persistence** (`agent_session.rs`, `session/`): decision, `context_edit`, `custom_message`, and input entries, with the crash-reconstruction tests.
4. **Wasm helper** (`bake-coding-agent/src/plugins/`, `bake-cli`): the bounded protocol, process owner and parent-death cleanup, engine, cooperative yields, limits, empty WASI context, pre-compilation import check, memory budget, `WorkGate`, generations, and lifecycle under `owned_work.rs`.
5. **Guest SDK and consumer** (`rust/crates/bake-plugin-sdk`, guest-only, the one new crate; `rust/plugins/guard`; fixtures under `bake-coding-agent/tests/`): the consumer and hostile suites.
6. **Management** (`tools/`, `cli/`, `bake-tui` with `bake-tui-view/src/copy.rs`): `plugin_inspect`, `plugin_manage`, `bake plugin`, and `/plugins`, with a paired eval record.
7. **Documentation:** the `rust/README.md` rows, the roadmap Dev Note, the migration inventory, `doc-sync`, and the A19 and A20 evidence.

## Implementation choices and qualification gates

This design makes these choices; review may change them without blocking the slices:

- **Exclusions:** 0.4.0 has no native-process or `dylib` plugins and no plugin imports for exec, fetch, or the filesystem. An accepted design records them in the register.
- **Surface:** `plugin_inspect` and `plugin_manage` are optional tools selected through the existing `--tools`, with the `bake plugin` CLI and `/plugins`; no preset exposes them and no new permission gate is added.

The plugin slice of scope 12, rows A19 and A20, qualifies only when slices 1 to 6 pass on Linux and Windows CI and on Intel macOS through `preflight:mac`, with the A19 and A20 evidence. Apple Silicon is checked only by the release builds. These gates must pass first, each by measurement:

- The configured copy budget rejects hostile valid returns before excessive allocation, and retained host data stays within the per-call and per-session quotas; Wasmtime metadata, resources, and compiler allocations need separate bounds.
- The aggregate limiter enforces the memory, instance, and table totals, sized from measured guest SDK components.
- Compiler deadlines kill and reap the helper, and admitted hostile inputs stay within the qualified platform memory budget. Isolation alone is not a memory limit.
- Async yields, bounded host futures, process startup, cancellation, pipe drains, shutdown and parent death pass owned-lifecycle tests on every supported platform.
- The quotas hold against hostile fixtures across many calls and a whole session.
- A paired eval record exists for any model-visible tool.

MCP, web, code mode, and hook processes remain separate scope 12 work and are not qualified by this slice.

## Dev Note

This page owns the proposed plugin architecture and its Pi v1.3.0 provenance (`71023aec1546d6d677a65669c5665c0927ce0dd0`: the extension runner, types, and session, the protected-paths example, and the Chord spec). The [support register](scope-00/support.md#decision-register) owns D6 and D7; an accepted design moves its decisions there. Adopted Pi code keeps its MIT notice.
