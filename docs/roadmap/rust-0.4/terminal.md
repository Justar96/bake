# Native terminal direction

## Summary

Keep Bake's familiar transcript and bottom composer while making agent activity, keyboard focus, and message destinations easier to understand. Use Ratatui with Crossterm as the starting stack and a pure Bake-owned editor model for the composer. Qualify inline scrollback and editor behavior before adopting the native frontend. This page defines acceptance criteria for [scopes 14 and 15](README.md#14--terminal-engine-and-rendering); the shipped TypeScript UI remains the comparison oracle.

## Table of Contents

- [Layout and interaction](#layout-and-interaction)
- [Agent handling](#agent-handling)
- [Composer](#composer)
- [Rust crate selection](#rust-crate-selection)
- [Acceptance scenarios](#acceptance-scenarios)
- [Delivery order](#delivery-order)
- [Dev Note](#dev-note)

## Layout and interaction

The native frontend keeps the current reading order: transcript, compact activity rows, composer, and contextual hints. Inline mode preserves shell scrollback; fullscreen remains available. The composer stays at the bottom as output arrives. Use the row budgets, resize behavior, narrow-terminal rules, and screen-reader behavior in [the layout design](../../../apps/tui/DESIGN-LAYOUT.md) as the starting requirements.

Make the screen cleaner through consistent spacing, aligned status text, and fewer competing labels. Show the selected agent's details when needed instead of repeating every detail in every row. Preserve room for the draft before optional hints or background-job detail. Status must remain understandable without color, and progress updates must not move focus, reset a menu selection, or displace the caret.

The runtime owns agent state, inboxes, permissions, and logged history. The frontend owns focus, open sheets, scroll position, and draft editing. Render the authoritative projections described in [the TUI design](../../../apps/tui/DESIGN.md); avoid a second agent lifecycle model in the frontend.

## Agent handling

The agent list should make three things clear: which agent is selected, what it is doing, and which action the current key will perform. Keep the existing keyboard entry points and parent/child hierarchy. Use a short label and stable identity, a textual state, and details for the selected row. Preserve selection by agent identity when another agent starts, finishes, or changes position.

Child inspection is read-only. Its header identifies the child and parent, shows parent activity or a pending human request, and explains how to return. Printable input during inspection must never reach either model or modify the parent draft; provide a brief visible explanation when the user tries to type. Returning restores the parent draft, caret, and undo history. Child inspection must not resume, cancel, or dispose the child.

Show the message destination clearly when entering or leaving inspection. Selecting an agent for inspection must never redirect the composer. New streaming output updates the selected view without stealing focus. Human requests remain scoped to their owning agent and request identity; selecting a child cannot answer a parent's request by accident.

These improvements use existing orchestration behavior. Direct user messages to a child would require a separate scope-11 design for admission, attribution, persistence, and model-visible input, followed by the required eval. A visual selection alone does not authorize that behavior.

## Composer

Keep a multiline editor that remains responsive during replay and streaming. Preserve grapheme movement, visual-row navigation, logical-line shortcuts, path-aware word stops, kill/yank behavior, grouped undo, history recall, completion, and attachment placeholders. [The editor contract](../../../apps/tui/DESIGN.md#5-input-flow) and its [pure editor tests](../../../apps/tui/packages/ui/tests/editor.test.ts) own the exact behavior.

Keep multiline bracketed paste separate from submission. Large text and image placeholders must preserve their existing admission and removal rules: deleting an image unstages it, and undo or yank must not silently attach it again. A refused submission retains the draft and caret. Canceling a picker, approval flow, or external editor must preserve the appropriate draft and restore keyboard focus.

Use one cell-width calculation for wrapping, caret placement, selection, and clicks. The native editor must cover emoji sequences, combining marks, CJK, Thai and Lao spacing marks, and tabs. Place the terminal cursor at the editing caret where the terminal supports it, and validate IME candidate placement manually on named terminals before claiming support.

Keep Bake's submit/newline shortcuts under Bake's control. A widget's default Enter, Ctrl-U, Ctrl-K, or Ctrl-C binding cannot replace the existing bindings implicitly. Keyboard-enhancement protocols need separate terminal qualification, including type-ahead, numpad Enter, and restoration after failure.

## Rust crate selection

Ratatui with Crossterm is the first implementation choice because Bake needs reusable rendering and layout, inline and fullscreen viewports, portable terminal events, and testable buffers. This is a choice for Bake's requirements, subject to the tests below.

| Layer | Choice | Adoption condition |
|---|---|---|
| Rendering and layout | [Ratatui](https://ratatui.rs/concepts/backends/) | Its [inline viewport](https://docs.rs/ratatui/latest/ratatui/enum.Viewport.html) must preserve the bottom composer, bounded live region, committed rows, and resize behavior in a real PTY. A fullscreen demo does not qualify inline mode. |
| Events and terminal modes | Crossterm, through Ratatui's supported backend | One compatible Crossterm dependency version must own input and raw mode. Verify acquisition, cancellation, and restoration on POSIX and Windows ConPTY. |
| Draft behavior | A pure Bake-owned editor model | The [widget qualification](scope-00/editor-2026-10-07/README.md) found 15 mismatches in `ratatui-textarea` 0.9.3, including grapheme editing and caret width. Render Bake's model with Ratatui and keep terminal effects outside it. |

The official backend guide warns that incompatible Crossterm versions have separate event queues and raw-mode tracking. Pin compatible crate versions, features, licenses, and a Rust minimum version when the renderer experiment passes; verify the resolved dependency graph then. The editor qualification's manifest and lockfile are frozen experiment records, separate from the planned native workspace and shipped dependencies.

The first renderer experiment must exercise variable-height inline content and terminal reflow. If Ratatui's inline facilities cannot meet those obligations, evaluate a small Bake-owned inline writer over its buffers and Crossterm before committing to a larger frontend. Keep any such writer responsible for output only; agent and editor state stay with their existing owners.

## Acceptance scenarios

Run native counterparts in real PTYs with a terminal emulator, in inline and fullscreen modes where applicable. Keep expected state, saved-session effects, and terminal restoration assertions as well as screen assertions. The linked TypeScript cases are existing oracles; additional acceptance criteria below are planned work, not passing native results.

| Scenario | Required outcome | Current oracle |
|---|---|---|
| Inspect a streaming child with an edited parent draft | Escape restores the exact draft, caret, and undo history. Typing during inspection submits nothing; the child log is unchanged. Parent output appears in order. | [Status tests](../../../apps/tui/packages/ui/tests/status.spec.tsx), [scrollback tests](../../../apps/tui/packages/ui/tests/scrollback.spec.tsx), PTY `inspect-agent`; add streaming-child, mid-draft caret, undo, and concurrent parent-output cases |
| Agent list changes during navigation | Selection follows the same agent identity; the selected details and action hints agree. Parent requests stay visible during inspection. | [Subagent routing tests](../../../apps/tui/packages/ui/tests/subagent-routing.spec.tsx); add native selection and parent-status cases |
| Stream while editing and completing a path | No lost or reordered input, caret movement, focus theft, or reset of the chosen completion. | [Placement tests](../../../apps/tui/packages/ui/tests/placement.spec.tsx), [live tests](../../../apps/tui/packages/ui/tests/live.spec.tsx) |
| Paste, edit, undo, and recall | Multiline paste does not submit; placeholders expand correctly on submission; image removal stays effective after undo. Refused admission keeps text and caret. | [Composer tests](../../../apps/tui/packages/ui/tests/shell.spec.tsx), [paste tests](../../../apps/tui/packages/ui/tests/paste.test.ts), PTY `attachments` |
| Unicode and narrow widths | Grapheme movement, wrapping, and the terminal's caret cell agree at 40 and 80 columns; short terminals preserve usable input. | [Editor tests](../../../apps/tui/packages/ui/tests/editor.test.ts), [caret tests](../../../apps/tui/packages/ui/tests/caret.test.ts), PTY `thai` |
| Resize while a sheet, child view, or stream is open | Draft and selection survive; committed output is not duplicated by streaming; fullscreen retains its reading position. | [Fullscreen tests](../../../apps/tui/packages/app/tests/fullscreen.spec.tsx), PTY `rendering` |
| Human request arrives during editing | Ownership and focus are visible; pasted or already queued draft input cannot approve a request. A canceled request cannot receive a late answer. | [Human-decision design](../../../apps/tui/DESIGN.md#6-human-decisions); add native type-ahead cases |
| Quit, panic, hangup, broken pipe, or external editor | Release owned work and restore raw mode, cursor, paste, mouse, autowrap, and screen state. Preserve pre-launch shell scrollback. | PTY `fatal-exception`, `late-rejection`, `hangup`, `fullscreen`, [external-editor tests](../../../apps/tui/packages/app/tests/external-editor.spec.ts); add broken-pipe and external-editor PTYs and Windows ConPTY counterparts |

The [PTY driver](../../../apps/tui/scripts/pty-smoke.ts) owns the named scenarios. Buffer snapshots complement these checks; they cannot prove input decoding or terminal restoration. Measure first usable input, live input, replay readiness, memory, and clean shutdown with the [performance diagnostic](../../../apps/tui/packages/app/performance/README.md), extending it for agent activity and resize before making performance claims about those paths.

## Delivery order

Preserve the [linear scope sequence](README.md#linear-development-sequence). Scope 00 records this direction and the TypeScript oracle. Scope 01 provides shared fixtures and comparison drivers. Scope 11 owns durable orchestration. Scope 14 implements terminal ownership, the editor, transcript output, and viewport behavior; scope 15 connects the interactive workflows.

A crate or renderer prototype may run earlier in an isolated development path. It neither changes the shipped launcher nor closes a later scope. Keep the current UI runnable until the native frontend satisfies the accepted behavior and intentional differences have their own reviewed tests.

## Dev Note

Non-authoritative: draft redo and keyboard selection may be useful after baseline editing parity. Their shortcut choices must work across supported terminals and compose with the kill ring and attachment placeholders. They are not prerequisites for the current comparison baseline.
