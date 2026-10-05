---
description: "Run TypeScript programs in fresh WebAssembly QuickJS VMs on worker threads, where the declared async bindings are the only capability."
kind: "package-reference"
---

# bake-ptc-runtime-codemode

## Summary

Execute model-written TypeScript against host-provided async bindings in a fresh QuickJS VM. The VM is a separate WebAssembly instance on its own worker thread, run through [`@earendil-works/pi-codemode`](https://www.npmjs.com/package/@earendil-works/pi-codemode). Programs have no filesystem, process, network, timer, or module access: every effect goes through a declared binding, and so through the binding owner's policy. Each call returns captured logs, an exact JSON value, or a structured failure. Elapsed deadlines, a combined output budget, a VM heap limit, and pending-call limits constrain execution. This is the `run_code` runtime in shipped compositions.

## Table of Contents

- [Use this package](#use-this-package)
- [Understand the implementation](#understand-the-implementation)
- [Further Exploration](#further-exploration)
- [Model Experience](#model-experience)
- [Known Limitations and Deferred Work](#known-limitations-and-deferred-work)
- [Dev Note](#dev-note)

-----

<a id="use-this-package"></a>
## Use this package

Mount this provider in any composition; it injects no services. PTC mode in `dsh-tools` supplies the bindings, and each nested tool call goes through the tool registry with its own visibility, ordering, logging, guard, and approval rules.

### Configuration

```yaml
- name: 'bake-ptc-runtime-codemode'
  config:
    timeoutMs: 120000
    maxTimeoutMs: 600000
    maxOutputBytes: 67108864
    maxMemoryBytes: 536870912
    maxMessageBytes: 134217728
    maxPendingCalls: 128
```

| Field | Default | Meaning |
|---|---|---|
| `timeoutMs` | `120,000` | Default elapsed execution deadline, including nested tool and approval waits |
| `maxTimeoutMs` | `600,000` | Elapsed deadline ceiling applied by the resolver |
| `maxOutputBytes` | `67,108,864` | Combined serialized logs and completion or diagnostic budget |
| `maxMemoryBytes` | `536,870,912` | QuickJS heap limit for one program |
| `maxMessageBytes` | `134,217,728` | Serialized bytes of one binding call's arguments and of all outstanding binding arguments |
| `maxPendingCalls` | `128` | Maximum simultaneous host binding calls |

The [configuration catalog](../../../docs/config-catalog.md#bake-ptc-runtime-codemode) defines accepted config fields. `resolve(request)` supplies the cwd and the numeric deadline. It rejects an explicit `sandboxPolicy`, because programs have no file access to confine, and it advertises no `sandboxMode`. `run(spec)` accepts resolved inputs and does not fill missing values.

### Execution and results

Programs are async function bodies: top-level `await` and `return` work. The host strips erasable TypeScript types with Node's `stripTypeScriptTypes`, which keeps every position, then runs the body as a strict function. Its parameters are the declared namespaces and error classes, so the program's namespace is those bindings, `console`, and the standard built-ins (`JSON`, `Math`, `Date`, `RegExp`, `Map`, `Set`, `Promise`, and the rest of the language). `import`, `require`, `process`, `fetch`, `Buffer`, and timers do not exist.

Binding arguments, binding results, and the completion value must be lossless JSON. A lossy argument rejects the call inside the program with the namespace's error class; a lossy completion is `invalid-output`. Reading a member a namespace does not declare throws a `TypeError` that names close matches, and `"name" in namespace` tests membership. `console.log`, `info`, `warn`, `error`, and `debug` append one log entry each, rendering non-string values as JSON. Each run starts a fresh VM with no state from earlier runs.

### Deadlines and cancellation

The runtime's `timeout` descriptor reports its effective default and maximum, and `executionInstructions` describes the sandbox in the model-visible schema. Omitting `timeoutMs` uses the configured default; numeric requests are validated and capped. The deadline covers worker startup and execution, including time awaiting bindings or approval. Timeout, cancellation, and disposal interrupt the VM and terminate its worker before `run` resolves, even for a synchronous loop. A program awaiting a promise that no pending binding call can settle fails at once, because nothing in the VM could ever resume it.

### Failures

Type-stripping errors and thrown exceptions are `exception`; deadline expiry is `timeout`; cancellation and disposal are `abort`; direct use of the internal call channel, an undeclared binding, or exceeding `maxPendingCalls` or `maxMessageBytes` is `protocol`; a worker that fails or exits early is `worker-exit`. An exception's message is its `Name: message` head plus the program's own stack frames, written as `<program>:line:column` against the submitted program. A heap allocation beyond `maxMemoryBytes` surfaces as a catchable `InternalError: out of memory` exception. An oversized outer result is `output-limit` and retains the fitting log prefix. Invalid options, explicit sandbox policies, and calls after disposal reject as caller misuse.

-----

<a id="understand-the-implementation"></a>
## Understand the implementation

<details>
<summary>Implementation internals — click to expand</summary>

The host owns deadlines, binding lookup, validation, and result accounting. The VM and everything evaluated in it belong to the program, which is treated as a hostile peer. The worker entry is host code: it sits between the VM and the host thread.

### Program shape

pi-codemode evaluates its script inside `(async (tools, console) => {…})`. This provider's script calls a prelude with a strict `async function` whose parameters are the declared namespaces, their error classes, and pi-codemode's own helper names (`tools`, `ALL_TOOLS`, `text`, `image`, `exit`, `store`, `load`) shadowed as `undefined`. The program body follows on line 1, so its line numbers are exact. The prelude captures intrinsics before the program starts, snapshots arguments under the lossless-JSON rules, and sends every call through one pi-codemode global as `[namespace, member, args]`. The completion crosses as `[0, value]`, or `[1]` when it is lossy. The program can reach that global directly, so the host validates the call shape, the declared member, the arguments, and the completion again.

### Worker metering

pi-codemode keeps output in host memory without a limit. The worker entry, [`src/worker.ts`](src/worker.ts), therefore wraps its parent port before importing `@earendil-works/pi-codemode/worker`. Text output is charged against `maxOutputBytes` with exact JSON-byte accounting. Image output is dropped. A binding call whose arguments exceed twice `maxMessageBytes` stops the run. A completion or failure diagnostic that cannot fit the remaining budget stops the run. When the meter stops a run, it forwards the fitting prefix, reports a private crash signal, sets the VM interrupt flag, and drops later messages. The host revalidates everything the worker forwards. Per-run limits and the program layout travel in a leading comment on the script, which the worker removes before evaluation.

The meter relies on pi-codemode's internal worker protocol (`output`, `call`, `done`, and `crash` messages, and `workerData.code` and `interrupt`). The manifest therefore pins the dependency exactly.

### Source and built workers

The provider starts `worker.ts` beside its source module in source runs and `lib/worker.js` beside the bundle in builds. Both are erasable TypeScript loaded natively by Node. pi-codemode stays external, so its worker module and the QuickJS WebAssembly file resolve from the installed packages.

### Source map

| File | Role |
|---|---|
| [`src/index.ts`](src/index.ts) | Configuration, resolution, host binding calls and outcome mapping |
| [`src/program.ts`](src/program.ts) | Type stripping, the strict wrapper and the VM prelude |
| [`src/worker.ts`](src/worker.ts) | Output, argument and completion metering before data reaches the host |
| [`src/protocol.ts`](src/protocol.ts) | Worker header, crash signals and program-relative stack rendering |
| [`src/output.ts`](src/output.ts) | Exact JSON byte accounting for the outer result |
| [`src/bindings.ts`](src/bindings.ts) | Portable binding-name validation |
| — | No runtime invariant companion is published; budgets are enforced across the worker boundary rather than through independent same-process observations. |

</details>

-----

<a id="further-exploration"></a>
## Further Exploration

Read the service contract before using the provider directly.

- [PTC runtime service](../ptc-runtime/README.md) — requests, resolved specs and results.
- [PTC mode](../../core/tools/README.md#ptc-mode) — `run_code`, the typed SDK and nested tool dispatch.

-----

<a id="model-experience"></a>
## Model Experience

Indirectly, through PTC mode in `dsh-tools`. `executionInstructions` adds this sentence group to the `run_code` description: each call runs in a fresh JavaScript sandbox; declared SDK bindings are the only access to files, processes, and the network; `import`, `require`, `process`, `fetch`, and timers do not exist; standard built-ins are available; `console.log` prints non-string values as JSON; and a program awaiting a promise no pending binding call can settle fails immediately. The runtime advertises no `sandboxMode`, so `run_code` offers no `sandbox_permissions` or `justification` parameters. Intermediate binding traffic stays outside model history; the outer result follows the ordinary tool spill policy.

#### KV Cache effect

The description text is fixed per deployment, so it changes the request prefix only when the composed runtime changes.

## Known Limitations and Deferred Work

<a id="known-limitations-and-deferred-work"></a>

These limits qualify the execution guarantees and the dependency.

- **The worker meter depends on pi-codemode internals** — an upgrade must re-check its worker messages, `workerData` fields, script prefix (`CODEMODE_SCRIPT_PREFIX`), and `codemode.js` stack file name before changing the exact pin.
- **The heap limit covers the QuickJS heap only** — the worker thread's own V8 heap, which holds messages in transit, has no separate resource limit.
- **Programs cannot use Node or web platform APIs** — no timers, `fetch`, `TextEncoder`, `URL`, `structuredClone`, `Intl`, or `crypto`; anything else needs a binding.
- **Execution is one-shot** — no yield/wait API, live result stream, or retained program state exists between calls.
- **Bindings are bounded at admission** — limits do not bound the memory a host binding allocates while producing its result.
- **Error text follows QuickJS** — messages are terser than V8's (for example `not a function`), which the member guard offsets for binding namespaces.

<a id="dev-note"></a>
### Dev Note

<details>
<summary>Working context for maintainers — click to expand</summary>

None.

</details>
