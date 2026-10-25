# Native terminal direction

## Summary

Keep Bake's familiar transcript and bottom composer while making agent activity, keyboard focus, and message destinations easier to understand. Use Ratatui with Crossterm as the starting stack and a pure Bake-owned editor model for the composer. Qualify inline scrollback and editor behavior before adopting the native frontend. This page defines acceptance criteria for [scopes 14 and 15](README.md#14--terminal-engine-and-rendering); the shipped TypeScript UI remains the comparison oracle. The [native terminal design](tui-design.md) proposes the screens, composer, and architecture that implement them.

## Table of Contents

- [Layout and interaction](#layout-and-interaction)
- [Agent handling](#agent-handling)
- [Composer](#composer)
- [Rust crate selection](#rust-crate-selection)
- [Acceptance scenarios](#acceptance-scenarios)
- [Delivery order](#delivery-order)
- [Dev Note](#dev-note)

## Layout and interaction

The native frontend keeps the current reading order: transcript, compact activity rows, composer, and contextual hints. Inline mode preserves shell scrollback; fullscreen remains available. The composer stays at the bottom as output arrives. Use the row budgets, resize behavior, narrow-terminal rules, and screen-reader behavior in [the layout design](https://github.com/Justar96/bake/blob/97ce6abba01fddb5ed532b60a2e25a1e134b5244/apps/tui/DESIGN-LAYOUT.md) as the starting requirements.

Make the screen cleaner through consistent spacing, aligned status text, and fewer competing labels. Show the selected agent's details when needed instead of repeating every detail in every row. Preserve room for the draft before optional hints or background-job detail. Status must remain understandable without color, and progress updates must not move focus, reset a menu selection, or displace the caret.

The runtime owns agent state, inboxes, permissions, and logged history. The frontend owns focus, open sheets, scroll position, and draft editing. Render the authoritative projections described in [the TUI design](https://github.com/Justar96/bake/blob/97ce6abba01fddb5ed532b60a2e25a1e134b5244/apps/tui/DESIGN.md); avoid a second agent lifecycle model in the frontend.

## Agent handling

The agent list should make three things clear: which agent is selected, what it is doing, and which action the current key will perform. Keep the existing keyboard entry points and parent/child hierarchy. Use a short label and stable identity, a textual state, and details for the selected row. Preserve selection by agent identity when another agent starts, finishes, or changes position.

Child inspection is read-only. Its header identifies the child and parent, shows parent activity or a pending human request, and explains how to return. Printable input during inspection must never reach either model or modify the parent draft; provide a brief visible explanation when the user tries to type. Returning restores the parent draft, caret, and undo history. Child inspection must not resume, cancel, or dispose the child.

Show the message destination clearly when entering or leaving inspection. Selecting an agent for inspection must never redirect the composer. New streaming output updates the selected view without stealing focus. Human requests remain scoped to their owning agent and request identity; selecting a child cannot answer a parent's request by accident.

These improvements use existing orchestration behavior. Direct user messages to a child would require a separate scope-11 design for admission, attribution, persistence, and model-visible input, followed by the required eval. A visual selection alone does not authorize that behavior.

## Composer

Keep a multiline editor that remains responsive during replay and streaming. Preserve grapheme movement, visual-row navigation, logical-line shortcuts, path-aware word stops, kill/yank behavior, grouped undo, history recall, completion, and attachment placeholders. [The editor contract](https://github.com/Justar96/bake/blob/97ce6abba01fddb5ed532b60a2e25a1e134b5244/apps/tui/DESIGN.md#5-input-flow) and its [pure editor tests](https://github.com/Justar96/bake/blob/97ce6abba01fddb5ed532b60a2e25a1e134b5244/apps/tui/packages/ui/tests/editor.test.ts) own the exact behavior.

Keep multiline bracketed paste separate from submission. Large text and image placeholders must preserve their existing admission and removal rules: deleting an image unstages it, and undo or yank must not silently attach it again. A refused submission retains the draft and caret. Canceling a picker, approval flow, or external editor must preserve the appropriate draft and restore keyboard focus.

Use one cell-width calculation for wrapping, caret placement, selection, and clicks. The native editor must cover emoji sequences, combining marks, CJK, Thai and Lao spacing marks, and tabs. Place the terminal cursor at the editing caret where the terminal supports it, and validate IME candidate placement manually on named terminals before claiming support.

Keep Bake's submit/newline shortcuts under Bake's control. A widget's default Enter, Ctrl-U, Ctrl-K, or Ctrl-C binding cannot replace the existing bindings implicitly. Keyboard-enhancement protocols need separate terminal qualification, including type-ahead, numpad Enter, and restoration after failure.

## Rust crate selection

Ratatui with Crossterm is the first implementation choice because Bake needs reusable rendering and layout, inline and fullscreen viewports, portable terminal events, and testable buffers. This is a choice for Bake's requirements, subject to the tests below.

| Layer | Choice | Adoption condition |
|---|---|---|
| Rendering and layout | [Ratatui](https://ratatui.rs/concepts/backends/) | Fixed buffers with Bake-owned output placement must preserve the bottom composer, bounded live region, committed rows, and resize behavior in a real PTY. The [inline experiment](tui-design.md#dev-note) records why the built-in inline resize path is unsuitable. A fullscreen demo does not qualify inline mode. |
| Events and terminal modes | Crossterm, through Ratatui's supported backend | One compatible Crossterm dependency version must own input and raw mode. Verify acquisition, cancellation, and restoration on POSIX and Windows ConPTY. |
| Draft behavior | A pure Bake-owned editor model | The [widget qualification](https://github.com/Justar96/bake/blob/97ce6abba01fddb5ed532b60a2e25a1e134b5244/docs/roadmap/rust-0.4/scope-00/editor-2026-10-07/README.md) found 15 mismatches in `ratatui-textarea` 0.9.3, including grapheme editing and caret width. Render Bake's model with Ratatui and keep terminal effects outside it. |

The official backend guide warns that incompatible Crossterm versions have separate event queues and raw-mode tracking. The opt-in [native workspace](../../development.md) pins the preview's toolchain and dependencies; its locked dependency graph must contain one Crossterm version. The editor qualification's manifest and lockfile remain frozen experiment records, separate from that workspace and shipped dependencies.

The inline preview exercises variable-height content through a Bake-owned writer over Ratatui buffers and Crossterm. The writer owns output only; agent and editor state stay with their existing owners. Linux PTY tests check the preview's resize and scrollback behavior. Real terminal reflow and the remaining platform cases still require qualification.

## Acceptance scenarios

Run native counterparts in real PTYs with a terminal emulator, in inline and fullscreen modes where applicable. Keep expected state, saved-session effects, and terminal restoration assertions as well as screen assertions. The linked TypeScript cases are existing oracles; additional acceptance criteria below are planned work, not passing native results.

| Scenario | Required outcome | Current oracle |
|---|---|---|
| Inspect a streaming child with an edited parent draft | Escape restores the exact draft, caret, and undo history. Typing during inspection submits nothing; the child log is unchanged. Parent output appears in order. | [Status tests](https://github.com/Justar96/bake/blob/97ce6abba01fddb5ed532b60a2e25a1e134b5244/apps/tui/packages/ui/tests/status.spec.tsx), [scrollback tests](https://github.com/Justar96/bake/blob/97ce6abba01fddb5ed532b60a2e25a1e134b5244/apps/tui/packages/ui/tests/scrollback.spec.tsx), PTY `inspect-agent`; add streaming-child, mid-draft caret, undo, and concurrent parent-output cases |
| Agent list changes during navigation | Selection follows the same agent identity; the selected details and action hints agree. Parent requests stay visible during inspection. | [Subagent routing tests](https://github.com/Justar96/bake/blob/97ce6abba01fddb5ed532b60a2e25a1e134b5244/apps/tui/packages/ui/tests/subagent-routing.spec.tsx); add native selection and parent-status cases |
| Stream while editing and completing a path | No lost or reordered input, caret movement, focus theft, or reset of the chosen completion. | [Placement tests](https://github.com/Justar96/bake/blob/97ce6abba01fddb5ed532b60a2e25a1e134b5244/apps/tui/packages/ui/tests/placement.spec.tsx), [live tests](https://github.com/Justar96/bake/blob/97ce6abba01fddb5ed532b60a2e25a1e134b5244/apps/tui/packages/ui/tests/live.spec.tsx) |
| Paste, edit, undo, and recall | Multiline paste does not submit; placeholders expand correctly on submission; image removal stays effective after undo. Refused admission keeps text and caret. | [Composer tests](https://github.com/Justar96/bake/blob/97ce6abba01fddb5ed532b60a2e25a1e134b5244/apps/tui/packages/ui/tests/shell.spec.tsx), [paste tests](https://github.com/Justar96/bake/blob/97ce6abba01fddb5ed532b60a2e25a1e134b5244/apps/tui/packages/ui/tests/paste.test.ts), PTY `attachments` |
| Unicode and narrow widths | Grapheme movement, wrapping, and the terminal's caret cell agree at 40 and 80 columns; short terminals preserve usable input. | [Editor tests](https://github.com/Justar96/bake/blob/97ce6abba01fddb5ed532b60a2e25a1e134b5244/apps/tui/packages/ui/tests/editor.test.ts), [caret tests](https://github.com/Justar96/bake/blob/97ce6abba01fddb5ed532b60a2e25a1e134b5244/apps/tui/packages/ui/tests/caret.test.ts), PTY `thai` |
| Resize while a sheet, child view, or stream is open | Draft and selection survive; committed output is not duplicated by streaming; fullscreen retains its reading position. | [Fullscreen tests](https://github.com/Justar96/bake/blob/97ce6abba01fddb5ed532b60a2e25a1e134b5244/apps/tui/packages/app/tests/fullscreen.spec.tsx), PTY `rendering` |
| Human request arrives during editing | Ownership and focus are visible; pasted or already queued draft input cannot approve a request. A canceled request cannot receive a late answer. | [Human-decision design](https://github.com/Justar96/bake/blob/97ce6abba01fddb5ed532b60a2e25a1e134b5244/apps/tui/DESIGN.md#6-human-decisions); add native type-ahead cases |
| Quit, panic, hangup, broken pipe, or external editor | Release owned work and restore raw mode, cursor, paste, mouse, autowrap, and screen state. Preserve pre-launch shell scrollback. | PTY `fatal-exception`, `late-rejection`, `hangup`, `fullscreen`, [external-editor tests](https://github.com/Justar96/bake/blob/97ce6abba01fddb5ed532b60a2e25a1e134b5244/apps/tui/packages/app/tests/external-editor.spec.ts); add broken-pipe and external-editor PTYs and Windows ConPTY counterparts |

The [PTY driver](https://github.com/Justar96/bake/blob/97ce6abba01fddb5ed532b60a2e25a1e134b5244/apps/tui/scripts/pty-smoke.ts) owns the named scenarios. Buffer snapshots complement these checks; they cannot prove input decoding or terminal restoration. Measure first usable input, live input, replay readiness, memory, and clean shutdown with the [performance diagnostic](https://github.com/Justar96/bake/blob/97ce6abba01fddb5ed532b60a2e25a1e134b5244/apps/tui/packages/app/performance/README.md), extending it for agent activity and resize before making performance claims about those paths.

## Delivery order

Preserve the [linear scope sequence](README.md#linear-development-sequence). Scope 00 records this direction and the TypeScript oracle. Scope 01 provides shared fixtures and comparison drivers. Scope 11 owns durable orchestration. Scope 14 implements terminal ownership, the editor, transcript output, and viewport behavior; scope 15 connects the interactive workflows.

A crate or renderer prototype may run earlier in an isolated development path. It neither changes the shipped launcher nor closes a later scope. Keep the current UI runnable until the native frontend satisfies the accepted behavior and intentional differences have their own reviewed tests.

The [Rust preview](../../development.md) is this early path: `bun run dev:rust:preview` launches a fullscreen draft editor, fixed sample-agent list, and read-only inspector, and `bake preview --inline` runs the same components with terminal scrollback, both with scripted fixture turns and no model connection or session persistence. `bun run dev:rust` runs the same components against a real Pi-format agent session (`bake`, or `bake --inline`). Their unit and PTY checks qualify only that supported subset; the full acceptance scenarios above remain open.

## Dev Note

2026-10-25: Bake requests Kitty keyboard disambiguation and event types on Unix, balancing the screen-local flag stack before terminal restoration. The live terminal reports lifecycle state and saved-session restore commands through Herdr's documented 0.9.5 CLI. A single worker coalesces queued reports, bounds each CLI child, reaps it, and releases the pane on shutdown. Unit tests cover lifecycle precedence, session replacement, argument validation, coalescing, and release ordering; PTY scenarios cover CSI-u modified keys and restoration. Protocol sources: [Kitty keyboard specification](https://sw.kovidgoyal.net/kitty/keyboard-protocol/) and [Herdr's agent integration](https://herdr.dev/docs/add-herdr-support/). This adapts no Pi implementation.

2026-10-25: Bake's live terminal shows its state to the terminal and reports more of it to Herdr. `bake-tui::lifecycle` is the one projection of runtime updates both use: `working`, `blocked` (an approval request), or `idle`, with one control-free line of detail. That detail is the bar's phase or `<tool> needs approval`, never a prompt, tool arguments, or reason. Dialogs no longer block, as they did in 0.4.2: each answers a command the person ran, and Herdr's guide and its own Pi integration reserve `blocked` for the agent needing a decision. A live run in an isolated Herdr 0.9.5 server showed a picker such as `/model` flagging the pane as needing attention. `bake-tui::indicators` ports Pi [v1.4.0](https://github.com/earendil-works/pi/releases/tag/v1.4.0) (`4a080252f06caf63a525d7873c5d94d3c23887e0`): `updateTerminalTitle` in `coding-agent/src/modes/interactive/interactive-mode.ts`, and `setTitle`, `setProgress`, and the one-second OSC 9;4 keep-alive in `tui/src/terminal.ts`. Pi's `terminal.showTerminalProgress` becomes a `Settings` field. Bake adds a `◐` or `?` title marker (Herdr's `terminal_title_stripped` recognizes and drops `◐`), the xterm title stack (Ghostty parses CSI 22/23 t but ignores it), paused progress while a running turn waits for a decision, and a 10-second error state after a failed turn. Unset, progress is on only in Ghostty, because a terminal that treats every OSC 9 as a notification would announce `4;3`. Ghostty also gets Bake's own OSC 777 notifications (`terminal.notifications`) when a turn completes or fails, or an approval request appears, while DECSET 1004 has reported that the terminal lost focus. The input decoder forwards focus changes outside the view's messages. The terminal owner records the title stack, progress, and focus reporting as modes, so restoration after a quit, signal, or panic undoes them. `SessionStatus` gains the session's `name`, and `SessionInfoChanged` refreshes it. The Herdr reporter adds `--message` to `blocked` reports, as its guide asks. It keeps sidebar tokens (`$model`, `$thinking`, `$context`, `$session`, `$activity`) through `pane report-metadata`, sending them after the state with the same sequence counter. It clears them before `release-agent`, because Herdr keeps tokens after a release. Pi's OSC 7501 program status is not ported: it needs a reply to its startup query, which Crossterm's input reader would not separate from keys, and Ghostty parses it without acting on it yet. Unit tests cover the lifecycle, title, progress keep-alive and error expiry, focus-gated notifications, settings, the loop's handling of updates and focus, Herdr messages and tokens, and the worker's clear-before-release order. Properties show that carried text holds no control character and that a notification keeps two fields. A Linux PTY scenario runs `bake` with `TERM_PROGRAM=ghostty` and a stand-in Herdr CLI through `/name`, a turn that ends unfocused, and `SIGTERM`. Protocol sources: [Ghostty's OSC 9;4](https://ghostty.org/docs/vt/osc/conemu), [OSC 9](https://ghostty.org/docs/vt/osc/9), the `desktop-notifications` and `progress-style` options in its [configuration reference](https://ghostty.org/docs/config/reference), and Herdr's [agent integration](https://herdr.dev/docs/add-herdr-support/), [CLI reference](https://herdr.dev/docs/cli-reference/), and [agent sidebar](https://herdr.dev/docs/agents/) at 0.9.5. No prompt, tool, or model request changes, so no paired evaluation is due.

Non-authoritative: draft redo and keyboard selection may be useful after baseline editing parity. Their shortcut choices must work across supported terminals and compose with the kill ring and attachment placeholders. They are not prerequisites for the current comparison baseline.
