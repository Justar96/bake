# Rust migration comparison harness

## Summary

Compare TypeScript and Rust using controlled fixtures and independently checked outcomes. The synthetic harness and [native eval fixture adapter](../evals/README.md#native-fixture-adapter) qualify comparison tooling for [migration scope 01](../docs/roadmap/rust-0.4/README.md#01--workspace-and-comparison-harness). Separate shared cases exercise Session headers, source references, row envelopes, strict V3 codec rows, and scans of plain logs against released codecs, and restoration of plain and Zstd-compressed bytes as the production read path restores them, and lookup of a Session by id in a root laid out on disk; three runtime fixtures capture a real TypeScript tool-call turn, a tool added, removed, and restored across turns, and a model request retried under a changed model, and request derivation cases replay the first and its variants through the TypeScript replay helper. The [v2→v3 cases](#v2-to-v3-migration-cases) compare a strict in-memory adjacent migration, stopping before whole-artifact validation. The [qualification ledger](../docs/roadmap/rust-0.4/ledger/README.md) records partial evidence. Agent resume, restoration of migrated logs, replay of seeded, resumed, or compressed logs, and live native evals remain open.

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
- [Zstd log restoration cases](#zstd-log-restoration-cases)
- [Session lookup cases](#session-lookup-cases)
- [Session metadata cases](#session-metadata-cases)
- [V2 to V3 migration cases](#v2-to-v3-migration-cases)
- [Token usage cases](#token-usage-cases)
- [Pending inbox and consumed-work cases](#pending-inbox-and-consumed-work-cases)
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

The helper does not restore its log. For a log already in format 3, [`scanLog`](#log-scan-cases) runs the strict V3 codec on each row and its `finish` checks; the catalog's transformed validation leaves current input unchanged. The helper then refuses bytes after the committed prefix and a seeded log, and builds a Session from the prefix that ends before each Assistant settlement of each step: both `assistant/message` and `assistant/attempt` supply cutoffs, including interrupted messages, so a step yields one request per recorded settlement. A step with no recorded settlement is refused. Session construction snapshots each prefix event as lossless JSON, then checks its envelope, message, settlement, request-header, and tool-update fields, its surface transition, including the rule that a replacement starting at a `system/message` in node 0 must be one `system/message` over exactly that node, and each tool update's references. Agreement covers that pipeline: codec admission, Session construction checks, and request derivation. The helper skips full restoration (`restoreReleasedV3Artifact`) and with it the restored vocabulary, turn and step relationships, tool lifecycles, compaction records, and restoration's stricter protected-first-head rules, so it accepts an unknown required event type and some logs restoration refuses, such as a `compaction/prune` that lists the system head. Rust instead admits only the 60 known event types of [`known-event-types.ts`](../packages/core/session/src/known-event-types.ts), less `session/end-seed`. A derived request is not restored Session state, and admitting a known log-only record, such as a compaction, hook, or title record, does not establish that it is valid.

Rust derives requests only inside a closed subset and refuses everything else as a native limit:

- the known event types other than `session/end-seed`, which belongs to seeded logs, with or without `ignorable`; unknown types, required or ignorable, are limits. A unit test pins the 60-name list against the generated TypeScript file;
- one `step/start` per turn and step, with safe-integer coordinates on it and on each settlement. Several settlements of one step are admitted;
- `request/header` rows whose `config` members are `LlmCallConfig` members and whose `tools` are absent or an array of objects;
- projected prefix payloads, those of the surface messages, request headers, and tool updates, holding only safe integers other than -0 and nesting arrays and objects at most 64 containers deep, because their values reach request JSON. When one payload breaks both rules, which of the two limits is reported is unspecified. Other payloads reach no request, so any number except -0 and any depth the scan parsed is admitted, as Session construction admits them.

Checks run in the helper's order: the scan, uncommitted bytes, a seeded header or nonzero cut, and then the Session construction checks on each prefix. Rows at or after the last cut are never checked by Session construction, as in the helper, so a codec-admitted replacement there that Session construction would refuse does not refuse the log. A codec-invalid or unparsable row anywhere still refuses it before any prefix is checked: the scan throws at a later `turn/end`, or the row's bytes stay uncommitted. Within a prefix, each event's lossless snapshot runs first: Session construction refuses -0 and non-finite numbers anywhere in the event, its markers included. The scan already refuses a number outside the `f64` range as a native limit, and serde_json's `float_roundtrip` parsing keeps the sign of every zero, including an underflowing spelling such as `-1e-400`, so Rust's -0 check over the whole row is exactly that refusal (`seed/lossless-json`). For the surface types and every known non-surface type the codec classifies, the codec already proves the envelope, the surface marker with an exact replacement shape and earlier endpoints, the event-local source rules, and the canonical request-header and tool-result rules that Session construction repeats, so Rust does not repeat them. A unit test shows the codec refusing both markers on each of those 50 non-surface types. It treats six known types as opaque: `request/tool-update`, `deliverables/presented`, `image/offload`, `subagent/catalog`, `subagent/routing-decision`, and `workspace/changes`. Their markers reach Session construction, which refuses either one on a type that is not surface-eligible (`seed/non-surface-marker`); Rust runs all of the tool update's Session checks. Session construction applies no other type-specific check to known log-only types except an `assistant/attempt`'s settlement fields, a safe-integer `turn` and `step` and an array `stream` (`seed/settlement`), and derivation does not either. An attempt is not a surface node, and its `stream` is never read beyond the -0 check. Session construction interprets a known type whether or not it carries `ignorable`, so an ignorable user message still contributes its message. The helper builds its Session without message projections, so an `image/offload` in a checked prefix is refused after its -0 and marker checks, however valid its decision (`seed/projection-required`); one after the last cut is never checked. A subset limit may fire before a check that TypeScript would fail, because a limit claims nothing.

Every shipped-profile Session snapshot under `snapshots/session/` carries such log-only records, including the permission, sandbox, and approval knobs and session titles. Those snapshots are logical and scrubbed, without `seq`, `time`, or real tool and system text, so this survey establishes their event vocabulary, not byte-replay outcomes for those snapshots.

Session construction keeps an ordered list of current surface nodes. An empty `system/message` or `assistant/message` derives no request message but keeps its node, so an empty system head at node 0 stays protected and an empty Assistant message can still be shadowed. A replacement is checked against the current nodes before any of them change, by the replacement checks of `planSurfaceEvent` that remain for this subset, in its order; the codec already proved sequence contiguity and the event's own surface metadata, and derivation has no message projections:

1. Its `startSeq` and then its `endSeq` must be current nodes. Shadowed and log-only seqs are not. Positions, not seq values, decide the range, so a range may open at a replacement node whose seq exceeds its end seq.
2. The start node must not sit after the end node.
3. The decoded `sourceEventSeqs`, after range expansion, must include every shadowed node; extra earlier sources are allowed. An Assistant message cannot carry sources, so an Assistant replacement in a checked prefix fails here once its endpoints pass. As the final settlement, it is outside every prefix and is not checked.
4. A `tool/result` replacement must shadow exactly one current `tool/result` and leave every member outside its result block's `content` equal, compared as unordered JSON members: data members such as `turn`, `step`, and `error`, message members such as `id` and `source`, and block members such as `isError`. A `null` member differs from an omitted one.
5. A replacement whose range starts at a `system/message` head must be one `system/message` over exactly that node. Later system nodes and a non-system head are unprotected.

The replacement then takes the place of the shadowed nodes, and requests carry its logged message.

A request's config and `tools` come from the latest `request/header` in its prefix. Its `toolHistory` is the snapshot of `ToolHistoryProjection` in [`tool-history.ts`](../packages/core/session/src/tool-history.ts) over the prefix. A tool update is checked in Session construction's order:

1. Its data: a safe-integer `headerSeq` of at least 0, a non-empty `afterMessageId`, and `additions` and `removals` of unique non-empty strings, with at least one name and none in both. This message is not wrapped as an invalid seed event.
2. No `surfaceOp` or `sourceEventSeqs`. The codec admits a `surfaceOp` of any value on this opaque type and still decodes `sourceEventSeqs` as on every row, so a malformed source list is a codec rejection. Neither marker's value is read, and a marker on a row after the last cut is never checked.
3. No `ignorable` marker: a tool update must be required on read (`seed/tool-update-required`).
4. `headerSeq` names an earlier `request/header`, which is the latest one, with no other tool update after it.
5. Another header precedes that one.
6. The additions are the referenced header's tool names missing from the header before it, in the referenced header's order, and the removals are the earlier header's names missing from it, in the earlier order. Repeats count, and only a string name matches.
7. The last current node that derives a non-system message is the named `user/message` or `tool/result`, after any replacements.

A header resets the history to its own tools when there is no history yet, its reason is `series`, it starts a series, a tool's `JSON.stringify` text differs from its last declaration under the same name, or the previous header's tools were not all available. An update to the reset header is then ignored. A request whose active tools are not all available, such as after a header change with no update, gets those tools and no updates. Tool names are JavaScript `Map` keys: `1` and `"1"` differ, an absent name is one key, and an object name matches only itself. Schema text comparison follows JavaScript's member order, with array-index keys first, so Rust reads member order from its parsed rows and makes no claim about the log's bytes.

Each case has an `id` and a `log`:

- `"fixture"` with `edits`: at most eight changes to the unchanged request-reconstruction log, applied in memory. An edit replaces the `header` record, replaces a `row` with exact `text`, sets or removes the value at a JSON `pointer` without `~` escapes in a `row`, truncates to the first `truncate` rows, or appends an unterminated `tail` after the final LF. Unedited rows keep their exact text. A pointer edit re-serializes its row with `JSON.stringify`, so its `value` may hold only safe integers other than -0; cases that depend on a number's spelling replace the row's text. Rust re-serializes with serde_json and requires the whole row to hold only safe integers and no array-index keys, so both harnesses build the same bytes.
- A list of lines: a header record and rows written for the case.

`ts` is the helper's outcome. `requests` lists the normalized requests, either explicitly or as pointer edits that set a `value`, `insert` an array item, or `remove` a member of the fixture's [`expected-requests.json`](runtime/request-reconstruction/tool-call-turn/expected-requests.json). `rejected` carries the exact error message, or the `TypeError` class without a message. The spec normalizes message IDs only after the helper returns, and leaves tool-update `afterMessageId` values as logged, so a message ID that is not a generated UUID fails the case rather than passing as a rejection. Expected requests were written from the case's change and then checked against the helper; rejection messages were recorded from it.

A `rust` override replaces the TypeScript outcome for Rust:

- `native-subset` names a limit. Rust claims nothing about the case. The `header` and `codec` limits pass through the [header reader](#session-header-cases) and [V3 row](#v3-row-cases) native-subset outcomes.
- `rejected` names the cause Rust reports: `header`, `codec` for a scan rejection of a row, `finish`, `uncommitted`, `seeded`, a `seed/` Session construction check, `no-later-settlement`, or `no-request-header`. It is allowed only where the helper rejects. The `codec`, `finish`, `uncommitted`, and `seeded` causes claim the helper's exact message; the others claim only that the helper throws.

Both harnesses pin the case count, reject unknown keys, and require every limit and cause to be witnessed, with limits covering both accepted and rejected input. The TypeScript spec checks both fixture files' SHA-256; Rust, which has no hash dependency, checks their sizes. Of the 200 cases, 87 agree on requests, including a three-turn log written for the table, ignorable user and log-only rows, an `image/offload` and an ignorable tool update after the last cut, an `image/offload` in a log without steps, a message with an own `__proto__` member, two-step logs written for replacement sequences, compaction records, header changes, tool updates, released knob, title, and hook records with fractional, rounded, or deeply nested payloads, attempt settlements, and numbers after the last cut that the prefix qualification never reads; 86 are rejections with a Rust cause, including torn tails, recovered invalid rows, -0 in log-only rows and markers, markers on codec-known types (a scan refusal) and on codec-opaque ones (a Session refusal), malformed attempts, ignorable tool updates at each of their three checks, and `image/offload` rows that need a projection, including one whose fractional index is no number limit, behind an earlier row's failure, -0, a marker, or a step's missing settlement as the helper orders them; and 13 are native limits, 9 of them on logs the helper accepts. One limit, `tools: null`, witnesses the helper's `TypeError` during request derivation. Requests compare as JSON values: equal values do not prove equal provider wire bytes or member order. The table's `history` records its versions: version 2 moved three version-1 `rust` overrides, `ignorable-marker`, `ignorable-tool-update`, and `image-offload`, to their unchanged TypeScript outcomes, removed the `ignorable` limit, and added 16 cases, with source-derived expectations. Their provenance is recorded in the table.

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
5. Session construction over the stored events and then the closers, with the catalog's message projections and without a lossless snapshot, so -0 restores; Rust limits it in projected payloads. It appends an ordinary `session/end-seed` unless the last event is one; its timestamp is the current time, so both harnesses report only whether it is appended.

`ts` is the restored state, written from the TypeScript sources and the committed captures before either harness ran: the header, inherited count, committed bytes, stored event count, full closers, whether an end seed is appended, `deriveMessages`, the canonical request header, the tool-history snapshot, and the latest `request/context` as spread. `{"$log": pointer}` and `{"$closer": pointer}` name a value in the edited rows or the closers, so IDs and times stay raw; there is no normalizer. `events` checks decoded `sourceEventSeqs`, which expand packed ranges, while the stored row keeps its packed field. A rejection carries the helper’s class and exact message, or only its `TypeError` class. The file backend adds path context to unsupported-format refusals and wraps other scan failures as `SessionPersistenceCorruptionError`; this table does not claim those wrapper messages. Each case edits one of the three [runtime captures](#runtime-request-reconstruction) in memory: it truncates rows, replaces the header, a row, or one exact substring of a row, appends a row, or adds an unterminated tail.

Session construction interprets a known type whether or not it carries `ignorable`: a hook result, a user message, a request header and context, a replacement, a final end seed, an `image/offload`, and the `subagent/routing-decision` production writes as ignorable all restore, and a marker on a known ignorable log-only type is still refused. A tool update must be required on read; that refusal follows its data and marker checks, which adoption runs first, and precedes its reference checks. Adoption never checks `ignorable` or runs a projection, so a later adoption failure beats an earlier projection or required-on-read refusal. Only an unknown ignorable type is opaque.

The catalog's `image/offload` projection, `imageOffloadProjection` in [`projection.ts`](../packages/compaction/compaction-image-offload/src/projection.ts), changes current messages without changing their nodes, identities, or logged events. A marker on the event is an adoption refusal. Its data must hold only a non-empty `targets` array. Each target, in order, must hold exactly a `seq` and a non-empty `imageIndexes`, then name a seq not already named by the decision, a current surface node, and a `user/message` or `tool/result`, then list strictly increasing indexes; then it is projected before the next target is read, so an earlier target's projection error beats a later target's shape error. Numbers are the values `JSON.parse` reads: `5.0`, `5e0`, and `1e-400` are integers, while -0, fractions, and unsafe integers are refused, and `5` and `5.0` are duplicates. Indexes count every image depth-first through nested `tool-result` blocks, already offloaded ones included; string, number, and array blocks are kept and not counted, and only targeted messages are walked. A selected image already offloaded, or a selected index past the last image, rejects the decision; the missing-index message names the first unmatched index. Projection starts from the node's previous projection, keeps every block's members in place, appends or overwrites `offloaded: true`, and commits only when every target succeeds. Projected messages stay keyed by original seq across the inherited cut and the end seed; a replaced node is no longer a target, and a replacement's tool-result rule compares logged data. Tool-update anchors see the projected message, whose identity is unchanged.

A `rust` override names a `native-subset` limit, which claims nothing, or a `rejected` cause: `scan`, which claims the exact message, or an `unsupported/`, `stored/`, or `restore/` check, which claims the layer and refused seq. A `restore/image-offload-` cause also claims the exact message. Rust limits an `image/offload` walk that reaches a `null` block or a `tool-result` block whose `content` is not an array, where JavaScript throws a `TypeError` that Session construction wraps with engine text, `request/context` data that is not an object, closers whose computation would depend on JavaScript coercion (`null` data, a `null` content block, a non-string pending call id, or an open turn or step that is not a safe count), and the request derivation subset's number, depth, coordinate, config-member, and tool-schema rules on projected payloads, including fields no output carries, such as usage. Unknown types carrying `ignorable` restore as opaque rows, markers included.

Both harnesses pin the case count, reject unknown keys, and require every limit and refusal layer to be witnessed. Restored messages are also compared as serialized text, because value equality ignores member order. Rust checks that restoration returns the scanned rows unchanged; a separate fold unit test verifies that projections leave logged node payloads and identities unchanged. Of the 118 cases, 55 restore identically, 49 are rejections with a Rust cause, and 14 are native limits, 9 of them on logs TypeScript restores. Thirteen restored cases, the three captures, a cut needing a closer, a torn and a recovered tail, a balanced end seed, two seeded cuts, a single and a successive image projection, a projection on each side of a seeded cut, and an ignorable routing decision, also run through the real JSONL backend and `readColdSessionLog` in a temporary store, and must match the table. The table's `history` records its versions: version 2 removed the `event-type` and `ignorable` limits, moved four version-1 overrides to their unchanged TypeScript outcomes, and added 42 cases, and 19 further cases. The table records expectation provenance, including engine-specific V8 error text. Rust also checks that request derivation differs on the same bytes where its prefix rules do.

## Zstd log restoration cases

[`session/zstd-cases.json`](session/zstd-cases.json) holds 49 compressed inputs and fixed expectations. The [TypeScript spec](../packages/session/session-persistence-jsonl/tests/zstd-conformance.spec.ts) opens each through the production JSONL backend in its own temporary store, observes recovery metadata before any write-open, and restores its Session projections. The [Rust test](../rust/crates/bake-session/tests/zstd_cases.rs) calls `restore_zstd_log` on the same bytes. Successful cases compare the header, every stored row, inherited cut, physical truncation offset, recovered-row start, closers, end-seed decision, messages, request header, tool history, and request context. Rust also checks the independent expected committed plaintext offset, which the backend's stored-file view does not expose.

The cases cover complete and empty frames, records and UTF-8 split across frames, RAW and RLE blocks, checksummed compressed blocks produced with Node's production encoder settings, incomplete headers/blocks/checksums, rejected window sizes and block lengths, structural errors, and decoder failures. Error-order witnesses distinguish structural pre-scan, per-frame decode and feed, the complete-frame committed check, and the final inherited-marker check. Torn-frame output is discarded on a decoder error; successfully decoded torn plaintext still passes row admission and can fail on an issue followed by `turn/end`. A padded header fills the native decoder's 64 KiB output buffer exactly.

`RestoredLog::torn()` reports `truncate_to` in physical input bytes and `recovered_from` as an index into stored rows. The recovered rows remain stored once. For plain logs, that index is the stored row count because no rows are recovered from the discarded tail. `ScannedLog::committed_bytes()` always counts plaintext bytes; it is not a compressed-file offset.

Four cases exceed the explicit native plaintext budget while TypeScript accepts their logs. They cover the header, a complete batch, and a recovered tail, with a zero budget included; an exact-budget case succeeds. These refusals have no invented event seq and make no TypeScript precedence claim. Existing scan and restoration native limits still apply.

The first 43 expectations were authored from source before either new harness ran. Earlier independent decoder probes informed the construction and edge selection. Two additional expectations reuse the authored full state with Node-encoded bytes, including a torn checksum. A further source-authored case forces a decoder error after a full output buffer has already been emitted. Three later cases, an `image/offload` projection, one in a row recovered from a torn frame, and an ignorable routing decision, carry the restoration stages through compressed input; their raw-block frames encode hand-written plaintext. Both harnesses pin the case count and distinct IDs. The 76 plain-scan and 118 plain-restoration cases also exercise the extracted incremental scanner. These finite cases qualify the pinned libzstd 1.5.7 behavior; they do not establish compatibility with arbitrary future decoder versions, migrations, file ownership, writeback, or live Agent resume.

Run the focused comparison with `bun run test:runtime packages/session/session-persistence-jsonl/tests/zstd-conformance.spec.ts` and, from `rust/`, `cargo test --locked -p bake-session --test zstd_cases`.

## Session lookup cases

[`session/lookup-cases.json`](session/lookup-cases.json) holds 88 Session roots laid out on disk, the id requested from each, and the outcome of opening it, plus 15 `encodeSegment` and 19 `projectKey` vectors. The [TypeScript spec](../packages/session/session-persistence-jsonl/tests/lookup-conformance.spec.ts) builds each layout in its own temporary directory and opens the id through the production JSONL backend with `open(id, 'read')`, the call `readColdSessionLog` makes, then restores the read as `SessionStore.prepare` does. The [Rust test](../rust/crates/bake-cli/tests/session_lookup.rs) builds the same layout independently and runs the built [`bake-rs session inspect --root --id`](../rust/README.md#inspect-a-session-log). Both require every entry under the case directory, links included, to be unchanged afterwards. `defaults` names the budgets the Rust test passes when a case sets none. Both check the vectors against their own encoders.

```sh
bun run test:runtime packages/session/session-persistence-jsonl/tests/lookup-conformance.spec.ts
(cd rust && cargo test --locked -p bake-cli)
```

The backend's stages run in this order, and an earlier stage's refusal wins:

1. `layout`: every real project directory, symlinks skipped, is listed; a regular `*.jsonl` or `*.jsonl.zstd` file in one is the flat legacy layout, and a canonical generation of the other compression in any real Session directory is an encoding mismatch. An unrelated Session's damage is not read.
2. `lookup`: each project is probed for `<id>.jsonl.zstd` and `<id>.jsonl` by opening them, so a linked file counts, and `<project>/<id>` is listed through a symlink. The other compression there is a mismatch, and the numerically highest canonical generation is selected by name, whatever its file type; a selected directory or dangling link is read as it stands, never replaced by a lower generation. Matches in two projects are a duplicate, even when one is corrupt; none is not found, as is an absent root.
3. `generation`: a newer generation's first line or first Zstd frame alone is read, so later bytes, even a corrupt frame, are ignored: an absent file or a malformed line, retired policy fields, a version other than its file name's, then unsupported, which first converts the id with `String(id)` and can throw a `TypeError` for an object. An older generation is migrated in memory by TypeScript.
4. `scan`, `identity`, `restore`: the current generation is scanned, then its header id and the path its id and `cwd` name are compared with the selected file, by spelling and otherwise by `realpath` string equality, so a hard link elsewhere is not an alias, and only then are its events validated and restored. A corrupt body or a header of another format version beats a wrong id, and a wrong id beats an unknown event type.

`ts` gives the stage, a `reason` code, the kind (`invalid`, `unsupported`, or `not-found`), and the root-relative path the refusal names; a restored case gives the selected path, header id and `cwd`, stored event, closer, and message counts, inherited count, end-seed decision, and physical torn-tail metadata; a `failure` gives the stage and Node error code, and a `type-error` the stage. A Rust failure is checked against `nativeDiagnostic`, the diagnostic's text and the case-relative path it quotes, so an unrelated failure cannot pass; a native budget failure is marked `budget` instead of an error code. Messages are compared exactly where a case gives one. Reason codes name lookup and header outcomes; scan and restore refusals carry none. The TypeScript spec observes the `layout` and `lookup` stages by awaiting the backend's own root check and `findLog` before `open`, which must then reject the same way. The selected version separates the header-only read from the current read, whose stages are told apart by message.

Files are literal text, a [runtime capture](#runtime-request-reconstruction) with its header line replaced or rows edited, a [Zstd case](#zstd-log-restoration-cases)'s bytes, or Zstandard frames of raw blocks without checksums that both harnesses encode. The cases cover plain, Zstd, and default-Zstd roots, a recovered torn frame, Unicode ids and `cwd` values, `..` as an id, a project key cut at 251 units inside an escape, numeric generation order, ignored noncanonical names, root-level files, followed and skipped symlinks, realpath and case aliases, roots spelled `link/../root`, through a symlink, or with `.` segments, and each refusal above. A `rust` entry replaces the expectation where the preview reports a native limit or a budget: the entry, file byte, decoded Zstd plaintext, and source budgets at and one past their limits, a newer generation over the byte budget, which TypeScript refuses from its header, older generations, a newer header whose id is an object or array, and project or Session directory names that are not UTF-8. Node lists such a name with replacement characters and then opens that spelling, which can be absent (`ENOENT`, or a skipped Session directory) or another entry (`ENOTDIR`).

Windows lookup refuses encoded ids and traversed directory names ending in a dot or space or naming a reserved device as native limits, and directory enumeration treats all Windows reparse points as links, as Node does. The legacy-name probe can open directories on Windows as well as POSIX.

Every platform-limited case states its `platformReason`; symlink cases run on Linux and macOS only, because Windows needs a privilege to create links. Win32 `cwd` and backslash-root variants run on Windows, where each native CI job runs both the Rust lookup suite and the TypeScript lookup spec. A case alias, and an id differing only in letter case, expect one outcome on a case-insensitive filesystem and another on a case-sensitive one; each harness probes its temporary directory to choose. Linux CI exercises only the case-sensitive branch.

Directory order is unspecified: Rust visits entries in byte order and TypeScript in the operating system's. Each case holds one fault per stage, or faults whose order the source fixes, such as a legacy file before a mismatched generation in the same project, or a whole-root fault before a duplicate id. With faults in different directories, even the reported reason or the choice between refusal and failure can differ. Rust lists each directory once per stage where TypeScript lists the root twice. Neither harness takes an atomic snapshot of the root, and these cases claim no race behavior. Rust resolves a relative root lexically, as Node's POSIX `path.resolve` does, and `x/../root` and `./root/.` run on every platform. On Windows it uses `GetFullPathNameW` for relative and drive-absolute roots and refuses other spellings as native limits, which a Rust-only test checks there; no other Win32 spelling is compared with Node. Ids and roots are UTF-8, so lone surrogates are not covered; a Linux-only Rust test refuses a relative root under a working directory that is not UTF-8.

The expectations were written from the TypeScript sources before either harness ran. The first TypeScript run showed one authoring mistake: the frozen v0 fixture had been expected to migrate, but the backend refuses it as unsupported, as `jsonl.spec.ts` also pins. The table's `amendments` records the corrected expectation and the added v2 case that migrates. Table version 2 added the 25 cases listed in `reviewAdditions` after a review of the first Rust candidate; their expectations were written from the TypeScript sources before their first run, and no earlier expectation changed. Restored counts reuse the expectations the [restoration](#plain-log-restoration-cases) and Zstd tables already authored.

## Session metadata cases

[`session/generation-header-cases.json`](session/generation-header-cases.json) compares the pure native header reader with the TypeScript backend's public `stat(id)`. Its 95 cases cover v0–v3 headers, the presence of `seedLength`, preset translation, omitted and null fields, numeric and parser limits, platform-specific absolute paths, and refusal precedence. The [Rust test](../rust/crates/bake-session/tests/generation_header_cases.rs) checks both path platforms; the [TypeScript spec](../packages/session/session-persistence-jsonl/tests/generation-header-conformance.spec.ts) checks its host's behavior through the real backend in private temporary roots. A `rust` entry records a native limit without weakening the TypeScript expectation.

[`session/stat-cases.json`](session/stat-cases.json) adds 46 disk layouts for the built [`session stat` command](../rust/README.md#inspect-session-metadata). The [native test](../rust/crates/bake-cli/tests/session_stat.rs) and [TypeScript spec](../packages/session/session-persistence-jsonl/tests/stat-conformance.spec.ts) build each layout independently and compare the selected generation, header, absence, or refusal. They cover plain and Zstd headers, corrupt bodies that must be ignored, missing and malformed headers, generation and identity errors, symlinks, and both read budgets. Physical size is checked against the fixture file. The native test also verifies that a FIFO cannot block the command. Each platform-restricted case states why it is restricted.

```sh
bun run test:runtime packages/session/session-persistence-jsonl/tests/generation-header-conformance.spec.ts packages/session/session-persistence-jsonl/tests/stat-conformance.spec.ts
(cd rust && cargo test --locked -p bake-session --test generation_header_cases)
(cd rust && cargo test --locked -p bake-cli --test session_stat)
```

Expected outcomes were authored from the TypeScript source before either implementation ran. Both suites use the production `stat` path; they do not migrate event rows or open a Session for writing. The disk-layout tests compare input bytes, modification times, and link targets after execution, and the native command's private home must remain empty. The generation-header cases separately check unchanged file bytes. The native CI job runs these TypeScript comparisons alongside the Rust tests on each operating system.

Header translation does not qualify historical event migration, Session resume, pending in-process Sessions, writer leases, or atomic observation during concurrent changes. TypeScript has no corresponding read-budget limit. JavaScript representations outside the native reader's supported subset remain named limits, and skipped platform cases remain missing evidence. These comparisons close no roadmap scope.

## V2 to V3 migration cases

[`session/v2-to-v3-cases.json`](session/v2-to-v3-cases.json) holds 367 synthetic cases for the strict released v2 codec feeding the adjacent migration chain. The [TypeScript spec](../packages/session/session-format-v2-to-v3/tests/v2-to-v3-migration-conformance.spec.ts) calls those production APIs, finishes the decoder, then finishes the chain. The [Rust test](../rust/crates/bake-session/tests/v2_to_v3_cases.rs) passes the same parsed header and rows to `migrate_v2_rows` under both path platforms. TypeScript checks its host's path behavior; CI runs the spec on Linux, macOS, and Windows.

Successful cases compare the logical v3 header, transformed events, inherited cut, and object member order. They cover every released source event family, system-prompt promotion, deterministic message IDs and collisions, local reference remapping, delivery coordinates that remain unchanged, seeded history, and the owned PTC renames. Refusals compare the codec or migration layer, row or finish location, and message. Expectations come from executing TypeScript independently of Rust. The tests also check that inputs remain unchanged. Deliberately changing a generated ID, generated member order, or opaque negative zero fails both harnesses.

```sh
bun run test:runtime packages/session/session-format-v2-to-v3/tests/v2-to-v3-migration-conformance.spec.ts
(cd rust && cargo test --locked -p bake-session --test v2_to_v3_cases)
```

The API consumes parsed JSON values and leaves framing, compression, and parser limits to its caller. Numbers compare with JavaScript semantics, preserving negative zero; integer-to-integer comparisons remain exact. Rust refuses retained integer values outside JavaScript's safe range after the row's ordinary checks succeed. Native limits also identify undecided integer spellings, engine-specific or numeric diagnostic text, and expanded source lists that exceed the caller's budget. Each fixture with a native limit still asserts TypeScript's outcome. Opaque floating-point values are preserved, but this suite does not qualify a byte encoder.

The result stops before `restoreReleasedV3Artifact`, which validates relationships, protected system messages, and vocabulary across the complete transformed artifact. It therefore does not establish that TypeScript would open the migrated Session. Recoverable decoding, earlier adjacent migrations, historical plain or Zstd file reads, publication, and Agent resume remain separate work. These cases close no roadmap scope.
## Token usage cases

[`session/usage-cases.json`](session/usage-cases.json) holds 34 edited runtime captures and the token-usage state token-meter folds from each. The [TypeScript spec](../packages/llm/token-meter/tests/usage-conformance.spec.ts) folds `tokenUsageProjectionDefinition.init` and `apply` from [`usage-projection.ts`](../packages/llm/token-meter/src/usage-projection.ts) over each case's `JSON.parse`d rows and their `interruptedTurnClosers`, and requires `Session.fromRestore`, without message projections, to admit the same events. The [Rust test](../rust/crates/bake-session/tests/usage_cases.rs) restores the same bytes with `restore_plain_log`, requires every case to restore with no torn tail, and folds the result with [`token_usage`](../rust/crates/bake-session/src/usage.rs).

```sh
bun run test:runtime packages/llm/token-meter/tests/usage-conformance.spec.ts
(cd rust && cargo test --locked -p bake-session --test usage_cases)
```

Each Assistant settlement contributes one sample: an `assistant/message`'s own `usage` member when present, even `null`, otherwise the stream's last raw `usage` chunk, found as `lastAssistantStreamChunk` finds it, scanning backwards and stopping at the first hit, even one without a `usage` member. A sample for the coordinate the slot holds replaces it in the totals, an equal one changes nothing, and `llm/retry-started` with a strictly equal `turn` and `step` empties the slot, so the retried request adds. Missing or `null` cache counts are 0. `ts` is the folded `{totals, last}` state, or a `TypeError` and the seq of the event that throws it, written from the TypeScript sources and the committed captures before either harness ran. Each case edits one of the three [runtime captures](#runtime-request-reconstruction) as the [restoration table](#plain-log-restoration-cases) does, without header or tail edits.

A `rust` override names a native limit and the refused seq, which must equal a TypeScript throw's: `number` for a sampled count not spelled as a safe integer (a fraction, an exponent, or out of range), which only an unqualified `assistant/attempt` stream can hold, or a running total past the safe-integer range; `usage` for a sample that is not an object or whose counts JavaScript would coerce; `stream` for a `null` record, or a `chunk` record whose `chunk` is absent or `null`, reached before a `usage` chunk; and `retry` for `llm/retry-started` data that is `null`, or lacks `turn` while the slot is empty. Of the 34 cases, 22 fold identically, 8 are TypeScript throws, and 4 are limits on input TypeScript folds through coercion or rounding. Both harnesses pin the case count and require every limit to be witnessed. Two negative controls were observed: dropping the `llm/retry-started` reset, or taking the first usage chunk instead of the last, fails `retry-started-closes-slot` in both arms. The fold does not cover `contextPressure`, turn usage, or pricing.

## Pending inbox and consumed-work cases

[`session/inbox-cases.json`](session/inbox-cases.json) holds 63 edits of the three [runtime captures](#runtime-request-reconstruction) and, for each, the pending inbox and the consumed-work account folded over its restored events. The [TypeScript spec](../packages/core/agent-loop/tests/inbox-conformance.spec.ts) restores each log with local copies of the [restore spec's](#plain-log-restoration-cases) `restorePlainLog` and `caseLog` helpers, then folds the restored Session's events, which are the stored events, the closers, and any appended end seed, with `inboxProjectionDefinition.apply` from `init()` and with `foldConsumedWork`. Because that package does not depend on the format catalog, the copy restores without message projections. Those projections change only derived messages, and the spec checks that no case logs an `image/offload`. The development [`bake-session`](../rust/crates/bake-session/src/inbox.rs) crate passes the same bytes to `restore_plain_log`, then to `restored_inbox` and `consumed_work`, which read the stored events and the closers. Neither fold reads the end seed.

```sh
bun run test:runtime packages/core/agent-loop/tests/inbox-conformance.spec.ts
(cd rust && cargo test --locked -p bake-session --test inbox_cases)
```

Every case restores. `ts.inbox` is either the `next-turn` and `next-step` messages or the exact refusal `invalid persisted inbox splice at session seq N`. The fold wraps every error it throws in that message, `TypeError`s included, so a `null` message, a non-iterable `inserted`, and a target other than the two lists all refuse exactly. `ts.consumedWork` is the accounting `turn/end`, absent when none, and `droppedUnrun`, or the `TypeError` class that `foldConsumedWork` throws unwrapped. `{"$log": pointer}` and `{"$closer": pointer}` name a value in the edited rows or the closers.

The cases cover:

- the unedited captures, and logs cut before a claim, after a claim, and inside a step, where a closer's interrupted end does or does not account for the work;
- `start` and `removedCount` at and past each bound, negative, absent, `null` (which removes nothing yet still counts as a claim or a cancellation), a string, and an unsafe integer;
- duplicate pending ids within one insert and across targets, a reused id after a claim, and messages without an id;
- cancellations with and without `inserted`, a replacement that inserts, and a claim outside a turn;
- claimed-but-unstepped turns ending with each built-in reason and an unknown one, a stepped end without a reason, a claim that survives a stepped end, and turns whose value is a string, `null`, an object, or absent.

A `rust` override names a native limit and the seq it applies to, and claims nothing for that fold. Inbox limits are a non-string `target`, which JavaScript coerces to a property key; a count spelled with a fraction or exponent; a string `inserted`, which the spread splits into characters; and a numeric message id that is not a safe integer lexeme. Consumed-work limits are `null` data, an absent or `null` `inserted` read for a cancellation, and an absent or `null` reason for a claimed turn, each a `TypeError`, and an object `inserted`, whose `length` this port does not read. A further limit covers a turn number that is not a safe integer lexeme. Both harnesses pin the case count, reject unknown keys, and require every limit to be witnessed. The expectations were written from the two TypeScript sources before either harness ran. Open compactions and open children have no pure TypeScript fold, so this table does not cover them.

## Runner contract

The [TypeScript arm](../scripts/rust-conformance/runner.ts) and [Rust arm](../rust/crates/bake-conformance/src/lib.rs) independently implement a small test protocol. Each reads one UTF-8 JSON input from stdin to EOF. It validates the entire document before writing, applies permitted writes in order, and prints one JSON observation followed by LF. Invalid input exits 2; an I/O failure exits 1. File writes are not transactional: an earlier write can remain after a later I/O failure, and that failed run cannot pass the harness.

The input schema is `bake/synthetic-conformance/input`, version 1. It contains prompt strings, event objects, permission records, and write requests. Permission decisions are supplied fixture data, rather than decisions made by Bake's policy engine. The output schema is `bake/synthetic-conformance/observation`, version 1, containing the relayed prompts, events, and permissions. This is a test protocol, not a Session or provider protocol.

The [TypeScript validator](../scripts/rust-conformance/fixture.ts) and Rust parser enforce the same bounds: 256 KiB per document, 64 entries per vector, 64 KiB per text or file payload, 200 UTF-8 bytes per relative path, and 32 nested event containers. File payloads use hex, so their encoded text can occupy twice the byte limit. JSON numbers use plain decimal integer tokens in the safe integer range; negative zero, fraction and exponent forms, duplicate object keys, BOMs, and unpaired surrogates are rejected. Paths exclude traversal, absolute forms, Windows-invalid characters and device names, and trailing dots or spaces.

## Ownership and limits

Each run owns a private home, temporary directory, workspace, and child process. Children receive a minimal environment rather than inherited model credentials. The driver bounds process time and output, waits for child closure and stdin completion, and removes the run's directories. A failed or canceled stdin write fails the run even when the child exits 0. Workspace observation visits at most 1,024 entries and reads at most 256 regular files, each bounded to 64 KiB. On POSIX, timeout, overflow, and cancellation stop the owned process group; on Windows, they stop the direct child. Both supplied runners are single processes.

The harness runs trusted fixtures and executables. It provides no sandbox boundary for a hostile runner. File comparison rejects symlinks and special files; it does not qualify empty-directory semantics, permissions, or filesystem modes. The synthetic driver performs no identity normalization. Runtime fixtures and live models require their own evidence. The Rust workspace and these fixtures remain development files, outside the shipped 0.3 launcher and release archives.
