# Rust migration comparison harness

## Summary

Run the same controlled fixtures through TypeScript and Rust, then compare prompt bytes, event order, permission records, and the files each runner changed. The fixtures qualify the comparison tooling for [migration scope 01](../docs/roadmap/rust-0.4/README.md#01--workspace-and-comparison-harness). They do not run Bake's agent, implement its permission policy, or establish runtime parity. The separate [native eval fixture adapter](../evals/README.md#native-fixture-adapter) checks a compiled fake arm with the evaluator's existing file predicates. The [qualification ledger](../docs/roadmap/rust-0.4/ledger/README.md) records partial evidence and failed attempts. A [runtime request-reconstruction fixture](#runtime-request-reconstruction) captures one real TypeScript tool-call turn. Rust replay and a live native eval arm remain open.

## Table of Contents

- [Run the comparisons](#run-the-comparisons)
- [What is compared](#what-is-compared)
- [Shared fixtures](#shared-fixtures)
- [Runtime request reconstruction](#runtime-request-reconstruction)
- [Runner contract](#runner-contract)
- [Ownership and limits](#ownership-and-limits)

## Run the comparisons

Use the repository's Bun dependencies and [pinned Rust toolchain](../rust/README.md#run-it). From the repository root:

```sh
bun run check:rust
bun run test:rust:conformance
```

The first command checks formatting, lints, tests, and builds the Cargo workspace. The second runs the Bun fixture runner and the built `bake-conformance-runner` binary against all three fixtures. Success prints one `pass` line per fixture and a final `synthetic conformance passed` summary. The report is written to ignored `.preflight/rust-conformance/report.json`; it contains fixture and runner-file hashes, host information, process outcomes, and comparison results. Raw prompts, observations, and child stderr are excluded from that report. The Rust binary digest identifies the built arm; the report does not infer its compiler version from the local toolchain. Raw child stderr and host errors are saved separately to ignored `.preflight/rust-conformance/diagnostics.json`.

Exit 0 means every comparison passed. Exit 1 means a runner or comparison failed, and exit 2 means arguments or setup were invalid. Interrupting the driver cancels its current child and exits 130. Read the report's separate process and comparator results when diagnosing a failure; a timeout is recorded independently of an exit code or signal. A failed run prints the diagnostics path. Starting a run removes the previous output files, including when setup fails, and completed documents are published atomically. Use `--output-dir <dir>` for concurrent CLI runs that need separate reports; sharing an output directory retains the last published files.

Run the TypeScript tests, including deliberate mismatches, without building Rust:

```sh
bun test --parallel --timeout=30000 scripts/rust-conformance
```

`bun run preflight --only native` builds Rust before running both this harness and the preview's PTY checks. The comparison harness runs on Linux, macOS, and Windows; it requires no terminal. `--fast` skips checks that need the Rust build. The ordinary TypeScript typecheck and tooling-test gates cover the driver and its tests.

## What is compared

The driver checks each runner against the independently specified expected values and compares the two runners with each other. It keeps all four comparison results, so one mismatch cannot hide another.

| Comparator | Required agreement | Deliberate failing case |
|---|---|---|
| `prompt-bytes` | Exact UTF-8 bytes and prompt count, including CRLF and trailing spaces | Change one byte without changing length |
| `event-order` | Every event, field, and array position; object key order is immaterial | Swap adjacent events |
| `permissions` | Exact ordered permission records | Change one reported decision without changing file effects |
| `final-files` | Exact regular-file path set and bytes read by the driver after the process exits | Change a file while returning an otherwise correct observation |

The driver also checks protected files against their bytes before the run. Changing an expected result to agree with a modified protected file cannot make that check pass. The runner never reports file hashes or supplies expected values. No IDs, paths, whitespace, unknown event fields, or errors are normalized away.

## Shared fixtures

[`fixtures/`](fixtures/) holds hand-authored inputs and expected outcomes:

- `allow-write.json` covers Unicode, CRLF, trailing whitespace, a permitted nested-file edit, an unchanged binary file, and protected files.
- `deny-write.json` preserves an existing file and leaves a refused new file absent.
- `ordered-writes.json` writes the same path twice and requires the second write's bytes.

Each version-1 `bake/synthetic-conformance/fixture` document has an ID, an `input`, initial file bytes encoded as lowercase hex, protected paths, and independent expected observations and final file bytes. The driver creates a separate workspace for each runner and sends only `input` to it. Expected values stay in the driver.

[`invalid/`](invalid/) holds malformed runner inputs shared by the TypeScript and Rust tests. These cover schema and permission errors, unsafe paths, invalid Unicode, duplicate keys, numeric forms, and excessive event nesting. The original runtime tests and frozen session generations remain their own oracles; these synthetic fixtures do not replace them.

## Runtime request reconstruction

[`runtime/request-reconstruction/tool-call-turn/`](runtime/request-reconstruction/tool-call-turn/) holds one current-format Session log from the real TypeScript loop and independently specified provider-neutral requests. The user sends `go`; the scripted external model calls `echo` with `{text:"one"}` and text `calling`; the tool returns `echo: one`; the model answers `done`. The loop dispatches two requests. The [fixture spec](../packages/core/agent-loop/tests/request-reconstruction.fixture.spec.ts) composes the actual core plugins with `MockAdapter` replacing the external model. It does not boot a shipped profile through the Loader.

Run the fixture, its retained original oracle, and the physical-fixture classification check on Node:

```sh
bun run test:runtime packages/core/agent-loop/tests/request-reconstruction.fixture.spec.ts packages/core/agent-loop/tests/request-reconstruction.spec.ts scripts/session-fixture-layout.spec.ts
```

The spec compares live requests and requests reconstructed from the saved log against `expected-requests.json`. Each replay prefix ends before its matching Assistant settlement, so a request excludes its own response. Two private live runs check request determinism. The committed log must parse completely and re-encode to the same bytes. Missing files, a torn log, changed tool content, reordered events, or a missing expected request fail the relevant check. Normal tests only read the fixture files; they never generate missing files or update snapshots.

The expectation was authored before capture from the scenario, the tool declaration, and the model adapter's documented defaults. `includeHarnessIdentity: false` leaves the exact system text `stable base`; optional reasoning effort, token cap, temperature, stop sequences, one-shot system text, purpose, and tool updates are absent. The result includes message sources, tool-call arguments, explicit success status, tool history, and session identity. The helper rejects unknown dispatch fields and excludes only the non-data abort signal; an `undefined` property is omitted as JSON omits it.

The capture uses production sources at `5cd716c70fdc443c9e997ee7b5e67ed2d4a1b06f` with this test composition. `session.jsonl` preserves the observed header, event envelopes, UUIDs, and stream timing. Request comparison maps only generated message IDs through one bijection per request sequence; repeated identities stay related, while opaque text and tool arguments stay exact. The timing of another execution need not reproduce the captured timing. This fixture does not measure performance.

Keep the committed generation and expectation unchanged. A correction or another scenario belongs in a new directory, with its own provenance and review. The [fixture-layout policy](../scripts/session-fixture-layout.ts) explicitly recognizes this physical log so the logical-fixture formatter cannot strip its envelopes. The original [request-reconstruction tests](../packages/core/agent-loop/tests/request-reconstruction.spec.ts) remain in place, including the broader header-change scenario.

The runtime suite exercises this fixture, and the ordinary TypeScript check includes its helper and spec. There is no Rust reader or reducer for it yet. Provider wire encodings, historical formats, seeded/forked logs, compaction, retries, cancellation, changing request headers, profile composition, and cross-platform release qualification remain separate work.

## Runner contract

The [TypeScript arm](../scripts/rust-conformance/runner.ts) and [Rust arm](../rust/crates/bake-conformance/src/lib.rs) independently implement a small test protocol. Each reads one UTF-8 JSON input from stdin to EOF. It validates the entire document before writing, applies permitted writes in order, and prints one JSON observation followed by LF. Invalid input exits 2; an I/O failure exits 1. File writes are not transactional: an earlier write can remain after a later I/O failure, and that failed run cannot pass the harness.

The input schema is `bake/synthetic-conformance/input`, version 1. It contains prompt strings, event objects, permission records, and write requests. Permission decisions are supplied fixture data, rather than decisions made by Bake's policy engine. The output schema is `bake/synthetic-conformance/observation`, version 1, containing the relayed prompts, events, and permissions. This is a test protocol, not a Session or provider protocol.

The [TypeScript validator](../scripts/rust-conformance/fixture.ts) and Rust parser enforce the same bounds: 256 KiB per document, 64 entries per vector, 64 KiB per text or file payload, 200 UTF-8 bytes per relative path, and 32 nested event containers. File payloads use hex, so their encoded text can occupy twice the byte limit. JSON numbers use plain decimal integer tokens in the safe integer range; negative zero, fraction and exponent forms, duplicate object keys, BOMs, and unpaired surrogates are rejected. Paths exclude traversal, absolute forms, Windows-invalid characters and device names, and trailing dots or spaces.

## Ownership and limits

Each run owns a private home, temporary directory, workspace, and child process. Children receive a minimal environment rather than inherited model credentials. The driver bounds process time and output, waits for child closure, and removes the run's directories. Workspace observation visits at most 1,024 entries and reads at most 256 regular files, each bounded to 64 KiB. On POSIX, timeout, overflow, and cancellation stop the owned process group; on Windows, they stop the direct child. Both supplied runners are single processes.

The harness runs trusted fixtures and executables. It provides no sandbox boundary for a hostile runner. File comparison rejects symlinks and special files; it does not qualify empty-directory semantics, permissions, or filesystem modes. The synthetic driver performs no identity normalization. Runtime fixtures and live models require their own evidence. The Rust workspace and these fixtures remain development files, outside the shipped 0.3 launcher and release archives.
