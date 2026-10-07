# Rust migration comparison harness

## Summary

Compare TypeScript and Rust using controlled fixtures and independently checked outcomes. The synthetic harness and [native eval fixture adapter](../evals/README.md#native-fixture-adapter) qualify comparison tooling for [migration scope 01](../docs/roadmap/rust-0.4/README.md#01--workspace-and-comparison-harness). Separate shared cases exercise Session headers, source references, row envelopes, and strict V3 codec rows against released codecs; a runtime fixture captures a real TypeScript tool-call turn, and request derivation cases replay it and its variants through the TypeScript replay helper. The [qualification ledger](../docs/roadmap/rust-0.4/ledger/README.md) records partial evidence. Rust Session restoration, replay outside a closed subset, and live native evals remain open.

## Table of Contents

- [Run the comparisons](#run-the-comparisons)
- [What is compared](#what-is-compared)
- [Shared fixtures](#shared-fixtures)
- [Runtime request reconstruction](#runtime-request-reconstruction)
- [Session header cases](#session-header-cases)
- [Source-event seq cases](#source-event-seq-cases)
- [Row-envelope cases](#row-envelope-cases)
- [V3 row cases](#v3-row-cases)
- [Request derivation cases](#request-derivation-cases)
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

The runtime suite exercises this fixture, and the ordinary TypeScript check includes its helper and spec. Rust reads its header record through the [Session header cases](#session-header-cases) and decodes its `sourceEventSeqs` fields and row envelopes through the [source-event seq cases](#source-event-seq-cases) and [row-envelope cases](#row-envelope-cases). It derives the fixture's two requests through the [request derivation cases](#request-derivation-cases), over a closed subset and without restoration. Provider wire encodings, historical formats, seeded/forked logs, compaction, retries, cancellation, changing request headers, profile composition, and cross-platform release qualification remain separate work.

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

A decoded row is codec output, not a restored event. Unknown types and ignorable obsolete types decode with any optional fields and an opaque payload, and other payloads stay unvalidated; restoration later requires an installed vocabulary and checks relationships between events. Known types are the surfaces, the released v2 disposition keys other than the obsolete types, `tool/ptc-dispatch-start`, `tool/ptc-dispatch`, `feedback/message-put`, `feedback/message-delete`, and the 12 `Object.prototype` names, which the frozen dispositions object inherits. The table's `vocabulary` lists them with near-miss opaque names. The TypeScript spec checks the lists against the real exports, Rust checks its constants against them, and both harnesses decode a row of every listed name. Recovery modes, `finish`, framing, and replay are not modelled.

`ts` is `decoded`, with the envelope as for row envelopes, or `rejected` with an exact `class`. `SessionFormatError` and `SessionFormatUnsupportedMigrationError` carry the exact message; `TypeError` carries none, because a `{"toString":null}` seq on an obsolete row throws with different messages in Bun and Node.

A `rust` override follows the row-envelope conventions. A class-only `rejected-class` lists `unexpectedKeys` when two or more keys are unexpected, and nothing for an unrendered seq gap. A `native-subset` override names a limit:

- `system-payload`: Rust claims the frozen validator's acceptance only for a source of exactly `kind` and a plugin other than `compact`, and content blocks that are each exactly `{type: "text" | "reasoning", text: <string>}`, an empty list included. Any other shape is a limit, including ones TypeScript accepts.
- `obsolete-seq-diagnostic`: an obsolete row's seq is an array, an object, an integer outside the safe range, or any number serde_json stores as an `f64`, -0 included. serde_json also reads an underflowing spelling such as `-2.4703282292062328e-324` as -0, where `JSON.parse` reads `-5e-324`, so Rust cannot render the seq from the parsed value. Both readings fail count and time validation, so this parser difference does not change their rejection.
- `system-turn-float-lexeme`, `system-step-float-lexeme`, `start-seq-float-lexeme`, and `end-seq-float-lexeme`: as for row envelopes, negative spellings are rejected exactly.
- `envelope/` limits are row-envelope limits passed through. A -0 seq stays `envelope/negative-zero-seq` even though V3 rejects it, because the rejection follows V2 checks the envelope decoder does not complete.

The `fixture` section names the unchanged [request-reconstruction log](#runtime-request-reconstruction). Both harnesses decode its 16 rows of 12 types and check that payloads are borrowed; TypeScript checks its SHA-256 and `finish`, and Rust checks that each envelope equals the row-envelope decoder's. Each mutant replaces one fixture value in memory: an empty header `tools` list and a header `system` member are refused exactly, and an image system block is a Rust limit.

`source_budget` bounds expanded sources only; Rust does not claim that every TypeScript resource failure, such as allocating a huge range, becomes a limit. The parsed row is the caller's, so the parser bounds its size and nesting, if anything does.

## Request derivation cases

[`runtime/request-derivation-cases.json`](runtime/request-derivation-cases.json) holds Session logs and the outcome of the TypeScript test helper `replayRequests` in [`runtime-fixture.ts`](../packages/core/agent-loop/tests/runtime-fixture.ts) for each. The [TypeScript spec](../packages/core/agent-loop/tests/request-derivation-conformance.spec.ts) runs every log through that helper and `normalizeRequests`. The development [`bake-session`](../rust/crates/bake-session/src/replay.rs) crate passes the same header record and parsed rows to `replay_requests`; only its test normalizes message IDs.

```sh
bun run test:runtime packages/core/agent-loop/tests/request-derivation-conformance.spec.ts
(cd rust && cargo test --locked -p bake-session)
```

The helper does not restore its log. For a log already in format 3, `scanLog` runs the strict V3 codec on each row and its `finish` checks; the catalog's transformed validation leaves current input unchanged. The helper then builds a Session from the prefix that ends before each step's first matching Assistant settlement. Session construction checks each prefix event's envelope, message, settlement, and request-header fields, and its surface transition, including the rule that only a `system/message` may replace the system head. Agreement covers that pipeline: codec admission, Session construction checks, and request derivation. The helper skips full restoration (`restoreReleasedV3Artifact`) and with it the restored vocabulary, turn and step relationships, tool lifecycles, and the protected-first-head rule, so it accepts an unknown required event type. Rust instead admits only its own 12 event types and refuses every replacement. A derived request is not restored Session state.

Rust derives requests only inside a closed subset and refuses everything else as a native limit:

- an unseeded header;
- the 12 event types of the request-reconstruction fixture, without `ignorable` markers;
- append-only surface events;
- one `step/start` and at most one Assistant message per turn and step;
- at most one `request/header`, whose `config` members are `LlmCallConfig` members and whose `tools` are absent or an array of objects;
- prefix payloads holding only safe integers and nesting arrays and objects at most 64 containers deep.

Checks run in the helper's order: the header record, every row through the V3 codec with the first refusal winning, and then the Session construction checks on each prefix. Rows at or after the last cut are never checked by Session construction, as in the helper. For rows of the 12 subset types, the codec already proves the envelope, surface marker, source, and canonical request-header and tool-result rules that Session construction repeats, so Rust does not repeat them. A subset limit may fire before a check that TypeScript would fail, because a limit claims nothing.

Each case has an `id` and a `log`:

- `"fixture"` with `edits`: at most eight changes to the unchanged request-reconstruction log, applied in memory. An edit replaces the `header` record, replaces a `row` with exact `text`, sets or removes the value at a JSON `pointer` without `~` escapes in a `row`, or truncates to the first `truncate` rows. Unedited rows keep their exact text. A pointer edit re-serializes its row, so its `value` may hold only safe integers other than -0; cases that depend on a number's spelling replace the row's text.
- A list of lines: a header record and rows written for the case.

`ts` is the helper's outcome. `requests` lists the normalized requests, either explicitly or as pointer edits that set a `value`, `insert` an array item, or `remove` a member of the fixture's [`expected-requests.json`](runtime/request-reconstruction/tool-call-turn/expected-requests.json). `rejected` carries the exact error message, or the `TypeError` class without a message. Expected requests were written from the case's change and then checked against the helper; rejection messages were recorded from it.

A `rust` override replaces the TypeScript outcome for Rust:

- `native-subset` names a limit. Rust claims nothing about the case. The `header` and `codec` limits pass through the [header reader](#session-header-cases) and [V3 row](#v3-row-cases) native-subset outcomes.
- `rejected` names the cause Rust reports: `header`, `codec`, a `seed/` Session construction check, `no-later-settlement`, or `no-request-header`. It is allowed only where the helper rejects, and it claims only that the helper throws, not its error class or message.

Both harnesses pin the case count, reject unknown keys, and require every limit and cause to be witnessed, with limits covering both accepted and rejected input. The TypeScript spec checks both fixture files' SHA-256; Rust, which has no hash dependency, checks their sizes. Of the 52 cases, 13 agree on requests, including a three-turn log written for the table and a message with an own `__proto__` member; 20 are rejections with a Rust cause; and 19 are native limits, 11 of them on logs the helper accepts. One limit, `tools: null`, witnesses the helper's `TypeError` during request derivation. Requests compare as JSON values: equal values do not prove equal provider wire bytes or member order.

## Runner contract

The [TypeScript arm](../scripts/rust-conformance/runner.ts) and [Rust arm](../rust/crates/bake-conformance/src/lib.rs) independently implement a small test protocol. Each reads one UTF-8 JSON input from stdin to EOF. It validates the entire document before writing, applies permitted writes in order, and prints one JSON observation followed by LF. Invalid input exits 2; an I/O failure exits 1. File writes are not transactional: an earlier write can remain after a later I/O failure, and that failed run cannot pass the harness.

The input schema is `bake/synthetic-conformance/input`, version 1. It contains prompt strings, event objects, permission records, and write requests. Permission decisions are supplied fixture data, rather than decisions made by Bake's policy engine. The output schema is `bake/synthetic-conformance/observation`, version 1, containing the relayed prompts, events, and permissions. This is a test protocol, not a Session or provider protocol.

The [TypeScript validator](../scripts/rust-conformance/fixture.ts) and Rust parser enforce the same bounds: 256 KiB per document, 64 entries per vector, 64 KiB per text or file payload, 200 UTF-8 bytes per relative path, and 32 nested event containers. File payloads use hex, so their encoded text can occupy twice the byte limit. JSON numbers use plain decimal integer tokens in the safe integer range; negative zero, fraction and exponent forms, duplicate object keys, BOMs, and unpaired surrogates are rejected. Paths exclude traversal, absolute forms, Windows-invalid characters and device names, and trailing dots or spaces.

## Ownership and limits

Each run owns a private home, temporary directory, workspace, and child process. Children receive a minimal environment rather than inherited model credentials. The driver bounds process time and output, waits for child closure and stdin completion, and removes the run's directories. A failed or canceled stdin write fails the run even when the child exits 0. Workspace observation visits at most 1,024 entries and reads at most 256 regular files, each bounded to 64 KiB. On POSIX, timeout, overflow, and cancellation stop the owned process group; on Windows, they stop the direct child. Both supplied runners are single processes.

The harness runs trusted fixtures and executables. It provides no sandbox boundary for a hostile runner. File comparison rejects symlinks and special files; it does not qualify empty-directory semantics, permissions, or filesystem modes. The synthetic driver performs no identity normalization. Runtime fixtures and live models require their own evidence. The Rust workspace and these fixtures remain development files, outside the shipped 0.3 launcher and release archives.
