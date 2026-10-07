# Rust migration comparison harness

## Summary

Compare TypeScript and Rust using controlled fixtures and independently checked outcomes. The synthetic harness and [native eval fixture adapter](../evals/README.md#native-fixture-adapter) qualify comparison tooling for [migration scope 01](../docs/roadmap/rust-0.4/README.md#01--workspace-and-comparison-harness). Separate shared cases exercise Session headers, source references, row envelopes, strict V3 codec rows, and scans of plain logs against released codecs, and restoration of plain logs as the production read path restores them; three runtime fixtures capture a real TypeScript tool-call turn, a tool added, removed, and restored across turns, and a model request retried under a changed model, and request derivation cases replay the first and its variants through the TypeScript replay helper. The [qualification ledger](../docs/roadmap/rust-0.4/ledger/README.md) records partial evidence. Agent resume, restoration of compressed or migrated logs, replay of seeded, resumed, or compressed logs, and live native evals remain open.

## Table of Contents

- [Run the comparisons](#run-the-comparisons)
- [What is compared](#what-is-compared)
- [Shared fixtures](#shared-fixtures)
- [Runtime request reconstruction](#runtime-request-reconstruction)
- [Session header cases](#session-header-cases)
- [Source-event seq cases](#source-event-seq-cases)
- [Row-envelope cases](#row-envelope-cases)
- [V3 row cases](#v3-row-cases)
- [Log scan cases](#log-scan-cases)
- [Request derivation cases](#request-derivation-cases)
- [Plain log restoration cases](#plain-log-restoration-cases)
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
bun run test:runtime packages/core/agent-loop/tests/request-reconstruction.fixture.spec.ts packages/core/agent-loop/tests/request-reconstruction.spec.ts packages/core/agent-loop/tests/tool-updates.spec.ts scripts/session-fixture-layout.spec.ts
(cd rust && cargo test --locked -p bake-session --test runtime_capture)
```

The spec compares live requests and requests reconstructed from the saved log against `expected-requests.json`. Each replay prefix ends before its matching Assistant settlement, so a request excludes its own response. Two private live runs check request determinism. The committed log must parse completely and re-encode to the same bytes. Missing files, a torn log, changed tool content, reordered events, or a missing expected request fail the relevant check. Normal tests only read the fixture files; they never generate missing files or update snapshots.

The expectation was authored before capture from the scenario, the tool declaration, and the model adapter's documented defaults. `includeHarnessIdentity: false` leaves the exact system text `stable base`; optional reasoning effort, token cap, temperature, stop sequences, one-shot system text, purpose, and tool updates are absent. The result includes message sources, tool-call arguments, explicit success status, tool history, and session identity. The helper rejects unknown dispatch fields and excludes only the non-data abort signal; an `undefined` property is omitted as JSON omits it.

The capture uses production sources at `5cd716c70fdc443c9e997ee7b5e67ed2d4a1b06f` with this test composition. `session.jsonl` preserves the observed header, event envelopes, UUIDs, and stream timing. Request comparison maps only generated message IDs through one bijection per request sequence; repeated identities stay related, while opaque text and tool arguments stay exact. The timing of another execution need not reproduce the captured timing. This fixture does not measure performance.

Keep the committed generation and expectation unchanged. A correction or another scenario belongs in a new directory, with its own provenance and review. The [fixture-layout policy](../scripts/session-fixture-layout.ts) explicitly recognizes this physical log, and the `dynamic-tools` and `retry-attempt` logs, so the logical-fixture formatter cannot strip its envelopes. The original [request-reconstruction tests](../packages/core/agent-loop/tests/request-reconstruction.spec.ts) remain in place, including the broader header-change scenario.

The runtime suite exercises this fixture, and the ordinary TypeScript check includes its helper and spec. Rust reads its header record through the [Session header cases](#session-header-cases) and decodes its `sourceEventSeqs` fields and row envelopes through the [source-event seq cases](#source-event-seq-cases) and [row-envelope cases](#row-envelope-cases). It derives the fixture's two requests through the [request derivation cases](#request-derivation-cases), over a closed subset and without restoration. Provider wire encodings, historical formats, seeded/forked logs, compaction validity, retry policies, cancellation, profile composition, and cross-platform release qualification remain separate work. Changing request headers and retried requests are covered only as the `dynamic-tools` and `retry-attempt` scenarios below exercise them.

### Dynamic tools

[`runtime/request-reconstruction/dynamic-tools/`](runtime/request-reconstruction/dynamic-tools/) holds a second capture from the same composition, with Session `dynamic-tools` and the tool `install` (`install fetch`, parameter `name`) instead of `echo`. The scenario has three turns and four requests:

1. The user sends `install fetch`. The scripted model calls `install` with `{name:"fetch"}` and text `installing`. Executing `install` registers `fetch` (`fetch a url`, parameter `url`) and returns `installed`; the model answers `ready`.
2. While the agent is idle, the runner removes `fetch`. The user sends `drop fetch`; the model answers `dropped`.
3. While idle, the runner registers an identical `fetch`. The user sends `restore fetch`; the model answers `restored`.

The system prompt plugin orders tools by name, so requests 2 and 4 declare `fetch` before `install`. Every request after the first logs a changed header (`reason: "change"`, without `startsSeries`) and one `request/tool-update`: `fetch` is added after the tool result, removed after `drop fetch`, and added again after `restore fetch`. The identical re-addition does not redeclare `fetch`, so the tool history keeps the first request's `[install]` baseline and accumulates all three updates. With no tool-update mode on the mock route, requests carry `tools` and `toolHistory` and omit `toolUpdates` and `deferLoading`. The log has 38 rows; requests are cut at the Assistant settlements, rows 8, 15, 25, and 35.

`runDynamicToolsScenario` in the [fixture helper](../packages/core/agent-loop/tests/runtime-fixture.ts) uses the registration disposer to remove `fetch` between turns, subscribes to idle before each follow-up, and awaits its Context disposal on every exit. The Context owns all registrations. The runner checks that the request count stays unchanged while disposal is pending, including after an aborted wait. The spec checks live and replayed requests against the expectation, two-run determinism, the committed log's SHA-256, complete scan, Session admission, byte-exact re-encoding, and its header, update, and cut rows. Controls change the live `fetch` description, redeclare `fetch` in the last logged header, which resets the replayed history, anchor the second update to an earlier message, which Session admission refuses, drop an expected request, and extend a cut past its settlement.

Requests carry tool-history anchors, so this scenario uses `normalizeAnchoredRequests`. It maps message IDs and then each `toolHistory.updates[].afterMessageId` through one bijection over the request sequence. An anchor must be a generated UUID that a message of the same request carries, and a request with `toolUpdates` is refused. `normalizeRequests` is unchanged and still leaves anchors raw, which the [request derivation cases](#request-derivation-cases) rely on.

The expectation was written from the scenario, the declared schemas, and the adapter defaults, and hashed before capture. The first draft listed tools in registration order; before capture it was corrected to the system prompt's name ordering and hashed again. The capture ran in a clean detached worktree of production sources at `ff27e58e04d9c58bfe07d9e55ce57afc6c0ead3b`. Its ignored entry composed the same production plugins and scripted these three turns. `session.jsonl` preserves the observed header, envelopes, UUIDs, and timing.

The development [`bake-session`](../rust/crates/bake-session/tests/runtime_capture.rs) crate replays the `tool-call-turn` and `dynamic-tools` logs from bytes with `replay_requests`, normalizes them with its own anchor rules, and compares them with each hand-written expectation. It reads no TypeScript output. Its controls cover the redeclared header reset, the misplaced anchor (`ToolUpdateAnchor`), a missing settlement, and foreign, absent, or non-UUID anchors. Replay and re-encoding prove that every request is reconstructable from the log. They do not prove live Agent restoration, resume, the live tool registry, which the log does not record, adapter projection for routes with a tool-update mode, or the JSONL plugin load path.

### Retry attempt

[`runtime/request-reconstruction/retry-attempt/`](runtime/request-reconstruction/retry-attempt/) holds a third capture from the `tool-call-turn` composition, with Session `retry-attempt` and `echo` registered. The user sends `go`. The scripted model's first stream ends with a retryable `SERVER` error before any content, and the loop settles that dispatch as an `assistant/attempt`. An `agent/request-error` listener returns `{ kind: 'retry' }` once and then delegates; an `agent/request` listener switches every request after that retry to model `mock-b`. No retry policy is loaded, so nothing waits on a timer. The retried dispatch answers `done`.

Tools and the system prompt are assembled once per step, so both requests carry the same messages, the system text `stable base` and the user message `go`, and the same `echo` schema; only `model` differs. The log has 14 rows: an `initial` header, an `assistant/attempt` at row 8, a `change` header for `mock-b` without `startsSeries` and its `request/context` at rows 9 and 10, then the `assistant/message` at row 11. There is no second `system/message` and no tool update. Requests are cut at the settlements, rows 8 and 11, so the second request's prefix contains the attempt and the changed header.

`runRetryAttemptScenario` in the [fixture helper](../packages/core/agent-loop/tests/runtime-fixture.ts) registers both listeners on its Context, which owns them, and awaits its disposal on every exit. The script's failure count and the listener's retry count bound every run, so a wrong retry decision ends the turn instead of looping. The spec checks live and replayed requests against the expectation, the single retry decision, two-run determinism, the committed log's SHA-256, complete scan, Session admission, byte-exact re-encoding, and its header, context, and settlement rows. An abort before the turn and an abort at the retry both unwind through disposal. Bounded variants show that no retry yields one request, two retries yield three, and two failures with one retry yield the expected two requests but end the turn in error. Controls replace the attempt with a log-only row, which leaves one request; cut the second request before the changed header, which keeps model `mock`; and give the attempt a non-array `stream` or a -0, which Session construction refuses. Its requests carry no tool-history anchors, so they use `normalizeRequests`.

The expectation was written from the scenario and the composition's sources, and hashed before the runner existed. The capture ran in a clean detached worktree of production sources at `9c851609c8719198742ffd78a76031ae38aa0453`, with frozen dependencies installed there. Its ignored entry ran this helper with only its mock adapter import path changed, and recorded every module file Vite loaded: each was a tracked file equal to its blob at that commit, apart from the two entry files. `session.jsonl` preserves the observed header, envelopes, UUIDs, times, and streams.

The development [`bake-session`](../rust/crates/bake-session/tests/runtime_capture.rs) crate replays this log from bytes, one request per settlement, with its own normalizer, and compares the requests with the hand-written expectation. Its controls replace the attempt with a log-only row, which fails the request count; replace the changed header, which fails at `/1/model`; and give the attempt a non-array `stream` (`Settlement`) or a -0 (`LosslessJson`). As with `dynamic-tools`, this proves the requests are reconstructable from the log, not restoration, resume, or retry policy behavior.

## Session header cases

[`session/header-cases.json`](session/header-cases.json) holds current-format (format 3) Session header records and the outcome of each. The [TypeScript header spec](../packages/session/session-persistence-jsonl/tests/header-conformance.spec.ts) runs every record through the real `SessionLogScanner` constructor, which parses the header of every current log. The development [`bake-session`](../rust/crates/bake-session/src/lib.rs) crate runs the same records through `read_header_record` under both path platforms. Neither harness reads event rows: agreement covers these header records, not whole logs, replay, or writers.

```sh
bun run test:runtime packages/session/session-persistence-jsonl/tests/header-conformance.spec.ts
(cd rust && cargo test --locked -p bake-session)
```

Each case supplies exactly one record source:

- `record`: text; both harnesses append the LF.
- `bytesHex`: the complete record bytes, used for framing and invalid UTF-8.
- `fixtureFirstRecord`: only the request-reconstruction `session.jsonl`, whose first record both harnesses read without changing the file.

`ts` is the scanner's outcome: `admitted`; `unsupported`, with whether the version is newer; `rejected`, with a reason that maps to exactly one error message; or `type-error`, the specific ID-conversion error that only a native-subset case may expect. It splits into `posix` and `win32` where Node's `path.isAbsolute` decides `cwd` differently. `meta` is the expected logical header of an admitted case. Rust checks it for every admitted case; TypeScript checks it for unseeded cases. Outcome precedence follows the scanner: framing, JSON, object, a numeric version other than 3, the retired `sandboxMode` and `approvalPolicy` fields, then the header shape. A failing shape check rejects even when a count is undecided, and collision rows pin these orders. A JSON `null` is a present value of the wrong type, never an omitted field, and a duplicate key keeps its last value. For a seeded header (`isSeeded: true`), TypeScript checks only the constructor outcome, because it exposes header metadata only after a complete log. The `absolutePaths` rows pin both Node path flavors on every host. Rust CI runs on Linux, macOS, and Windows, and the Windows release job runs the TypeScript spec so the Win32 `cwd` rows meet the real scanner.

A `rust` override marks a case whose TypeScript outcome Rust does not reproduce. Rust then returns `NativeSubset` and claims no TypeScript class. Of the 86 cases, 19 are native subset:

- `float-lexeme`: a number that serde_json stores as a non-negative `f64` decides the version or a count. These are fraction and exponent spellings, including `0.0`, and integers above `u64::MAX`; serde_json's default float parsing is not proven to round as JavaScript does. A negative number that serde_json parses is decided exactly.
- `json-parser` (5): serde_json refuses input that `JSON.parse` accepts, such as lone surrogate escapes, nesting at serde_json's recursion limit, and out-of-range numbers. Rust reports a JSON rejection only for a fixed list of serde_json 1.0.151 syntax-error codes. A unit test requires a case in this table, which the TypeScript spec runs through `JSON.parse`, to witness each code. Any other parse error is a subset refusal.
- `invalid-utf8` (2): Node decodes invalid UTF-8 with replacement characters.
- `version-diagnostic` (4): a foreign version's `id` is an object or array. The scanner formats it with `String(id)`, which can throw a `TypeError`; Rust leaves that conversion outside this subset, including objects and arrays TypeScript can convert.

Expected metadata never contains a lone surrogate; such input appears only as escaped text in `record`. Expectations come from the TypeScript scanner, not from the Rust reader. The table's rows were selected from a wider measured probe, with precedence-collision rows, refusal-message rows, and ten syntax-error witnesses added.

## Source-event seq cases

[`session/source-event-seqs-cases.json`](session/source-event-seqs-cases.json) holds values of one event's physical `sourceEventSeqs` field and the outcome of each. The current format's codec delegates this field to the released v2 codec. The [TypeScript spec](../packages/session/session-format-v2-to-v3/tests/source-event-seqs-conformance.spec.ts) creates that codec's strict decoder, decodes `seq` minimal rows numbered from 0, and then decodes one row carrying the field. The development [`bake-session`](../rust/crates/bake-session/src/source_event_seqs.rs) crate passes the same parsed value and seq to `decode_source_event_seqs`. Agreement covers source-reference decoding only, not V3 event, payload, or replay admission: the minimal rows have no valid V3 payload, and Rust validates no row.

```sh
bun run test:runtime packages/session/session-format-v2-to-v3/tests/source-event-seqs-conformance.spec.ts
(cd rust && cargo test --locked -p bake-session)
```

Each case has an `id`, the event `seq`, at most 64 so the TypeScript spec decodes at most 64 priming rows, and the field as JSON text in `field`, so number spellings such as `1.0` and `-0` reach both parsers unchanged. A case without `field` omits the property, which both runtimes leave absent; JSON `null` is a present non-array value. `ts` is `absent`, `decoded` with the exact expanded `seqs`, or `rejected` with the exact `SessionFormatError` message. Any other error fails the TypeScript spec.

The decoder keeps scalar members in order. A `[start, end]` pair expands inclusively; its end must be earlier than the event seq, and it may not make the expanded output longer than the event seq. Every expanded seq must then be unique and earlier than the event seq, and only a list containing a range must strictly increase. Checks run in TypeScript's order, so the first failing member, pair, start, end, or range check wins over later entries, and uniqueness is checked before order. Rust's error also names the failing entry or seq, which the TypeScript message omits.

A `rust` override marks a case whose TypeScript outcome Rust does not reproduce; Rust returns `NativeSubset` and claims no TypeScript error. The two native-subset limits are:

- `float-lexeme`: a number serde_json stores as a non-negative `f64`, such as `1.0`, `2e0`, or an integer above `u64::MAX`. Rust stops at the first such number, without claiming JavaScript's rounding or the outcome of later entries. Every negative spelling, including `-0` and `-0.0`, is a negative number or -0 in JavaScript and is rejected exactly.
- `output-budget`: the expansion would exceed the case's `budget`, or the table's `defaultBudget` of 64 when the case sets none. Rust applies each entry's member, pair and range checks, then checks the budget before adding or expanding it. This bounds the entry counts of the output and uniqueness set. Budget exhaustion precedes the uniqueness and global-order checks, which run after expansion. TypeScript has no budget and keeps its own outcome.

Both harnesses pin the case count, require the cases to witness all eight messages and both limits, and reject unknown keys. Invalid JSON and UTF-8 are outside the field decoder, which takes an already parsed value. A Rust unit test refuses a range of 2^53 − 1 seqs against a budget of 16 without expanding it; the TypeScript spec never decodes such a range.

`fixtureReferences` lists the decoded references of the unchanged [request-reconstruction log](#runtime-request-reconstruction): its `tool/result` at seq 10 cites seq 9. TypeScript decodes the whole log through the real V3 codec in strict mode; Rust decodes only each row's `sourceEventSeqs` field. Both compare the exact list, not only its length. Expected expanded values and TypeScript refusals are specified in the shared table and checked against the released codec.

## Row-envelope cases

[`session/event-envelope-cases.json`](session/event-envelope-cases.json) holds event rows and the outcome of one strict released v2 row decode. The [TypeScript spec](../packages/session/session-format-v2-to-v3/tests/event-envelope-conformance.spec.ts) creates the released v2 codec's strict decoder, decodes `expectedSeq` priming rows numbered from 0, and then decodes the case row. The development [`bake-session`](../rust/crates/bake-session/src/envelope.rs) crate passes the same parsed row and expected seq to `decode_row_envelope`. That call checks the envelope fields, decodes `sourceEventSeqs` against the row's own seq, checks the seq gap, and requires a `session/end-seed` row's data to be an object. The result borrows `data` and `surfaceOp` unvalidated.

```sh
bun run test:runtime packages/session/session-format-v2-to-v3/tests/event-envelope-conformance.spec.ts
(cd rust && cargo test --locked -p bake-session)
```

Agreement covers that one call, not current-format row admission; [V3 row cases](#v3-row-cases) cover the whole codec call. The V3 codec checks `request/header` and `system/message` payloads and obsolete dispatch rows before the call, and known-event envelope and payload rules after it. V2 may accept rows that V3 rejects, such as a `turn/start` with `surfaceOp`, a `user/message` with empty sources, or a `-0` seq. Recovery modes, a seeded log's end-seed checks in `finish`, record framing, and replay are not modelled. Messages assume strict, contiguous decoding, where the row index equals the expected seq.

Each case has an `id`, an `expectedSeq` of at most 16, and the row as JSON text in `row`, so number spellings such as `1.0` and `-0` reach both parsers unchanged. An optional `budget` replaces the table's `defaultBudget` of 64 for expanded source seqs. `ts` is one of:

- `decoded`, with the envelope's `type`, `seq`, `time`, and `ignorable`, plus `sourceEventSeqs` and `surfaceOp` exactly when the row carries them. A present JSON `null` stays distinct from an omitted field. Both harnesses also check that the payload is the row's own value, not a copy.
- `rejected`, with the exact `SessionFormatError` message.
- `thrown`, for a `TypeError`. The gap message formats the seq with JavaScript string conversion outside the decoder's error handling, so a seq such as `{"toString":null}` throws instead.

Checks run in TypeScript's order and the first failure wins: object, required fields in the order `type`, `seq`, `time`, `data`, unexpected fields, type, time, `ignorable`, then, only when `sourceEventSeqs` is present, the seq as a non-negative safe integer and the field itself, then the gap, then end-seed data. Without `sourceEventSeqs`, a wrong-typed seq reaches the gap message.

A `rust` override marks a case whose TypeScript outcome Rust does not fully reproduce. A `native-subset` override names a limit; Rust claims nothing and fires it at the TypeScript check that reads the value, so it may hide a later rejection but never an earlier one:

- `time-float-lexeme`, `seq-float-lexeme`, and `source-float-lexeme`: serde_json stores the number as an `f64` other than -0, such as `1.0`, `1e3`, or `1.5`. JavaScript accepts the integral ones. Negative values and negative zero are rejected exactly where TypeScript requires a count. Negative-zero times are also rejected exactly; other floating-point times remain subset-limited.
- `negative-zero-seq`: a `-0` seq without `sourceEventSeqs`. The released v2 decoder accepts it as seq 0 at expected seq 0, and V3 rejects it.
- `seq-diagnostic`: an object or array seq without `sourceEventSeqs`. JavaScript's string conversion yields text such as `[object Object]` or throws. With `sourceEventSeqs`, the same seq is rejected exactly as an invalid count.
- `source-output-budget`: the source expansion exceeds the budget, which TypeScript does not have.

A `rejected-class` override claims the refusal but not its message:

- `unexpected-fields`, with the unexpected keys in byte order: JavaScript names the first key in its property order, integer-like keys first and then insertion order, which a parsed serde_json map does not keep. Rust's message is exact for one unexpected key.
- `seq-gap`: an integer seq outside the safe range is certainly a gap, but Rust does not render JavaScript's rounded value. Safe integers, strings, `null`, and booleans are rendered exactly.

An optional `v3` outcome records what the real V3 codec does with the same rows. The TypeScript spec checks it; Rust ignores it. These controls show V3 rejecting rows the released v2 decoder accepts, reporting its structural error before a v2 envelope error, and accepting an unknown type with opaque payload.

Both harnesses pin the case count, reject unknown keys, and require every rejection kind, limit, and class-only refusal to be witnessed. `fixtureEnvelopes` lists the 16 envelopes of the unchanged [request-reconstruction log](#runtime-request-reconstruction), covering 12 event types. TypeScript checks the log's SHA-256 and decodes it through the real V3 codec; Rust, which has no hash dependency, checks its size and record count and decodes each row. Expected envelopes and messages are written in the shared table and checked against the released codec.

## V3 row cases

[`session/v3-row-cases.json`](session/v3-row-cases.json) holds event rows and the outcome of one strict V3 codec row decode. The [TypeScript spec](../packages/session/session-format-v2-to-v3/tests/v3-row-conformance.spec.ts) primes the real V3 codec's strict decoder with `expectedSeq` rows numbered from 0, then decodes the case row. The development [`bake-session`](../rust/crates/bake-session/src/v3_row.rs) crate passes the same parsed row and expected seq to `decode_v3_row`.

```sh
bun run test:runtime packages/session/session-format-v2-to-v3/tests/v3-row-conformance.spec.ts
(cd rust && cargo test --locked -p bake-session)
```

The call runs three steps, and the first failure wins:

1. Raw-row admission, before any envelope check. A `request/header` row needs object data and an object header without its own `system` member. `system/message` data must pass the codec's own key, coordinate, identity, and plugin-source checks, then the frozen payload validator. An obsolete `tool/code-dispatch` or `tool/code-dispatch-start` row that is not ignorable is unsupported; the message renders its raw seq with JavaScript's `String`.
2. The [row envelope](#row-envelope-cases), unchanged.
3. Per-event checks. A known type other than a surface accepts only `ignorable` beside the required fields. A surface requires `surfaceOp`: `"append"`, or exactly `op: "replace"`, `startSeq`, and `endSeq`, each counted and then required to be earlier than the row; `startSeq` may exceed `endSeq`. Then `assistant/message` refuses sources, and other surfaces refuse an empty list. A `request/header` refuses `tools: []` and `adapterDefaults: {}`. A `tool/result` with an `error` member, `null` included, needs an object message holding exactly one `tool-result` block whose `isError` is `true`.

A decoded row is codec output, not a restored event. Unknown types and ignorable obsolete types decode with any optional fields and an opaque payload, and other payloads stay unvalidated; restoration later requires an installed vocabulary and checks relationships between events. Known types are the surfaces, the released v2 disposition keys other than the obsolete types, `tool/ptc-dispatch-start`, `tool/ptc-dispatch`, `feedback/message-put`, `feedback/message-delete`, and the 12 `Object.prototype` names, which the frozen dispositions object inherits. The table's `vocabulary` lists them with near-miss opaque names. The TypeScript spec checks the lists against the real exports, Rust checks its constants against them, and both harnesses decode a row of every listed name. Recovery modes, `finish`, framing, and replay are not modelled here; [log scan cases](#log-scan-cases) cover them.

`ts` is `decoded`, with the envelope as for row envelopes, or `rejected` with an exact `class`. `SessionFormatError` and `SessionFormatUnsupportedMigrationError` carry the exact message; `TypeError` carries none, because a `{"toString":null}` seq on an obsolete row throws with different messages in Bun and Node.

A `rust` override follows the row-envelope conventions. A class-only `rejected-class` lists `unexpectedKeys` when two or more keys are unexpected, and nothing for an unrendered seq gap. A `native-subset` override names a limit:

- `system-payload`: Rust claims the frozen validator's acceptance only for a source of exactly `kind` and a plugin other than `compact`, and content blocks that are each exactly `{type: "text" | "reasoning", text: <string>}`, an empty list included. Any other shape is a limit, including ones TypeScript accepts.
- `obsolete-seq-diagnostic`: an obsolete row's seq is an array, an object, an integer outside the safe range, or any number serde_json stores as an `f64`, -0 included. Rust does not reproduce JavaScript's number formatting, so it renders no such seq, even an underflowing spelling such as `-2.4703282292062328e-324` that both parsers read as `-5e-324`.
- `system-turn-float-lexeme`, `system-step-float-lexeme`, `start-seq-float-lexeme`, and `end-seq-float-lexeme`: as for row envelopes, negative spellings are rejected exactly.
- `envelope/` limits are row-envelope limits passed through. A -0 seq stays `envelope/negative-zero-seq` even though V3 rejects it, because the rejection follows V2 checks the envelope decoder does not complete.

The `fixture` section names the unchanged [request-reconstruction log](#runtime-request-reconstruction). Both harnesses decode its 16 rows of 12 types and check that payloads are borrowed; TypeScript checks its SHA-256 and `finish`, and Rust checks that each envelope equals the row-envelope decoder's. Each mutant replaces one fixture value in memory: an empty header `tools` list and a header `system` member are refused exactly, and an image system block is a Rust limit.

`source_budget` bounds expanded sources only; Rust does not claim that every TypeScript resource failure, such as allocating a huge range, becomes a limit. The parsed row is the caller's, so the parser bounds its size and nesting, if anything does.

## Log scan cases

[`session/log-scan-cases.json`](session/log-scan-cases.json) holds plain, uncompressed logs and the outcome of `scanLog` in [`format.ts`](../packages/session/session-persistence-jsonl/src/format.ts). The [TypeScript spec](../packages/session/session-persistence-jsonl/tests/log-scan-conformance.spec.ts) runs each log through the real function. The development [`bake-session`](../rust/crates/bake-session/src/scan.rs) crate passes the same bytes to `scan_log`. Neither reads a default Session file, which is Zstd-compressed.

```sh
bun run test:runtime packages/session/session-persistence-jsonl/tests/log-scan-conformance.spec.ts
(cd rust && cargo test --locked -p bake-session)
```

`scanLog` reads the bytes through the first LF as the [header record](#session-header-cases); a log without an LF is refused. Each later LF ends one event record, numbered from line 1; CR is JSON whitespace, and the bytes after the last LF are a torn tail that is never parsed. Each record runs these steps in its default recoverable mode:

1. `JSON.parse` of the record decoded as UTF-8. A parse failure is an unparsable issue.
2. [Raw-row admission](#v3-row-cases), even after an issue. A refusal is fatal: `SessionFormatError`, or `SessionFormatUnsupportedError` for an unsupported row.
3. After an issue, an object whose `type` is `turn/end` throws the first issue as a plain `Error`, and any other record is ignored.
4. Otherwise the strict V3 codec decodes the row. A rejection is an invalid issue, thrown at once when the row is a `turn/end`. A decoded row extends the event prefix and the committed byte offset, LF included.

`finish` then throws `SessionFormatError` for a seeded header without an inherited `session/end-seed` marker or an unseeded one with a marker, and otherwise returns the cut: the seq of the last marker, 0 for an unseeded log. The released v2 decoder records a marker before the V3 event checks run, so a marker row that passes the v2 checks and then fails a V3 check, such as one carrying `surfaceOp`, still sets the cut, although it ends the prefix. A marker after an issue, or one that fails a v2 check such as its seq, sets nothing.

Each case has an `id` and one log source: `lines`, each written with a final LF, and an optional unterminated `tail`; `bytesHex`; or `fixture`, which names the unchanged [request-reconstruction log](#runtime-request-reconstruction). `ts` is `scanned`, with the header metadata, the event count, the inherited cut, and the committed bytes, or `thrown` with the exact error class and message. A scanned case's events are its leading records as `JSON.parse` reads them, with `sources` giving expanded `sourceEventSeqs` where a row spells a range. `numbers` gives the exact float64 bits of named values: TypeScript reads them from the events, and Rust reads `Number::as_f64` of its parsed rows, so -0 and rounding are compared without text. Rust also checks its rows, each decoded envelope, and that payloads are borrowed.

A `rust` override replaces the TypeScript outcome for Rust:

- `thrown-class`: Rust claims the class but renders no message, as for two unexpected V3 fields.
- `native-subset` names a limit, which claims nothing about the record or any later one. A limit refuses the scan even after an issue, where TypeScript ignores the record or rethrows the issue at a `turn/end`; the table witnesses each limit on logs TypeScript scans and on logs it throws for.
  - `invalid-utf8`: Node decodes invalid UTF-8 with replacement characters; Rust does not.
  - `json-parser`: serde_json refuses a lone surrogate escape, nesting deeper than 128, or a number outside the `f64` range, all of which `JSON.parse` accepts.
  - `number-lexeme`: a number's integer part has more than 768 digits. serde_json 1.0.151 keeps 768 significant digits and treats any further digit as nonzero, without trimming an integer part's trailing zeros, so it rounds an exact halfway value such as `9007199254740993` followed by 784 zeros and `e-784` up, where `JSON.parse` rounds to even. Rust refuses every such integer part, whatever its value. Digit strings, fractions, and exponent digits of any length are not limited.
  - `codec`: a [V3 row](#v3-row-cases) limit, from raw-row admission or the codec.

The workspace enables serde_json's `float_roundtrip` feature, its correctly rounded decimal parser. The table pins -0, an underflow, a halfway tie, a 17-digit decimal the default parser misrounds, long fractions and exponents, and integers beyond 2^53, which compare as their nearest `f64`; agreement on other lexemes is not proven. Of the 76 cases, 50 scan and 26 throw; 17 have a native limit and one is class-only.

## Request derivation cases

[`runtime/request-derivation-cases.json`](runtime/request-derivation-cases.json) holds Session logs and the outcome of the TypeScript test helper `replayRequests` in [`runtime-fixture.ts`](../packages/core/agent-loop/tests/runtime-fixture.ts) for each. The [TypeScript spec](../packages/core/agent-loop/tests/request-derivation-conformance.spec.ts) runs every log through that helper and `normalizeRequests`. The development [`bake-session`](../rust/crates/bake-session/src/replay.rs) crate passes the same bytes to `replay_requests`; only its test normalizes message IDs.

```sh
bun run test:runtime packages/core/agent-loop/tests/request-derivation-conformance.spec.ts
(cd rust && cargo test --locked -p bake-session)
```

The helper does not restore its log. For a log already in format 3, [`scanLog`](#log-scan-cases) runs the strict V3 codec on each row and its `finish` checks; the catalog's transformed validation leaves current input unchanged. The helper then refuses bytes after the committed prefix and a seeded log, and builds a Session from the prefix that ends before each Assistant settlement of each step: both `assistant/message` and `assistant/attempt` supply cutoffs, including interrupted messages, so a step yields one request per recorded settlement. A step with no recorded settlement is refused. Session construction snapshots each prefix event as lossless JSON, then checks its envelope, message, settlement, request-header, and tool-update fields, its surface transition, including the rule that a replacement starting at a `system/message` in node 0 must be one `system/message` over exactly that node, and each tool update's references. Agreement covers that pipeline: codec admission, Session construction checks, and request derivation. The helper skips full restoration (`restoreReleasedV3Artifact`) and with it the restored vocabulary, turn and step relationships, tool lifecycles, compaction records, and restoration's stricter protected-first-head rules, so it accepts an unknown required event type and some logs restoration refuses, such as a `compaction/prune` that lists the system head. Rust instead admits only the 60 known event types of [`known-event-types.ts`](../packages/core/session/src/known-event-types.ts), less two. A derived request is not restored Session state, and admitting a known log-only record, such as a compaction, hook, or title record, does not establish that it is valid.

Rust derives requests only inside a closed subset and refuses everything else as a native limit:

- the known event types other than `image/offload`, which needs a message projection, and `session/end-seed`, which belongs to seeded logs; unknown types, required or ignorable, and any `ignorable` row are limits. A unit test pins the 60-name list against the generated TypeScript file;
- one `step/start` per turn and step, with safe-integer coordinates on it and on each settlement. Several settlements of one step are admitted;
- `request/header` rows whose `config` members are `LlmCallConfig` members and whose `tools` are absent or an array of objects;
- projected prefix payloads, those of the surface messages, request headers, and tool updates, holding only safe integers other than -0 and nesting arrays and objects at most 64 containers deep, because their values reach request JSON. When one payload breaks both rules, which of the two limits is reported is unspecified. Other payloads reach no request, so any number except -0 and any depth the scan parsed is admitted, as Session construction admits them.

Checks run in the helper's order: the scan, uncommitted bytes, a seeded header or nonzero cut, and then the Session construction checks on each prefix. Rows at or after the last cut are never checked by Session construction, as in the helper, so a codec-admitted replacement there that Session construction would refuse does not refuse the log. A codec-invalid or unparsable row anywhere still refuses it before any prefix is checked: the scan throws at a later `turn/end`, or the row's bytes stay uncommitted. Within a prefix, each event's lossless snapshot runs first: Session construction refuses -0 and non-finite numbers anywhere in the event, its markers included. The scan already refuses a number outside the `f64` range as a native limit, and serde_json's `float_roundtrip` parsing keeps the sign of every zero, including an underflowing spelling such as `-1e-400`, so Rust's -0 check over the whole row is exactly that refusal (`seed/lossless-json`). For the surface types and every known non-surface type the codec classifies, the codec already proves the envelope, the surface marker with an exact replacement shape and earlier endpoints, the event-local source rules, and the canonical request-header and tool-result rules that Session construction repeats, so Rust does not repeat them. A unit test shows the codec refusing both markers on each of those 50 non-surface types. It treats six known types as opaque: `request/tool-update`, `deliverables/presented`, `image/offload`, `subagent/catalog`, `subagent/routing-decision`, and `workspace/changes`. Their markers reach Session construction, which refuses either one on a type that is not surface-eligible (`seed/non-surface-marker`); Rust runs all of the tool update's Session checks. Session construction applies no other type-specific check to known log-only types except an `assistant/attempt`'s settlement fields, a safe-integer `turn` and `step` and an array `stream` (`seed/settlement`), and derivation does not either. An attempt is not a surface node, and its `stream` is never read beyond the -0 check. A subset limit may fire before a check that TypeScript would fail, because a limit claims nothing.

Every shipped-profile Session snapshot under `snapshots/session/` carries such log-only records, including the permission, sandbox, and approval knobs and session titles. Those snapshots are logical and scrubbed, without `seq`, `time`, or real tool and system text, so this survey establishes their event vocabulary, not byte-replay outcomes for those snapshots.

Session construction keeps an ordered list of current surface nodes. An empty `system/message` or `assistant/message` derives no request message but keeps its node, so an empty system head at node 0 stays protected and an empty Assistant message can still be shadowed. A replacement is checked against the current nodes before any of them change, by the replacement checks of `planSurfaceEvent` that remain for this subset, in its order; the codec already proved sequence contiguity and the event's own surface metadata, and message projections are outside the subset:

1. Its `startSeq` and then its `endSeq` must be current nodes. Shadowed and log-only seqs are not. Positions, not seq values, decide the range, so a range may open at a replacement node whose seq exceeds its end seq.
2. The start node must not sit after the end node.
3. The decoded `sourceEventSeqs`, after range expansion, must include every shadowed node; extra earlier sources are allowed. An Assistant message cannot carry sources, so an Assistant replacement in a checked prefix fails here once its endpoints pass. As the final settlement, it is outside every prefix and is not checked.
4. A `tool/result` replacement must shadow exactly one current `tool/result` and leave every member outside its result block's `content` equal, compared as unordered JSON members: data members such as `turn`, `step`, and `error`, message members such as `id` and `source`, and block members such as `isError`. A `null` member differs from an omitted one.
5. A replacement whose range starts at a `system/message` head must be one `system/message` over exactly that node. Later system nodes and a non-system head are unprotected.

The replacement then takes the place of the shadowed nodes, and requests carry its logged message.

A request's config and `tools` come from the latest `request/header` in its prefix. Its `toolHistory` is the snapshot of `ToolHistoryProjection` in [`tool-history.ts`](../packages/core/session/src/tool-history.ts) over the prefix. A tool update is checked in Session construction's order:

1. Its data: a safe-integer `headerSeq` of at least 0, a non-empty `afterMessageId`, and `additions` and `removals` of unique non-empty strings, with at least one name and none in both. This message is not wrapped as an invalid seed event.
2. No `surfaceOp` or `sourceEventSeqs`. The codec admits a `surfaceOp` of any value on this opaque type and still decodes `sourceEventSeqs` as on every row, so a malformed source list is a codec rejection. Neither marker's value is read, and a marker on a row after the last cut is never checked.
3. `headerSeq` names an earlier `request/header`, which is the latest one, with no other tool update after it.
4. Another header precedes that one.
5. The additions are the referenced header's tool names missing from the header before it, in the referenced header's order, and the removals are the earlier header's names missing from it, in the earlier order. Repeats count, and only a string name matches.
6. The last current node that derives a non-system message is the named `user/message` or `tool/result`, after any replacements.

A header resets the history to its own tools when there is no history yet, its reason is `series`, it starts a series, a tool's `JSON.stringify` text differs from its last declaration under the same name, or the previous header's tools were not all available. An update to the reset header is then ignored. A request whose active tools are not all available, such as after a header change with no update, gets those tools and no updates. Tool names are JavaScript `Map` keys: `1` and `"1"` differ, an absent name is one key, and an object name matches only itself. Schema text comparison follows JavaScript's member order, with array-index keys first, so Rust reads member order from its parsed rows and makes no claim about the log's bytes.

Each case has an `id` and a `log`:

- `"fixture"` with `edits`: at most eight changes to the unchanged request-reconstruction log, applied in memory. An edit replaces the `header` record, replaces a `row` with exact `text`, sets or removes the value at a JSON `pointer` without `~` escapes in a `row`, truncates to the first `truncate` rows, or appends an unterminated `tail` after the final LF. Unedited rows keep their exact text. A pointer edit re-serializes its row with `JSON.stringify`, so its `value` may hold only safe integers other than -0; cases that depend on a number's spelling replace the row's text. Rust re-serializes with serde_json and requires the whole row to hold only safe integers and no array-index keys, so both harnesses build the same bytes.
- A list of lines: a header record and rows written for the case.

`ts` is the helper's outcome. `requests` lists the normalized requests, either explicitly or as pointer edits that set a `value`, `insert` an array item, or `remove` a member of the fixture's [`expected-requests.json`](runtime/request-reconstruction/tool-call-turn/expected-requests.json). `rejected` carries the exact error message, or the `TypeError` class without a message. The spec normalizes message IDs only after the helper returns, and leaves tool-update `afterMessageId` values as logged, so a message ID that is not a generated UUID fails the case rather than passing as a rejection. Expected requests were written from the case's change and then checked against the helper; rejection messages were recorded from it.

A `rust` override replaces the TypeScript outcome for Rust:

- `native-subset` names a limit. Rust claims nothing about the case. The `header` and `codec` limits pass through the [header reader](#session-header-cases) and [V3 row](#v3-row-cases) native-subset outcomes.
- `rejected` names the cause Rust reports: `header`, `codec` for a scan rejection of a row, `finish`, `uncommitted`, `seeded`, a `seed/` Session construction check, `no-later-settlement`, or `no-request-header`. It is allowed only where the helper rejects. The `codec`, `finish`, `uncommitted`, and `seeded` causes claim the helper's exact message; the others claim only that the helper throws.

Both harnesses pin the case count, reject unknown keys, and require every limit and cause to be witnessed, with limits covering both accepted and rejected input. The TypeScript spec checks both fixture files' SHA-256; Rust, which has no hash dependency, checks their sizes. Of the 184 cases, 82 agree on requests, including a three-turn log written for the table, a message with an own `__proto__` member, two-step logs written for replacement sequences, compaction records, header changes, tool updates, released knob, title, and hook records with fractional, rounded, or deeply nested payloads, attempt settlements, and numbers after the last cut that the prefix qualification never reads; 86 are rejections with a Rust cause, including torn tails, recovered invalid rows, -0 in log-only rows and markers, markers on codec-known types (a scan refusal) and on codec-opaque ones (a Session refusal), and malformed attempts; and 16 are native limits, 10 of them on logs the helper accepts. One limit, `tools: null`, witnesses the helper's `TypeError` during request derivation. Requests compare as JSON values: equal values do not prove equal provider wire bytes or member order.

The helper emits a request for a settlement after a step's `assistant/message`, which the loop never writes, and leaves the last settlement row unchecked because no prefix contains it; Rust does the same. A settlement whose coordinate has no `step/start` cuts nothing. The rule is the helper's: it does not validate step and turn lifecycles or a global dispatch order for interleaved coordinates. In `assistant-attempt`, an attempt and a message both settle step 1.1, so the case yields three requests, the second repeating the first.

## Plain log restoration cases

[`session/restore-cases.json`](session/restore-cases.json) holds plain current-format logs and the state the production read path restores from each. The [TypeScript spec](../packages/session/session-persistence-jsonl/tests/restore-conformance.spec.ts) composes that path in its `restorePlainLog` helper: `scanLog`, `validateStoredEvents`, `interruptedTurnClosers`, then `Session.fromRestore` with the current message projections, as `readColdSessionLog` and `SessionStore.prepare` do. The development [`bake-session`](../rust/crates/bake-session/src/restore.rs) crate passes the same bytes to `restore_plain_log`.

```sh
bun run test:runtime packages/session/session-persistence-jsonl/tests/restore-conformance.spec.ts
(cd rust && cargo test --locked -p bake-session)
```

A current-format read runs no `restoreReleasedV3Artifact`, so neither harness checks turn, step, or tool lifecycles, and neither claims Agent resume: nothing truncates a torn tail, writes a closer or end seed, or checks a file's path or stored identity. The stages run in this order, and an earlier stage's refusal wins:

1. The scan, which accepts a torn or recovered tail and leaves it out.
2. A pass over every event refusing an unknown type without `ignorable` or a `request/header` whose reason is `fallback` (`SessionFormatUnsupportedError`).
3. A pass adopting every event (`SessionPersistenceCorruptionError`). Adoption checks message shapes, model and tool sources, tool-update data, and markers on non-surface types, but not settlements, header config, reason or series fields, or surface transitions, so a later adoption failure beats an earlier event that only Session construction refuses. Two cases witness that inversion.
4. The closers for a turn the writer left open: an error `tool/result` per pending call in insertion order, then `step/end` when a step is open, then `turn/end {interrupted}`, with continuing seqs and the last row's time. A repeated call id keeps its position and forgets its `tool/call`.
5. Session construction over the stored events and then the closers, without a lossless snapshot, so -0 restores; Rust limits it in projected payloads. It appends an ordinary `session/end-seed` unless the last event is one; its timestamp is the current time, so both harnesses report only whether it is appended.

`ts` is the restored state, written from the TypeScript sources and the committed captures before either harness ran: the header, inherited count, committed bytes, stored event count, full closers, whether an end seed is appended, `deriveMessages`, the canonical request header, the tool-history snapshot, and the latest `request/context` as spread. `{"$log": pointer}` and `{"$closer": pointer}` name a value in the edited rows or the closers, so IDs and times stay raw; there is no normalizer. `events` checks decoded `sourceEventSeqs`, which expand packed ranges, while the stored row keeps its packed field. A rejection carries the helper’s class and exact message, or only its `TypeError` class. The file backend adds path context to refusals and wraps scan errors as `SessionPersistenceCorruptionError`; this table does not claim those wrapper messages. Each case edits one of the three [runtime captures](#runtime-request-reconstruction) in memory: it truncates rows, replaces the header, a row, or one exact substring of a row, appends a row, or adds an unterminated tail.

A `rust` override names a `native-subset` limit, which claims nothing, or a `rejected` cause: `scan`, which claims the exact message, or an `unsupported/`, `stored/`, or `restore/` check, which claims the layer and refused seq. Rust limits `image/offload`, every known type carrying `ignorable` (TypeScript accepts a hook result and a user message, and refuses a tool update, all witnessed), `request/context` data that is not an object, closers whose computation would depend on JavaScript coercion (`null` data, a `null` content block, a non-string pending call id, or an open turn or step that is not a safe count), and the request derivation subset's number, depth, coordinate, config-member, and tool-schema rules on projected payloads, including fields no output carries, such as usage. Unknown types carrying `ignorable` restore as opaque rows, markers included.

Both harnesses pin the case count, reject unknown keys, and require every limit and refusal layer to be witnessed. Of the 57 cases, 29 restore identically, 13 are rejections with a Rust cause, and 15 are native limits, 11 of them on logs TypeScript restores. Nine restored cases, the three captures, a cut needing a closer, a torn and a recovered tail, a balanced end seed, and two seeded cuts, also run through the real JSONL backend and `readColdSessionLog` in a temporary store, and must match the table. Rust also checks that request derivation differs on the same bytes where its prefix rules do.

## Runner contract

The [TypeScript arm](../scripts/rust-conformance/runner.ts) and [Rust arm](../rust/crates/bake-conformance/src/lib.rs) independently implement a small test protocol. Each reads one UTF-8 JSON input from stdin to EOF. It validates the entire document before writing, applies permitted writes in order, and prints one JSON observation followed by LF. Invalid input exits 2; an I/O failure exits 1. File writes are not transactional: an earlier write can remain after a later I/O failure, and that failed run cannot pass the harness.

The input schema is `bake/synthetic-conformance/input`, version 1. It contains prompt strings, event objects, permission records, and write requests. Permission decisions are supplied fixture data, rather than decisions made by Bake's policy engine. The output schema is `bake/synthetic-conformance/observation`, version 1, containing the relayed prompts, events, and permissions. This is a test protocol, not a Session or provider protocol.

The [TypeScript validator](../scripts/rust-conformance/fixture.ts) and Rust parser enforce the same bounds: 256 KiB per document, 64 entries per vector, 64 KiB per text or file payload, 200 UTF-8 bytes per relative path, and 32 nested event containers. File payloads use hex, so their encoded text can occupy twice the byte limit. JSON numbers use plain decimal integer tokens in the safe integer range; negative zero, fraction and exponent forms, duplicate object keys, BOMs, and unpaired surrogates are rejected. Paths exclude traversal, absolute forms, Windows-invalid characters and device names, and trailing dots or spaces.

## Ownership and limits

Each run owns a private home, temporary directory, workspace, and child process. Children receive a minimal environment rather than inherited model credentials. The driver bounds process time and output, waits for child closure and stdin completion, and removes the run's directories. A failed or canceled stdin write fails the run even when the child exits 0. Workspace observation visits at most 1,024 entries and reads at most 256 regular files, each bounded to 64 KiB. On POSIX, timeout, overflow, and cancellation stop the owned process group; on Windows, they stop the direct child. Both supplied runners are single processes.

The harness runs trusted fixtures and executables. It provides no sandbox boundary for a hostile runner. File comparison rejects symlinks and special files; it does not qualify empty-directory semantics, permissions, or filesystem modes. The synthetic driver performs no identity normalization. Runtime fixtures and live models require their own evidence. The Rust workspace and these fixtures remain development files, outside the shipped 0.3 launcher and release archives.
