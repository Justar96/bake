---
description: "Pure Ink presentation of committed session rows, live output, pending input, and human interactions."
kind: "package-library"
---

# @dsh-tui/ui

English | [中文](README.zh.md)

## Summary

This library renders terminal state supplied by `@dsh-tui/app`. `App` displays committed history, live response text, pending input, status, and one human request. Components use typed locale dictionaries and callbacks. They do not access Cordis, Node services, storage, or a clock.

## Table of Contents

- [Use this package](#use-this-package)

- [Understand the implementation](#understand-the-implementation)

- [Model Experience](#model-experience)

- [Known Limitations](#known-limitations)

<a id="use-this-package"></a>

## Use this package

Import `App` and `AppProps` from the package entry point. Supply authoritative state and callbacks as demonstrated by the [runner](../app/src/runner.ts) and [component tests](tests/shell.spec.tsx). `project` maps a committed Session event to zero or more rows; `formatRow` provides a plain-text representation. Supply `completion` with session-owned command/skill metadata and discovery status, plus a positive integer `completionLimit` for visible menu rows. The menu filters a leading slash token, inserts the selected name on Tab, and closes on Escape before cancellation reaches the application. Paste only edits the draft. This is a library dependency, with no Cordis mount row.

Supply query-tagged `files` and `onReferenceQuery` for workspace-path discovery. The callback receives the active query or `undefined` when the menu closes. Only results matching the current query can be selected. Pure `activeAtToken` and `formatFileMention` imports from the Harness grammar own token detection and quoting; file Tab completion preserves surrounding text and directory Tab completion keeps discovery open.

The composer supports cursor insertion, grapheme movement and deletion, logical-line Home/End, and Shift-Enter for newlines. Enter submits the full draft. Up/Down browses human input when completion is closed; Ctrl-P/N recalls history while a menu is open. Returning past the newest entry restores the original draft and cursor. See the [keyboard controls](../app/README.md#use-this-package) for completion and editing behavior.

Supply `attachments` with staged metadata; pending inputs and user rows can also carry attachment summaries. The UI renders names, byte lengths, and available image metadata, without reading bytes or paths. `onSubmit` may return `false` or a promise: refusal/rejection preserves the draft and cursor, while successful asynchronous acceptance clears them. Pending acceptance suppresses edits, paste, recall, and repeated Enter, including keys decoded in the same read; Escape and Ctrl-C remain active. An empty draft can submit when staged attachments exist.

Use `dictionaries.en` or `dictionaries.zh` for application labels. Model text, tool output, question details, and service diagnostics remain verbatim. Build `committed` from `emptyTranscript` and `appendTranscript(previous, rows)`. Snapshots and their row batches must remain immutable and append-only because Ink `Static` prints each suffix once. `transcriptRows(snapshot, start)` reads only the suffix following a previously rendered count; streaming updates do not scan committed rows. Changing `sessionId` remounts session presentation, resets draft/cursor/recall and completion state, and prints a localized session heading before its history. Set `inputBlocked` during navigation to suspend composer input while retaining Escape and Ctrl-C; choice interactions remain usable. Context estimates display a `~` prefix, and pending input includes the localized `/clear-pending` hint.

<a id="understand-the-implementation"></a>

## Understand the implementation

<details>

<summary>Input and presentation</summary>

Ink `usePaste` inserts text without actions; `useInput` handles typed keys. `useComposer` keeps same-read edits available before React paints. History visits lazily traverse pending human input and committed user rows, retaining local edits only for visited entries. Typing and rendering do not scan history, and recalling or editing an entry never mutates the transcript. Text questions and login prompts share cursor editing without a history source.

`InteractionView` renders complete questions and plan details, masks secrets, and submits exact option labels. A `select` interaction displays a filterable `Picker` using `completionLimit` rows. Up/Down moves the selection; Enter returns one exact value, including the empty value used for provider defaults. Pasting filters without accepting, and no-match input stays editable. Request ids let the application reject stale callbacks.

See [the wiring reference](../../DESIGN.md) for ownership and [current dependency APIs](../../DEPENDENCIES.md) for the Ink and React versions. No independent runtime invariant is installed because these components only render their supplied props.

</details>

<a id="model-experience"></a>

## Model Experience

None directly; the application owns callbacks that submit logged messages and service answers.

### KV Cache effect

None; presentation does not construct model requests.

<a id="known-limitations"></a>

## Known Limitations and Deferred Work

- Inline terminal output has no virtual scrolling or rich attachment viewer.

- The composer has no mouse positioning, word movement, or vertical movement through wrapped lines; Up/Down browses history when completion is closed.

### Dev Note

None.
