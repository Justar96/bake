---
description: "Pure Ink presentation of committed session rows, live output, pending input, and human interactions."
kind: "package-library"
---

# @dsh-tui/ui

English | [中文](README.zh.md)

## Summary

This library renders terminal state supplied by `@dsh-tui/app`. `App` displays committed history, live response text, pending input, status, and one human request. Components use typed locale dictionaries and callbacks. They do not access Cordis, Node services, storage, or a clock; the turn header is timed only from an optional `clock` prop, which `App` ignores under a screen reader, and animates only while the `motion` prop is not false. The runner always passes the clock and turns motion off under `NO_COLOR`, which leaves the elapsed seconds counting and nothing else moving.

## Table of Contents

- [Use this package](#use-this-package)

- [Understand the implementation](#understand-the-implementation)

- [Model Experience](#model-experience)

- [Known Limitations](#known-limitations)

<a id="use-this-package"></a>

## Use this package

Import `App` and `AppProps` from the package entry point. Supply authoritative state and callbacks as demonstrated by the [runner](../app/src/runner.ts) and [component tests](tests/shell.spec.tsx). `project(event, projector)` maps a committed Session event to zero or more rows; build the projector with `projector(copy, lookup)`, one per transcript, where `lookup` resolves a recorded tool name to its `presentCall` / `presentResult` — pass `() => undefined` to render every tool at its raw arguments. `formatRow` provides a plain-text representation. Supply `completion` with session-owned command/skill metadata and discovery status, plus a positive integer `completionLimit` for visible menu rows and a non-negative integer `resultLines` for the tool-result lines the transcript keeps under each outcome. The menu filters a leading slash token, inserts the selected name on Tab, and closes on Escape before cancellation reaches the application. Paste only edits the draft. This is a library dependency, with no Cordis mount row.

Supply query-tagged `files` and `onReferenceQuery` for workspace-path discovery. The callback receives the active query or `undefined` when the menu closes. Only results matching the current query can be selected. Pure `activeAtToken` and `formatFileMention` imports from the Harness grammar own token detection and quoting; file Tab completion preserves surrounding text and directory Tab completion keeps discovery open.

The composer supports cursor insertion, grapheme movement and deletion, logical-line Home/End, and Shift-Enter for newlines. Enter submits the full draft. Up/Down browses human input when completion is closed; Ctrl-P/N recalls history while a menu is open. Returning past the newest entry restores the original draft and cursor. See the [keyboard controls](../app/README.md#use-this-package) for completion and editing behavior.

Supply `todos` with the agent's current task list, or `undefined` before it writes one; the panel shows what is left and counts what is finished. Supply `attachments` with staged metadata; pending inputs and user rows can also carry attachment summaries. The UI renders names, byte lengths, and available image metadata, without reading bytes or paths. `onSubmit` may return `false` or a promise: refusal/rejection preserves the draft and cursor, while successful asynchronous acceptance clears them. Pending acceptance suppresses edits, paste, recall, and repeated Enter, including keys decoded in the same read; Escape and Ctrl-C remain active. An empty draft can submit when staged attachments exist.

Use `dictionaries.en` or `dictionaries.zh` for application labels. Model text, tool output, question details, and service diagnostics remain verbatim. Build `committed` from `emptyTranscript` and `appendTranscript(previous, rows)`. Snapshots and their row batches must remain immutable and append-only because Ink `Static` prints each suffix once. `transcriptRows(snapshot, start)` reads only the suffix following a previously rendered count; streaming updates do not scan committed rows. Changing `sessionId` remounts session presentation, resets draft/cursor/recall and completion state, and prints a localized session heading before its history. Set `inputBlocked` during navigation to suspend composer input while retaining Escape and Ctrl-C; choice interactions remain usable. Pass the optional Harness `plan` projection to show active or pending plan mode in the status line. Context estimates display a `~` prefix, and pending input includes the localized `/clear-pending` hint.

<a id="understand-the-implementation"></a>

## Understand the implementation

<details>

<summary>Input and presentation</summary>

Ink `usePaste` inserts text without actions; `useInput` handles typed keys. `useComposer` keeps same-read edits available before React paints. History visits lazily traverse pending human input and committed user rows, retaining local edits only for visited entries. Typing and rendering do not scan history, and recalling or editing an entry never mutates the transcript. Text questions and login prompts share cursor editing without a history source.

The composer follows the newest line: one blank row separates the latest transcript or streaming row from the framed input, and the status line sits under the frame, indented to the prompt. Once the screen fills, history scrolls and the composer rests at the bottom. While a turn runs, a header above the frame keeps one word for the whole turn (`✻ Kneading…  thinking · 12s`), with the phase and elapsed time beside it. Streaming reasoning is shown there as a single dim line holding the newest thought, rather than scrolling the live region; the full reasoning still commits to history. The application prints each finished line of a streaming answer to history as it completes (rows marked `continued` carry the rest of a block without reopening it), so the live region holds only the line still arriving and any call still streaming, and the composer stays on the bottom row of a full screen. Completion lists, notices, queued input, and quit feedback open between the blank row and the frame, sharing the available rows. Wrapped drafts grow within the composer limit. Narrowing the terminal repaints it so reflowed frame rows are not left behind; normal streamed turns preserve native scrollback without full-screen clears.

The composer follows the caret within wrapped text, including Home/End moves and Unicode input. Short terminals drop spacing, status, and framing as needed to keep input visible. A stable `Static` instance retains committed history across resize and reads only each new suffix.

A dim divider opens each user message. Assistant replies use a `<` marker, and reasoning is dimmed and wraps to the prose measure. A call and its result draw as one block opened by `●`. While the call runs, the marker pulses in the accent and the verb is present tense (`run`, `read`); when the result arrives, the same row turns green, or red on failure, and the verb becomes past tense (`ran`, `edited`, `found`). No call ids are drawn. `Actions` folds each result into its call and releases merged calls in call order, so out-of-order results cannot reorder the blocks; the application draws the calls it still holds in the live region. The marker and verb are bold, and `error` is bold red. Arguments and output use normal brightness; failures and diffs retain their semantic colors. The turn header uses its own warm accent, with a glint that crosses the word while it moves. When the turn ends, the header's row stays as a summary until the next turn: `✓ Completed`, a yellow `■` with the reason a turn stopped, or `✗ Failed`, then the elapsed time, the calls counted by past-tense verb, and how many failed. A resumed session shows its newest ended turn the same way, without a time. A result's output is bounded rather than printed whole: a command's output, an edit's diff, and any failure show the first `resultLines` lines at the output column and a dim count of the rest, while a read, a search, or a fetch reports only its size on the head line. It prints once and is never removed, so a result does not move the composer after it appears. The full text stays in the Session log. A completed turn leaves no line in the transcript, because the summary row says so. Every other recorded turn outcome — interruption, blocking, output-token limits, and errors — stays as a separate footer without a turn number; it does not imply that the Agent is idle. Completion windows keep the selection visible after height changes and count omitted entries on both sides.

`cards.ts` maps the `card`-tagged render intents of `dsh-tools` into card lines, and `present.ts` places them under the call they belong to. A tool with no presenter, one the lookup does not find, a card kind newer than this build, and a presenter that throws all keep the raw arguments and result text. The presenters are pure over the call's arguments and the durable result, including the `meta` the session log persists, so a replay reproduces the identical card.

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

- A `diff` card marks the span between the lines a hunk shares at each end. A pair that also matches inside the change widens the marked span rather than splitting it.

- The composer has no mouse positioning, word movement, or vertical movement through wrapped lines; Up/Down browses history when completion is closed.

### Dev Note

None.
