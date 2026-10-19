# Code mode design for the Rust harness

## Summary

This page designs code mode for `bake-rs`: a `codemode` tool whose input is a model-written JavaScript script that calls the session's other tools, so independent calls run in one round trip and large results are filtered before the model sees them. Only the script's output reaches the model.

The model surface is Pi's `codemode` tool, ported under [D22](scope-00/support.md#decision-register). The engine is the same QuickJS-NG WebAssembly binary Pi runs, executed by the Wasmtime the workspace already pins, inside the `bake-rs` process. Bake writes its own WASI shim and its own driver for the binary's C API, so code mode adds no Rust crate and no JavaScript runtime. Nested calls run through the session's ordinary tool pipeline, so [D24](scope-00/support.md#decision-register)'s permissions, approvals, and sandbox apply to every call a script makes.

**Status: slices 1 to 3 implemented 2026-10-19; the tool (slice 4) is not.** `bake-codemode` is the engine and script environment, and `bake_agent::nested` runs nested calls; no `bake-rs` tool uses them yet, so no request changes. [D42](scope-00/support.md#decision-register) records the choices, and supersedes D17. [D43](scope-00/support.md#decision-register) defers the rest of the native plugin API beyond 0.4.0, so code mode is the remaining scope 12 work after HTTP MCP.

## Table of Contents

- [Sources](#sources)
- [Decisions](#decisions)
- [Model surface](#model-surface)
- [Architecture](#architecture)
- [Engine](#engine)
- [Nested tool calls](#nested-tool-calls)
- [Session, durability, and the terminal](#session-durability-and-the-terminal)
- [Containment](#containment)
- [Dependencies and the engine artifact](#dependencies-and-the-engine-artifact)
- [Measurements](#measurements)
- [Tests](#tests)
- [Implementation slices](#implementation-slices)
- [Risks and open questions](#risks-and-open-questions)
- [Dev Note](#dev-note)

## Sources

| Source | Revision | Used for |
|---|---|---|
| Pi `packages/codemode/src/` (`runtime/vm.ts`, `runtime/execution.ts`, `runtime/prelude-source.ts`, `runtime/protocol.ts`, `source.ts`, `declarations.ts`, `identifier.ts`, `types.ts`) and `test/` | v1.3.0, `71023aec1546d6d677a65669c5665c0927ce0dd0`, MIT | The VM driver contract, the script environment (prelude), the source format, declarations, limits, and 102 test cases |
| Pi `packages/coding-agent/src/extensions/codemode/{tool,execute,renderer}.ts`, `core/nested-tool-calls.ts`, `docs/codemode.md`, and their tests | Same revision, MIT | The tool's name, schema, description, loadout presentation, result format, store entries, nested-call records, and the renderer |
| [`quickjs-wasi`](https://github.com/vercel-labs/quickjs-wasi) 3.6.2 (`quickjs.wasm`, `dist/index.js`) | npm `sha512-FCqGtGOrMgzUiIrMNMA2YnsOxCNwo31dzqXvclXUC6xeT35NJLKXQJsvbeCTjvoFAwZgEAPg8U6+KAPDGXn8Mg==`, MIT | QuickJS-NG compiled to a WASI preview 1 reactor; its exported `qjs_*` C API and the semantics of its six `env` imports |
| Bake 0.3 `packages/ptc-runtime/ptc-runtime-codemode/` | `develop` | Lessons only: the metering it had to wrap around pi-codemode's worker, and the limits it chose. Its TypeScript `run_code` surface is not ported (D42) |
| Wasmtime [interrupting execution](https://docs.wasmtime.dev/examples-interrupting-wasm.html), [`StoreLimitsBuilder`](https://docs.rs/wasmtime/49.0.3/wasmtime/struct.StoreLimitsBuilder.html), [`Config::max_wasm_stack`](https://docs.rs/wasmtime/49.0.3/wasmtime/struct.Config.html#method.max_wasm_stack) | v49.0.3, the workspace pin | Epoch interruption, linear-memory limits, and the stack bound |

## Decisions

These choices are recorded as [D42](scope-00/support.md#decision-register).

1. **Pi's tool, not 0.3's.** The tool is Pi's `codemode`: its name, its one `code` parameter, its description, its globals, and its result text. D17 kept 0.3's `run_code` with model-written TypeScript and a typed SDK. D22 makes tools follow Pi, and accepting TypeScript would need a TypeScript parser to strip types, the largest dependency this design would otherwise add. Models write JavaScript as readily as TypeScript.
2. **Opt-in, as in Pi.** `codemode` is registered but inactive. `--tools codemode` or the `defaultTools` setting turns it on. Default requests do not change, so turning it on by default is a later decision, made with an eval record.
3. **The same engine as Pi.** The engine is `quickjs-wasi` 3.6.2's `quickjs.wasm`, the binary Pi v1.3.0 runs. A script therefore sees the same language, built-ins, and error text in Bake as in Pi.
4. **Native host, no new crates.** The workspace's pinned Wasmtime runs the binary as a core module. Bake implements the module's twelve imports itself: six WASI preview 1 functions and six `env` callbacks. No QuickJS binding crate, WASI crate, JavaScript engine, or TypeScript tooling is added.
5. **In-process, one thread per script.** Unlike [third-party plugins](native-plugins.md), the guest is Bake's own pinned module, and only the JavaScript it interprets is untrusted. Each script runs on its own OS thread inside `bake-rs`, behind two boundaries: QuickJS's language boundary and Wasm's memory boundary. No helper process is needed. See [Containment](#containment).
6. **Nested calls are ordinary calls.** Each call a script makes goes through the session's tool pipeline with its own id `<call id>/<n>`. Validation, D24 permissions, approval prompts, sandboxing, hooks, plugin vetoes, the mutation queue, and the work gate all apply to it as they would to a direct call.
7. **Never replayed.** `codemode` has replay class `Never`: a script's nested calls may write, run commands, or leave the machine. Pi's deterministic mode makes replay possible later; see [slice 7](#implementation-slices).
8. **Left out for 0.4.0.**
   - `models.*`: Bake has no classifier or image models; Pi also omits `models` when the host offers none.
   - Grammar-constrained input (`openai_lark`): needs custom tool formats in `bake-ai`.
   - Pi's Cloudflare and inline sandboxes.

## Model surface

The model surface follows Pi v1.3.0 exactly, adapted only where Bake lacks a feature.

- **Input.** `{ "code": string }`, the body of an async function: top-level `await` and `return` work.
  - An optional first line `// @options: {"max_output_tokens": N, "timeout_ms": N}` sets the script's options, parsed as in Pi's `source.ts`.
  - The options line is replaced by an empty line, so stack line numbers match the input.
- **Globals.**
  - `tools.<identifier>(args)`; identifiers come from Pi's `toCodemodeIdentifier`.
  - Output: `text`, `image`, `console.*`, and `return`. `exit()` ends the script successfully.
  - `store` and `load`.
  - Discovery: `ALL_TOOLS`, `searchTools`, `describeTool`, and `describeNamespace`. `describeNamespace` reports MCP servers as namespaces.
  - The VM has nothing else: no timers, I/O, modules, or `WebAssembly`.
- **Results.**
  - A script's result starts with `Script completed` or `Script failed` and the wall time.
  - Output items follow in order: `==> text N/M <==` headers when there are several text items, and one `<console_output>` block.
  - A failed script keeps its partial output, then `Script error:` and the error.
  - Output beyond `max_output_tokens` (default 10000) keeps its start and end. The full text goes to a temp file through Bake's existing output files.
  - Each `image()` is saved to a temp file whose path the result names.
- **Nested results.**
  - A tool with an output schema resolves to its structured content.
  - Any other tool resolves to its text.
  - A failed, refused, or invalid call rejects with an `Error` carrying the tool's error text.
  - A refused approval is such a failure.
- **Description and loadout.** The description and loadout follow Pi's `prepareCodemodeLoadout` and `createCodemodeDescription`, with `codemode.mode` (`on` or `only`) and `codemode.inlineBudget` (3000 estimated tokens) in settings.
  - With `on`, each declared tool's description gains `Codemode: tools.x(args) resolves to …`.
  - With `only`, direct tools are listed in the `codemode` description instead of being declared.
  - Declarations are rendered from each tool's JSON schema by a port of Pi's `declarations.ts`.
- **Reach.**
  - A script may call every session tool except `codemode` itself.
  - That includes `bash`, the file tools, web, MCP, job, subagent, and schedule tools that the session has.
  - A subagent's session gets `codemode` only when its own tool list names it.
- **Limits.**
  - VM heap: 256 MiB.
  - Output: 16 Mi characters and 100000 output calls.
  - Store: 256 Ki characters per value and 1 Mi in total.
  - At most two scripts run at once per process; at capacity a new script fails at once.
  - No deadline unless `timeout_ms` sets one; Esc and session shutdown abort the script.

## Architecture

Code mode has three layers. Each depends only on the layer below it.

```text
bake-coding-agent::tools::codemode      tool definition, loadout, settings, store entries,
                                        result text, admission limit, temp files
            │ host callbacks (tool calls, discovery)
bake-agent::nested                      nested call ids, records, exclusivity, cancellation,
                                        through the session's tool pipeline
            │
bake-codemode (new crate)               Engine, Vm, prelude, protocol, source and declarations;
                                        no agent types, like Pi's pi-codemode package
            │
Wasmtime core module ── quickjs.wasm (vendored, pinned by SHA-256)
```

`bake-codemode`'s public API is small and synchronous at its core:

```rust
pub struct CodemodeEngine { /* compiled Module, compiled once per process */ }
pub struct ExecuteRequest { pub source: Source, pub tools: Vec<ToolDeclaration>,
                            pub globals: Vec<GlobalDeclaration>, pub store: StoreValues,
                            pub limits: Limits, pub deterministic: Option<Deterministic>,
                            pub on_exit: Option<ExitHook> }
pub enum HostCall { Tool { id: u32, name: String, args: Option<Value> },
                    Global { id: u32, name: String, args: Option<Value> } }   // discovery
pub enum Event { Started { fingerprint: String }, Call(HostCall), Output(OutputItem),
                 Idle { seq: u32, issued: u32 }, Done(Arc<Outcome>) }

impl CodemodeEngine {
    pub fn get() -> Result<&'static Self, EngineError>;   // blocks on first use
    pub fn start(&self, request: ExecuteRequest) -> Result<Run, StartError>;
}
impl Run {
    pub async fn next(&mut self) -> Event;               // calls and output, then Done
    pub fn handle(&self) -> RunHandle;                    // cloneable: answer, abort
    pub fn answer(&self, id: u32, result: CallResult) -> Result<(), AnswerError>;
    pub fn abort_with(&self, message: impl Into<String>); // abort() says "Execution aborted"
}
```

`Run` owns its thread. Dropping a `Run` aborts the script and joins the thread, in place outside Tokio and on a blocking task inside it, so no thread outlives its tool call; this follows the [defensive patterns](../../defensive-patterns.md) for owned work. `Outcome` carries the completion value or a typed failure: `Exception`, `Timeout`, `Aborted`, `OutOfMemory`, `OutputLimit`, `NeverSettles`, or `Protocol`. The tool layer turns it into Pi's result text.

## Engine

**Compilation.**
- The binary is embedded with `include_bytes!` and compiled by Cranelift on the first `codemode` call.
- The compiled `Module` is kept for the process in a `OnceLock`.
- The engine has its own `Config`: epoch interruption on, async off, and `max_wasm_stack` set. It never shares the plugin host's engine.
- Precompiling at build time (`Engine::precompile_module` and the unsafe `Module::deserialize`) is not adopted; the measured 0.5 s once per process does not justify `unsafe`.

**Imports.** The module imports twelve functions, all implemented in `bake-codemode`:

| Import | Implementation |
|---|---|
| `wasi_snapshot_preview1.clock_time_get` | The host's realtime and monotonic clocks; virtual time in deterministic mode |
| `wasi_snapshot_preview1.random_get` | OS randomness through `getrandom`, already in the workspace; a seeded generator in deterministic mode |
| `wasi_snapshot_preview1.fd_write` | Discards, and reports the bytes as written; QuickJS's own stdout and stderr are not script output |
| `wasi_snapshot_preview1.fd_close`, `fd_seek`, `fd_fdstat_get` | Return `EBADF`; the guest has no descriptors |
| `env.host_call` | Dispatches the prelude's host functions: tool calls, output items, discovery, and the store |
| `env.host_interrupt` | Returns the abort flag, which QuickJS polls between opcodes |
| `env.host_promise_rejection` | Frees the values; unhandled rejections are not recorded, as with Pi's VM |
| `env.host_module_normalize`, `env.host_module_load` | Refuse; scripts have no modules |
| `env.host_get_timezone_offset` | Returns 0: scripts run in UTC, as Pi's deterministic mode does |

Every pointer and length the guest passes is checked against its linear memory before the host reads, and a bad one fails the run as `Protocol`. Rust never panics on guest input.

**Driver.**
- The driver calls the module's `qjs_*` exports: `qjs_init`, `qjs_eval`, `qjs_new_host_function`, `qjs_execute_pending_job`, and the value, property, and promise functions. It checks every export's signature when the module compiles.
- It ports the subset of `quickjs-wasi`'s `index.js` that Pi's `vm.ts` uses, with its MIT notice.
- The prelude is Pi's `prelude-source.ts`, carried as a JavaScript asset with Pi's notice, and evaluated before the script.
- The driver follows `vm.ts`'s contract:
  - The VM runs only when the host delivers something.
  - It drains promise jobs in bounded batches.
  - It reports `NeverSettles` when no call is pending and the script's promise is still pending after a drain.
  - It never decides on its own that it is finished.

**Interruption.**
- Three mechanisms stop a script:
  - A deadline or abort increments the engine's epoch. Each store's epoch deadline turns that into a trap at the next loop back-edge or function entry of the compiled module.
  - `host_interrupt` answers QuickJS's own interrupt poll, which ends the script with an error it cannot catch.
  - Between job batches, the driver checks the abort flag itself, as Pi does for `while (true) await null`.
- Built-ins are compiled Wasm too, so a long regex or `indexOf` stops at the deadline. Pi's Bun workers cannot stop such built-ins ([Measurements](#measurements)).
- One process-wide ticker thread is not needed. An abort increments the epoch directly; a run with a deadline has a small watchdog thread that stops it at the deadline even when nobody polls `Run::next`, and the script thread joins it before exiting.

**Memory and stack.**
- `qjs_set_memory_limit` caps the QuickJS heap at 256 MiB, so an allocation beyond it is a catchable `InternalError: out of memory`, as in Pi.
- `StoreLimits` caps the module's linear memory slightly above that: 320 MiB with one memory and one table. A growth past it fails, and the run ends as `OutOfMemory` instead of the host running out.
- `qjs_set_max_stack_size` (512 KiB, Pi's `MAX_STACK_SIZE`) stays below `max_wasm_stack` (32 MiB) on a 40 MiB script thread. Measured: 1e5-deep `JSON.parse`, `JSON.stringify`, `String()`, and `eval` trapped the Wasm stack at 4 and 8 MiB and are a catchable `RangeError` from 12 MiB.
- At most `MAX_PENDING_CALLS` (128) calls may wait for results. A script that issues calls without awaiting them never lets the VM take an answer, so the next call fails the run as `Protocol`, telling the script to await calls first, as Bake 0.3's `maxPendingCalls` did. Pi has no such limit.

**Deterministic mode.**
- The shim already owns every source of nondeterminism: clocks, randomness, and the time zone.
- The driver implements Pi's deterministic mode: a virtual clock from `startedAt`, a seeded generator, UTC, no `WeakRef` or `FinalizationRegistry`, and results applied in sequence order.
- It also reports Pi's fingerprint, built from the engine versions, the prelude hash, and the limits.
- 0.4.0 does not use this mode for replay, but porting it now costs little and keeps [slice 7](#implementation-slices) open.

## Nested tool calls

`bake-agent` gains Pi's nested-call executor, from `core/nested-tool-calls.ts`. It is the `ctx.executeTool` that `codemode` calls.

- **Ids.** A nested call's id is `<caller id>/<n>`. Nested tools' own nested calls extend the same id.
- **One pipeline.**
  - A nested call is prepared, executed, and finalized exactly as a direct call: argument validation, `before_tool_call` and `after_tool_call` hooks, and plugin vetoes.
  - It also gets D24 permission evaluation, the approval prompt, the sandbox, the file mutation queue, and the session work gate.
  - An interactive approval prompt names the nested call and its script. In print mode a nested call meets the same preset as a direct one.
- **Ordering.** When the agent executes tool calls sequentially, nested calls run exclusively, through Pi's exclusive queue: a call holding the queue does not wait on its own nested calls. Otherwise a script's `Promise.all` runs its calls concurrently, subject to the mutation queue.
- **Cancellation.**
  - Aborting the script cancels its running nested calls and awaits them before the tool result is written.
  - Calls still running when the script ends are cancelled, as Pi's description promises.
- **Records.**
  - The tool result message carries Pi's `nestedCalls`: arguments, status, and duration, bounded as in Pi. `bake-ai::NestedToolCalls` already has the type.
  - Nested results never enter model history.
  - Compaction serializes nested calls as Pi's `compaction/utils.ts` does.

## Session, durability, and the terminal

- **Store.**
  - A successful script that called `store` appends one Pi `custom` entry, type `codemode-store`, holding `{ set, delete }`.
  - `load` reads the values written on the current branch path. Resume, `/tree`, `/fork`, and `/clone` therefore see each branch's own values without a separate file.
  - Failed scripts write nothing.
- **Reconstruction.** Everything model-visible is in the log: the script is the tool call's input, and its output is the tool result. The store comes from the branch's `codemode-store` entries.
- **Recovery.**
  - `codemode` is `ToolReplay::Never`.
  - A crash during a script leaves the run journal's interrupted-call notice, as for `bash`.
  - Its nested calls are each journaled by their own ids, so the recovery notice can say which nested calls had started.
- **Terminal.**
  - `bake-tui-view` renders a `codemode` call as Pi's renderer does: the script, then one line per nested call with its status and duration, updated live from the nested call events.
  - Text lives in `copy.rs`.
  - The fullscreen and inline transcripts replay the same rows from the log.

## Containment

| Threat | Boundary | Evidence required |
|---|---|---|
| Script reads files, opens sockets, starts processes | The module has no WASI descriptors, sockets, or process imports; every effect is a nested tool call under D24 | Tests that `fetch`, `require`, `import`, `process`, timers, and `WebAssembly` are absent; a nested `bash` under `read-only` is refused |
| Script escapes QuickJS | Wasm linear memory; the host checks every pointer the guest passes | Bounds tests on every `env` import with forged pointers and lengths |
| Infinite loops, long built-ins, job floods | Epoch trap, `host_interrupt`, and the driver's batch checks | Loop, regex-backtracking, and `while (true) await null` cases end within the deadline and after an abort |
| Memory exhaustion | The QuickJS heap limit, then `StoreLimits` on linear memory | An allocation loop ends as `out of memory`; a growth past the store limit ends the run while the process keeps working |
| Deep recursion and nesting | QuickJS stack guard below a 32 MiB `max_wasm_stack`, on a 40 MiB thread | Recursion and 1e5-deep JSON fail as a catchable `RangeError` |
| Output and store floods | The prelude's limits, checked again by the host before it allocates | Pi's limit cases, ported |
| A never-settling script | Detection after each drain | Pi's never-settles case, ported |
| A script that outlives its call | `Run` owns and joins its thread; aborted nested calls are awaited | A teardown test asserts no thread or nested call survives the tool result |
| Too many scripts | Two per process, released when the thread exits | Pi's admission cases, ported |

A Wasmtime or QuickJS memory-safety bug could break the Wasm boundary. The same is true of Pi's worker thread. The remaining exposure is the user's own process, which nested tools already act within. If qualification requires more, the run can move into the existing [Wasm helper process](native-plugins.md) behind the same `Run` API.

## Dependencies and the engine artifact

- **Crates.** Code mode adds no crate.
  - `bake-codemode` depends on the workspace's `wasmtime`, `serde`, `serde_json`, and `getrandom`.
  - The WASI shim, the QuickJS driver, BM25 ranking for `searchTools` (Pi's tool-search ranker), schema-to-declaration rendering, and the source parser are Bake code, ported from Pi with its notice.
- **Artifact.**
  - `rust/crates/bake-codemode/engine/quickjs.wasm` is the 637,405-byte binary from `quickjs-wasi` 3.6.2, SHA-256 `d4c9375f2b1ca4dc95f72c8aa2982a7a9951ac8011490d79c6582df732b4bbd9`.
  - Beside it are `quickjs-wasi`'s and QuickJS-NG's MIT licenses and a provenance file naming the npm tarball's integrity hash.
  - A unit test checks the embedded bytes' hash.
  - The native license bundle lists both licenses.
- **Rebuild from source.** Rebuilding the binary from QuickJS-NG and `quickjs-wasi`'s C glue with a pinned WASI toolchain, and checking the hash, is deferred. Until then the artifact is trusted on its provenance, as Pi trusts it.

## Measurements

Measured 2026-10-19 on Linux x86_64 (Intel Core i9-14900KS), Rust 1.99.0, Wasmtime 49.0.3 with only `runtime`, `std`, and `cranelift`, release build. The experiment was a scratch program kept outside the repository. Its imports were stubs except `host_interrupt`, and it ran each script under a 1.5 s epoch deadline.

| Measurement | Result |
|---|---|
| Cranelift compile of `quickjs.wasm` | 516 ms, once per process |
| Serialized compiled module | 2,419,016 bytes (not used) |
| Instantiate, `_initialize`, and `qjs_init` for one VM | 0.28 to 0.39 ms |
| `1+2` | 0.35 ms in total, no exception |
| `throw new Error('x')` | Exception reported |
| Ten million loop iterations | 537 ms |
| `JSON.stringify` of a three-million-element array | 479 ms |
| `while(true){}` | Trapped at 1.500 s |
| `/(a+)+b/.test('a'.repeat(40))`, backtracking inside a built-in | Trapped at 1.500 s |

The module's imports are exactly the twelve in the [Engine](#engine) table. In slice 1 a debug build compiled it in about 0.64 s, and the tests share one `CodemodeEngine`.

## Tests

- **Pi's cases.** These are ported with the code:
  - `sandbox.test.ts` (52), `deterministic.test.ts` (16), `declarations.test.ts` (17), `source.test.ts` (5), and `worker-lifetime.test.ts` (5) in `bake-codemode`.
  - `nested-tool-calls.test.ts` (6) and `compaction-nested-calls.test.ts` (1) in `bake-agent`.
  - The renderer, admission, and configuration cases in the tool's crates.
  - Cases about Node workers, Bun, or Cloudflare become cases about `Run`'s thread, or are named as not applicable.
- **Bake's cases.**
  - Nested calls under each D24 preset, with an approval answered, refused, and aborted while pending.
  - Hooks and plugin vetoes on nested calls.
  - Pointer forgery on each `env` import.
  - Built-in interruption.
  - `StoreLimits` exhaustion.
  - Abort during a nested `bash`, with the tree reaped.
  - `codemode-store` across `/tree`, `/fork`, and resume.
  - Replay class `Never` in `REPLAY_CLASSES`.
  - A print-mode CLI test with a faux model.
  - One PTY scenario for the rendered rows.
- **Platforms.**
  - Linux and the Intel Mac through `preflight`.
  - Windows and Apple Silicon through a dispatched CI run, as for scopes 03 to 08.
  - Cross-target Clippy for both.

## Implementation slices

| Slice | Scope | Exit |
|---|---|---|
| 1. Engine | `bake-codemode`: the artifact, WASI shim, `env` imports, driver, limits, interruption, `Run` | Evaluation, exceptions, loops, built-ins, memory, and recursion tests; measured debug compile |
| 2. Script environment | Pi's prelude, protocol, source parser, declarations, output items, store, deterministic mode | Pi's `sandbox`, `source`, `declarations`, `deterministic`, and lifetime cases |
| 3. Nested calls | `bake-agent` executor: ids, exclusivity, cancellation, records, compaction | Pi's nested-call cases; D24, hook, and veto cases on nested calls |
| 4. The tool | `tools::codemode`: description, loadout, settings, `--tools codemode`, store entries, results, temp files, admission, replay class | Print-mode tests with a faux model; a live CLIProxyAPI smoke test |
| 5. Terminal | The renderer in `bake-tui-view` and `bake-tui` | Renderer cases and one PTY scenario |
| 6. Qualification | Eval record, ledger record, `rust/README.md`, the roadmap, `bake-rs docs` | A paired eval with `codemode` on in the candidate arm; Windows and Apple Silicon CI |
| 7. Later | Script replay from the run journal through deterministic mode, and the rebuilt engine artifact | Not part of 0.4.0 |

Slices 1 and 2 touch nothing the model sees. Slice 3 changes no request while no tool calls `ctx.executeTool`. Slice 4 changes requests only when `codemode` is turned on. Slice 6's eval measures that opt-in arm against `develop`'s base, so the round-trip and token effect is recorded before anyone proposes turning it on by default.

## Risks and open questions

- **A frozen engine.** QuickJS-NG security fixes arrive through `quickjs-wasi` releases. Updating means replacing the artifact and its hash, re-running Pi's cases, and re-checking the ABI the driver uses. Pi pins the same version.
- **ABI drift.** The `qjs_*` exports are `quickjs-wasi`'s internal API. The driver checks each export's signature when it compiles the module and refuses a mismatch.
- **Cranelift time in debug tests.** If the shared engine is still too slow for test binaries, the package's test profile can raise Cranelift's optimization level for `bake-codemode`, as the workspace already does for Wasmtime's crates.
- **Only one engine.** A pure-Rust engine (Boa) or a C binding (`rquickjs`) would change the language semantics Pi's prompts and tests assume, add a large dependency, and give up the Wasm boundary. Neither is adopted.

## Dev Note

<details>
<summary>Working context for maintainers — click to expand</summary>

- Re-check Pi's latest release when slice 1 starts. Record any change to `packages/codemode` or `extensions/codemode`, and to the `quickjs-wasi` pin.
- Slices 1 to 3 (2026-10-19) ported Pi v1.3.0, `71023aec1546d6d677a65669c5665c0927ce0dd0`, the latest release when they started. `engine/` carries quickjs-wasi's license, QuickJS-NG v0.17.0's exact LICENSE, and wasi-libc's, musl's, and cloudlibc's notices from wasi-libc `165235bc467d`; the binary has no producers section, so the wasi-libc revision of the build is unknown. The release license bundle copies every `LICENSE-*` file under a Bake crate's immediate subdirectories.
- Slice 3 decisions: tools gain a fifth `ToolContext` parameter rather than a task-local executor, because the codemode VM runs on its own thread; each nested call is a task its caller owns; nesting stops at 8 levels; `AgentTool::nested_callable` lets `codemode` exclude itself. `bake.run.v1` dispatch records gain an optional `parent_tool_call_id`.
- Slice 4 notes: `max_concurrent_read_tools` does not cap read-only nested calls; the terminal does not yet show an approval's `parent_tool_call_id`.
- The measurement program compiled with `cargo +1.99.0 build --release --offline` against the workspace's cached Wasmtime 49.0.3 sources. Slice 1 turns it into the engine's first tests instead of committing it.

</details>
