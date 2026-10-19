# bake-codemode

The native engine of Pi's codemode for Bake's Rust agent, slices 1 and 2 of the [code mode design](../../../docs/roadmap/rust-0.4/code-mode.md). A script is model-written JavaScript, the body of an async function, whose only capability is calling the tools and globals its host declares. The crate runs it in the QuickJS-NG WebAssembly binary Pi v1.3.0 runs, with Wasmtime, inside the calling process. Like Pi's `packages/codemode`, it has no agent types: the host answers every call. Nothing in the agent uses it yet; the `codemode` tool is slice 4.

## Table of Contents

- [API](#api)
- [Engine](#engine)
- [Script environment](#script-environment)
- [Containment and limits](#containment-and-limits)
- [Deterministic mode](#deterministic-mode)
- [Sources and licenses](#sources-and-licenses)
- [Tests](#tests)
- [Differences from Pi and the design](#differences-from-pi-and-the-design)

## API

- `CodemodeEngine::get()` compiles the engine once per process (a `OnceLock`) and returns it; `compile_time()` reports how long Cranelift took.
- `CodemodeEngine::start(ExecuteRequest)` starts a script on its own thread and returns the `Run`, the only handle to it. The request holds the parsed `Source` (whose `timeout_ms` is the run's deadline; none without it), the `ToolDeclaration`s on `tools`, the `GlobalDeclaration`s (`discovery_globals()` gives Pi's `searchTools`, `describeTool`, and `describeNamespace`), the `store` snapshot `load()` reads, `Limits`, optional `Deterministic` mode, and an optional `ExitHook` called when the thread exits. An invalid request (Pi's global name rules, duplicate tool names, and in deterministic mode duplicate script names or a bad start time) is a `StartError` and starts no thread.
- `Run::next().await` returns `Event`s in order: `Call(HostCall::Tool | HostCall::Global)`, `Output(OutputItem::Text | Image)`, in deterministic mode `Started` and `Idle`, then one `Done(Outcome)`, which later calls return again. With a deadline it must be awaited on a Tokio runtime with the time driver.
- `Run::answer(id, Ok(value) | Err(CallError))` resolves or rejects a call; `Run::answer_in_order(seq, now_ms, id, result)` does so in deterministic mode. Both return `AnswerError` for a call that is not waiting and, in order, for a reused result number or a clock value that is not finite; answers after the outcome are ignored. `Run::finished()` says whether the outcome is decided.
- `Run::abort()` ends the script with an error it cannot catch. Dropping a `Run` aborts it and joins its thread: in place outside a Tokio runtime, on a blocking task inside one.
- `Outcome` carries `Ok(Completion { value, store_writes })` or a `Failure`: `Exception(ScriptError)` (Pi's `script` kind), `NeverSettles`, `Timeout`, `Aborted`, `OutOfMemory` (the linear-memory limit), `OutputLimit` (the host's recheck), or `Protocol` (Pi's `sandbox` kind: a broken bridge, a bad guest pointer, or a trap); in deterministic mode also `During`. Output is not repeated in the outcome: the host keeps the `Output` events.
- `Source::parse` is Pi's `// @options:` parser; `render_declarations`, `render_tool_signature`, `render_tool_sample`, and `schema_to_type` render TypeScript from JSON Schema; `to_codemode_identifier` maps tool names; `Bm25Ranker`, `create_tool_search_document`, and `tokenize` rank tools for the host's `searchTools()` answers.

## Engine

- `engine/quickjs.wasm` is `quickjs-wasi` 3.6.2's binary (QuickJS-NG 0.17.0, 637,405 bytes, SHA-256 `d4c9375f…4bbd9`), embedded with `include_bytes!`; `engine/PROVENANCE` records the npm integrity hash. A unit test checks the embedded bytes' SHA-256.
- The engine has its own Wasmtime `Config`: epoch interruption on, a 4 MiB Wasm stack, and no Wasm backtraces. Compiling checks that the module imports exactly the twelve functions below and that every export the driver calls has the signature it expects.
- The twelve imports are Bake's (`src/host.rs`): `clock_time_get` (the host's clocks, or virtual time), `random_get` (`getrandom`, or a seeded mulberry32), `fd_write` (discarded, reported as written), `fd_close`, `fd_seek`, and `fd_fdstat_get` (`EBADF`); `host_call` (the prelude's bridge), `host_interrupt` (the stop flag and deadline), `host_promise_rejection` (never enabled; frees its values), `host_module_normalize` and `host_module_load` (refused with NULL), and `host_get_timezone_offset` (0: scripts run in UTC). Every guest pointer and length is checked against linear memory before the host reads or writes; a bad one ends the run as `Protocol`.
- The driver (`src/vm.rs`, `src/guest.rs`) ports the part of `quickjs-wasi`'s `index.js` that Pi's `vm.ts` uses and follows `vm.ts`'s contract: the VM runs only when the host delivers something, drains promise jobs in batches of 50 with a stop check between batches, asks the prelude after each drain whether the script waits on nothing (`NeverSettles`), and never decides by itself that the script ended: the prelude's `done` unwinds the guest, so nothing queued after it runs. A stop recorded before `done` wins.

## Script environment

The prelude is `assets/prelude.js`, the text of Pi v1.3.0's `prelude-source.ts` with its MIT notice; a unit test pins its FNV-1a hash (`8c6c9242`) to Pi's. It freezes the built-ins and defines `tools` (with Pi's close-match errors), `ALL_TOOLS`, `text`, `image`, `exit`, `console.*`, `store` and `load` (256 Ki characters per value, 1 Mi in total), and the declared globals; `a.b` globals form frozen namespaces. Calls and results cross the bridge as JSON text. Nothing else is in the VM: no timers, I/O, modules, `WebAssembly`, or `models`.

## Containment and limits

- Each run is its own Wasm instance on its own thread with an 8 MiB stack. QuickJS's stack guard (`qjs_set_max_stack_size`, 512 KiB) trips well before the Wasm stack limit, so deep recursion is a catchable `RangeError`.
- QuickJS's heap is limited to 256 MiB by default (`qjs_set_memory_limit`): an allocation beyond it is a catchable `InternalError: out of memory`. A `StoreLimits` limiter caps linear memory at 320 MiB with one memory, table, and instance; growth past it traps and the run ends as `OutOfMemory` while the process keeps working.
- A deadline or `abort()` sets the run's stop flag and increments the engine's epoch; every running store's epoch callback then checks its own flag and deadline, so a long built-in such as one `JSON.stringify` of ten million elements stops at the next loop back-edge or call in compiled code. `host_interrupt` answers QuickJS's own interrupt poll (QuickJS-NG's regex engine polls it too), and the driver checks between job batches and while it waits for answers. `Run::next` holds the deadline timer, so the epoch is bumped even while the script runs a built-in.
- The prelude limits output to 16 Mi characters and 100,000 items; the host rechecks the same limits before copying output out of the guest.

## Deterministic mode

`ExecuteRequest::deterministic` runs Pi's `start()` mode: the realtime clock reads the seed while the context is created (QuickJS seeds `Math.random()` from it) and then virtual time from `started_at_ms`, advanced only by answered results and never backwards; randomness comes from the seed; `WeakRef` and `FinalizationRegistry` are absent; answers are applied in `seq` order, early ones buffered. The run reports `Started` with Pi's fingerprint (`quickjs-wasi 3.6.2, quickjs 0.17.0, prelude 8c6c9242, deterministic 1, stack 524288, heap …, budget none`) and `Idle` after each drain, and the outcome's `During`. Pi's pinned `Math.random()` values for seeds 0 and 1 reproduce exactly.

## Sources and licenses

Ported from Pi v1.3.0 (`71023aec1546d6d677a65669c5665c0927ce0dd0`, MIT): `packages/codemode/src/runtime/{vm,execution,prelude-source,protocol}.ts`, `src/{source,declarations,identifier,types}.ts`, and `packages/coding-agent/src/extensions/tool-search/tool.ts`; and from `quickjs-wasi` 3.6.2 (MIT): `dist/index.js` and `dist/wasi-shim.js`. `NOTICE` keeps both notices. `engine/LICENSE-quickjs-wasi` is the package's license and `engine/LICENSE-QuickJS-NG` the engine's MIT license; the native license bundle reproduces both once `bake-cli` links this crate (`scripts/release/native-licenses.ts` includes each `LICENSE-*` file in a Bake crate's immediate subdirectories). The QuickJS-NG text was written from QuickJS-NG's license, not copied from the package, which does not carry it: check its copyright lines against quickjs-ng's `LICENSE` at v0.17.0 before release.

The crate adds no dependency to the lock: Wasmtime, serde_json, getrandom, and Tokio (`rt`, `sync`, `time`) at the workspace's versions.

## Tests

`cargo test -p bake-codemode --locked` runs 41 unit tests and 83 integration tests (`tests/codemode/`, one binary so all share one compiled engine; a debug build compiles it in about 0.64 s on Linux x86_64, since the workspace builds Cranelift optimized). They port Pi v1.3.0's cases, naming each:

- `test/source.test.ts` (5) and `test/declarations.test.ts` (17) as unit tests of `src/source.rs` and `src/declarations.rs`; the `tokenize` and `Bm25Ranker` cases of `coding-agent/test/tool-search.test.ts` (4) in `src/tool_search.rs`.
- `test/sandbox.test.ts` in `tests/codemode/sandbox.rs`, against a `Run`-based host (`tests/codemode/harness.rs`) that executes calls the way Pi's `execution.ts` does. Pi runs each case on its worker, inline, and remote backends; Bake has one. "supports register and unregister between executions" becomes per-request tool declarations; the seven broken-bridge cases run against the bridge decoder (`src/host.rs`); "embedded sources parse as JavaScript" is the prelude hash test and a run using every prelude global. Not applicable: "rejects execute after close" (a `Run` is one script; there is no sandbox object), the worker path, missing worker file, failing wasm module (the module is embedded and its ABI checked at compile time), and Node `--watch` cases.
- `test/deterministic.test.ts` in `tests/codemode/deterministic.rs`, including the pinned random values. "leaves a call waiting when its answer cannot be serialized" and the non-object rejection properties are ruled out by `serde_json::Value` and `CallError`'s types; the remote transport's reordering case is the in-order delivery case.
- `test/worker-lifetime.test.ts` in `tests/codemode/lifetime.rs`: a refused request starts no thread and calls its exit hook once; abort, timeout, and drop settle with the partial output and the thread exits; ordinary and deterministic runs report their thread's exit; a spinning script and its thread end within a second of an abort, timeout, or drop. Not applicable: worker construction and wasm loading failures and a throwing `onWorkerStarted` hook.

Bake's own cases: the embedded binary's SHA-256 and ABI; forged pointers and lengths on `host_call`, `host_promise_rejection`, and every WASI import, refused without a panic, and forged value pointers that make the guest trap; malformed bridge messages; ten million loop iterations; an infinite loop, regex backtracking, and a long `JSON.stringify` stopped at the deadline (the last is stopped only by the epoch: with the epoch increment removed it completes); a spinning loop, a job flood, and a regex stopped by `abort()`; the heap limit as a catchable error and linear-memory exhaustion ending the run with the engine still usable; deep recursion; and a dropped `Run` joining its thread, outside and inside a runtime.

## Differences from Pi and the design

- Every script runs in UTC; Pi keeps the host's time zone outside deterministic mode.
- `HostCall::Global` covers the discovery functions and any other global, since Pi's globals are generic; `Event` adds `Started` and `Idle`, and `Run` adds `answer_in_order` and `finished` for deterministic mode. `answer` returns an `AnswerError` instead of throwing.
- The prelude's own output-limit error is a script `RangeError`, as in Pi; `Failure::OutputLimit` is the host's recheck.
- Unhandled promise rejections are not recorded: Pi installs no handler, so `host_promise_rejection` is never enabled.
- Lone surrogates in strings leaving the VM become U+FFFD, and a JSON payload nested more than 128 levels deep or holding an escaped lone surrogate fails `serde_json` and ends the run as a broken bridge, where Pi would accept it.
- An unknown host function name, which `quickjs-wasi` throws into the script, ends the run as `Protocol`: the module's only host function is the bridge.
