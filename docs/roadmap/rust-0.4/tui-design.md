# Native terminal design

## Summary

This page proposes how Bake's Rust terminal frontend looks, behaves, and is built, starting with fullscreen mode. It keeps the shipped TypeScript layout and visual grammar, frames the draft in a rounded box that costs no more rows than the two rules it replaces, makes the composer render dynamically without moving text or the input row, and separates pure presentation from terminal effects so each layer can be tested on its own. It implements the [terminal direction](terminal.md), which owns the acceptance scenarios; the [layout design](../../../apps/tui/DESIGN-LAYOUT.md) and [TUI design](../../../apps/tui/DESIGN.md) remain the oracle for every behavior not listed here as an intentional difference.

Status: proposed. The [Rust preview](../../../rust/README.md) implements the composer box, its wrapping and window, the frame fallback, and [delivery slice 1](#delivery-slices): the crate split, the update function, the key-binding table, and the channel-driven loop. The rest of this page is not implemented. Fullscreen comes first by owner decision; inline scrollback remains a 0.4.0 requirement of [scope 14](README.md#14--terminal-engine-and-rendering), and this design keeps it possible without qualifying it.

## Table of Contents

- [Principles](#principles)
- [Screen anatomy](#screen-anatomy)
- [Intentional differences](#intentional-differences)
- [Composer](#composer)
- [Activity line](#activity-line)
- [Panels above the input](#panels-above-the-input)
- [Layout planner](#layout-planner)
- [Transcript viewport](#transcript-viewport)
- [Architecture](#architecture)
- [Preview status](#preview-status)
- [Delivery slices](#delivery-slices)
- [Verification](#verification)
- [Open decisions](#open-decisions)
- [Dev Note](#dev-note)

## Principles

Every rule below is testable. Each rule names a behavior that a buffer or PTY test can check.

1. **Input comes first.** A key changes the draft and reaches the screen before any runtime, transcript, or measurement work. The input reader never waits on the runtime, and a frame that contains an edit is drawn without waiting for the coalescing interval.
2. **Nothing locks input.** Every panel, sheet, picker, and inspection closes with Esc, and focus returns to where the user opened it. Streaming, replay, and resize never drop or reorder keys.
3. **Things leave the way they came.** Input panels open above the header and give their rows back to the transcript when they close. A key that opens a sheet also closes it. Leaving a child restores the parent draft, caret, undo history, and reading position.
4. **Every screen answers three questions.** What am I looking at: the header names the agent and its activity. What will Enter do: the placeholder or the box's bottom edge says. How do I get out: Esc always has a named destination.
5. **Motion is restrained.** Only the shimmer across the header's activity word and its seconds move. Nothing animates on a keystroke. With motion off, under `NO_COLOR`, or with a screen reader, state changes are discrete and the word holds still.
6. **One grammar.** The rail markers, verbs, palette roles, and the "same family, one weight heavier" relation between `›` (a user's words) and `❯` (where the user types) stay as the [layout design](../../../apps/tui/DESIGN-LAYOUT.md#the-composer-is-the-same-family-one-weight-heavier) defines them.

## Screen anatomy

Fullscreen fills the terminal rectangle. The transcript viewport takes every row the controls leave, and the controls rest on the bottom rows in every state. The mockups are illustrative at 80 columns, wrapped as the preview wraps them; `▏` marks the terminal cursor, and product text comes from the copy module.

Every row aligns to four edges:

| Edge | Column | Holds |
|---|---|---|
| Rail | 0 | Transcript markers (`›`, `●`, `*`) and the box's left side |
| Prompt | 2 | The composer's `❯`, `^`, and `v`, and the first cell of every row drawn outside the box: header, panels, standing rows, status |
| Text | 4 in the box, 2 in the transcript | Draft and prose; tool output hangs from `⎿` as the [verb column](../../../apps/tui/DESIGN-LAYOUT.md#ascii-only-and-actions-are-named-rather-than-pictured) defines |
| Right | last column less 2 | The end of right-aligned keys and hints outside the box, level with the box's inner padding |

Idle, after a turn:

```
› Find where the session controller registers commands

  The registry is the list, so discovery should read it.
● Bash(rg -n "commands.register" -g '*.ts')
  ⎿  packages/app/src/controller.ts:45
     packages/app/src/controller.ts:52
  Two registrations, both through ctx.effect.
                                                  PgUp scroll · Ctrl+↑ prompts

  ✓ Completed  42s · read 1 · ran 1 · 42 tok/s             Ctrl+O ● Goal 3/256
╭──────────────────────────────────────────────────────────────────────────────╮
│ ❯ Ask anything · / commands · @ files                                        │
╰──────────────────────────────────────────────────────────────────────────────╯
  ↳ Subagents 2 · 1 working · 1 done                                    Ctrl+G
  deepseek-v4-flash  think high  ctx ~12% (15k/128k)  ⎇ main +2  ~/bake
```

Running, with a draft taller than its window; the activity is words alone, the hidden-row count and the mode hint ride the box's edges:

```
  Kneading…  running bash · 12s                            Ctrl+O ● Goal 3/256
╭─────────────────────────────────────────────────────────────────── +2 above ─╮
│ ^ Then thread the value through startup.ts and the session store, and add a  │
│   regression test that sets both DSH_HOME and --home to prove which one      │
│   wins.                                                                      │
│                                                                              │
│   Keep the public API unchanged.▏                                            │
╰───────────────────────────────────────────────────────────── Esc interrupts ─╯
  deepseek-v4-flash  think high  ctx ~14% (18k/128k)  ⎇ main +2  ~/bake
```

Completion open; the panel sits on the header, nearest the token that opened it:

```
* /changelog  Show what changed in this Bake version
  /model      Choose model and reasoning effort
  /thinking   Change reasoning effort, also during a turn
  +14 more — keep typing to narrow
  ✓ Completed  42s · read 1 · ran 1                        Ctrl+O ● Goal 3/256
╭──────────────────────────────────────────────────────────────────────────────╮
│ ❯ /▏                                                                         │
╰────────────────────────────────────────────────────────────────── ↑↓ select ─╯
```

Inspecting a child; the parent draft is kept and shown dim:

```
  ↳ explorer  child of main · read-only                 main: approval waiting
╭──────────────────────────────────────────────────────────────────────────────╮
│ ❯ Then thread the value through startup.ts and the session store, and        │
╰─────────────────────────────────────────────────── draft kept · Esc returns ─╯
  ↳ Subagents 2 · 1 working · 1 done                                    Ctrl+G
```

## Intentional differences

Each difference from the TypeScript frontend needs its own reviewed acceptance case under [scope 15](README.md#15--interactive-workflows) before release. Everything not listed here is a parity requirement.

| Id | Difference | Reason | Acceptance case |
|---|---|---|---|
| D1 | The composer window holds `clamp(rows / 5, 5, 12)` draft rows instead of five, so it is unchanged below 30 rows | A tall terminal has room to show a long draft; five rows of 60 is cramped editing | Buffer tests at 24, 40, and 60 rows; the transcript keeps at least its minimum rows |
| D2 | The terminal's cursor marks the caret instead of a reverse-video cell | [Terminal direction](terminal.md#composer) requires the cursor at the caret for IME placement; it also follows the user's cursor shape and blink settings | PTY reads the cursor position after every edit and resize; manual IME check on named terminals |
| D3 | Atomic draft tokens are styled: a registered `/command` name bold, an `@path` mention and a paste or image placeholder cyan | The user can see which text the runtime will treat specially before sending it | Buffer styles under truecolor and `NO_COLOR`; the text and caret columns are identical with and without styling |
| D4 | Child inspection shows the kept parent draft, dim, with `draft kept · Esc returns` on the box's bottom edge | Inspection must state where input goes and that the draft is safe ([agent handling](terminal.md#agent-handling)) | The existing inspection case, plus a buffer check that the dim draft matches the parent draft byte for byte |
| D5 | A rounded box frames the draft in place of the two bare rules; rows outside it are inset two cells to align with its contents; hidden-row counts and mode hints sit on its edges instead of a right-hand slot beside the caret | The input reads as one control distinct from the transcript. The box spends the rules' two rows and no more, and hints on the edges take no column from the draft, so a mode change never rewraps it | Buffer tests of the box at every width from 1 to 200, the classic frame, and edge labels that drop whole; PTY resize scenarios |
| D6 | The header's activity is text alone: no spinner glyph for a turn or for compaction. A band of light sweeps across the activity word, and the word list grows from 12 verbs to 32 | Words say what is happening without a symbol a terminal could measure differently, and a larger list keeps consecutive turns distinct | Pure tests pin the sweep, the colour levels, and the word choice; a PTY scenario checks that elapsed time advances without input and that no Braille glyph is drawn |

## Composer

The composer is a box around the draft: a prompt marker, the draft, and two edges that carry the window's hidden-row counts and the mode's hint. It changes height with its content and changes words with the session's mode, while its text, caret, and bottom edge stay where the user expects them.

### Shape

The box replaces the two rules that frame the TypeScript composer, so it costs the same rows. Its geometry depends on the terminal width alone, so a change in height never rewraps the draft.

- **12 columns or wider:** `│ ❯ text… │`. The left side, a padding cell, the prompt, and a space take four cells; a padding cell and the right side take two. The right padding cell is the caret's column, so a full row of text never wraps for the caret.
- **Narrower than 12 columns:** no sides. The edges are plain rules, the prompt takes two cells when the width exceeds three, and the last column is the caret's.
- **Short terminals:** the box closes only when both edges have rows. With one edge, that edge is a plain rule. The text keeps its columns either way.

The frame is chosen once, before the first frame, by the same rules as `resolveFrame`: a terminal without a UTF-8 locale, with no `TERM` or `TERM=dumb`, or with a Chinese, Japanese, or Korean character locale gets `+`, `-`, `|`, and a `>` prompt, because a full-width run of box-drawing characters drawn wider than measured wraps and breaks every row below it. Windows Terminal's `WT_SESSION` stands in for the locale and `TERM`.

Edge labels are dim and sit near the right end, with at least one rule cell on each side. The top edge carries `+N above`. The bottom edge carries `+N below`, or else the mode's hint. A label that does not fit is left out whole; the rail's `^` and `v` still mark hidden rows.

### Pipeline

Each stage is a pure function of the previous stage and the frame width. Only the last step touches a Ratatui buffer.

```mermaid
flowchart LR
    D[Draft: text, caret, tokens, undo] --> W[Wrap: rows per logical line]
    W --> L[DraftLayout: rows and caret cell]
    L --> V[Window: top row, hidden counts]
    V --> C[ComposerView: box, styled rows, edge labels, cursor]
    C --> B[Buffer and cursor position]
```

- **Wrap per logical line.** A visual row never crosses a line break, so each logical line wraps on its own. The wrap cache keys each line by its content revision and the draft width. An edit invalidates only the lines it touched, and a resize invalidates all of them. The preview wraps the whole draft each frame; the cache is planned.
- **The layout ignores the caret.** The row count depends only on the text and the width, never on where the caret is. Wrapping uses one column less than the row, and the caret takes that last column only after a row's text or over hanging whitespace, as the [composer rules](../../../apps/tui/DESIGN-LAYOUT.md#26-composer--a-cursor-following-window) require. Moving the caret therefore never moves a word or changes the composer's height.
- **Only visible rows are styled.** The view builds spans for the rows in the window. Rows above and below contribute only their counts.

### Wrapping

Wrapping follows `wrapDraft` exactly: rows break after spaces and tabs; whitespace at a break hangs past the row; wide characters are break opportunities of their own; Thai, Lao, Khmer, and Myanmar use dictionary word boundaries; a word longer than a row splits only between graphemes; and a tab advances to the next four-column stop. Wrapping, the caret, mouse clicks, and selection share one cell-width function, which delegates to the measure that Ratatui's buffer uses.

Hints and counts sit on the box's edges, never beside the draft, so the draft's width depends on the terminal width alone. A mode change, an opened menu, or a hidden-row count never rewraps the draft.

### Window

The window holds the visible rows. Its top row is presentation state that persists between frames. The first frame puts the caret on the window's bottom row. After that, the window moves only when the caret would leave it: typing at the end keeps the caret on the bottom row, and moving up inside a tall draft moves the caret up the window before the text scrolls. A width change renumbers the rows, so after a resize the caret keeps the window row it had, as far as the new layout allows. A shrinking draft never leaves blank rows under it while rows above are hidden.

A draft taller than the window marks what it hides. A dim `^` in the prompt column of the first visible row means rows above, and a dim `v` in the prompt column of the last row means rows below. The prompt marker stays on the draft's first row. The box's edges count the hidden rows: `+N above` and `+N below`.

### Height

The composer's height is `min(draft rows, window maximum)`, and the [planner](#layout-planner) grants it before any panel above the input. Growth takes rows from the transcript viewport, never from the controls below the composer, so the bottom edge, standing rows, and status line never move.

- **While following output,** the viewport stays anchored at its bottom, so a growing composer pushes the transcript up, in the same direction new output moves.
- **While reading history,** the viewport's anchor is at its top, so a growing composer covers the bottom of the viewport and the passage being read does not move.
- **When the draft shrinks,** freed rows return to the viewport in the same frame.

### Modes

The composer renders from the session's mode. The editor, the wrapping, and the window are the same in every mode; only the prompt style, placeholder, bottom-edge hint, and the meaning of Enter change.

| Mode | Prompt | Empty placeholder | Bottom edge | Enter |
|---|---|---|---|---|
| Idle | `❯` cyan | `Ask anything · / commands · @ files` | `Enter sends` while editing | Starts a turn |
| Running | `❯` cyan | `Enter steers the next step · Alt+↑ sends now` | `Esc interrupts` | Steers the next step |
| Compacting | `❯` cyan | `Compacting… Enter queues · Esc cancels` | none | Queues a turn |
| Completion open | `❯` cyan | n/a | `↑↓ select` | Selects the highlighted choice |
| Inspecting a child | `❯` dim | the kept parent draft, dim | `draft kept · Esc returns` | Explains that inspection is read-only |
| Masked sign-in field | `❯` cyan | the field's prompt | the field's hint | Submits the field |

The placeholder's parts after the first drop whole when they do not fit, and the idle placeholder drops them below 60 columns, as the [width rules](../../../apps/tui/DESIGN-LAYOUT.md#the-composer-yields-structure-before-it-yields-content) require. A hidden-row count takes the bottom edge before the hint does. A question's free-text answer and the sign-in field reuse the same editor and view with their own drafts and no history. A masked field draws each grapheme as `•`, and word motion treats the line as one word.

### Tokens

Paste placeholders, image placeholders, and completed mentions are atomic spans the draft records by byte range and identity. Motion and deletion treat a span as one grapheme. Deleting an image placeholder emits an unstage effect, and neither undo nor yank restores it; a text placeholder stays registered for both, as [the input contract](../../../apps/tui/DESIGN.md#5-input-flow) requires. Styling (D3) never changes a span's cell width.

## Activity line

The header says what the session is doing in words alone, at the prompt column: the turn's word with an ellipsis, then its phase and elapsed time, dim. `Kneading…  running bash · 12s`. No glyph stands beside it, so the row has no symbol whose width a terminal could measure differently.

- **Words.** One verb is chosen per turn by the TypeScript `activityWord` hash of a seed fixed for the turn, from 32 baking verbs: the 12 the TypeScript copy names and 20 more. Each names active work. None says the turn is resting or done, and `Laminating` stays with compaction.
- **Phase and time.** The phase follows `phaseOf` (`thinking`, `writing`, `running <tool> +N`), and elapsed time reads `8s` or `1m 05s`. On a narrow header the right-hand keys give way first, then the phase and time together; the word is never cut for them.
- **Shimmer.** A band of light three graphemes wide sweeps across the word, left to right, one grapheme every 70 ms, then the word rests unlit for twelve beats. On a truecolor terminal (`COLORTERM` of `truecolor` or `24bit`) the band blends from the running orange `#f97316` to the palette's glint `#fff7ed`. Otherwise it steps through yellow, light yellow, and bold bright white. The phase and time never shimmer.
- **Motion off.** Under `NO_COLOR` the word is bold in the terminal's foreground and holds still, and only the seconds change, once a second. Under a screen reader the line changes only when its words do.
- **Compaction.** `Compacting history…` takes the same row in the compacting blue `#3b82f6`, its band blending toward `#dbeafe` (blue, light blue, and bright white without truecolor), and returns to the turn's word when compaction settles.
- **Redraws.** The line changes on each shimmer beat, or on each second with motion off. The event loop wakes for that deadline and nothing else; Ratatui's buffer diff writes only the cells that changed.

## Panels above the input

Input panels belong to the input, not to the conversation. They stack between the blank row that opens the controls and the header, in the TypeScript order, top to bottom:

1. Pending input
2. Staged attachments
3. Sending notice
4. Interaction (approval or question)
5. Running command
6. Notice
7. Quit feedback
8. Completion
9. Command usage

Completion is lowest, so it sits next to the token that opened it. Every panel that lists a source of unbounded length uses a [derived item limit](../../../apps/tui/DESIGN-LAYOUT.md#the-item-limit-is-derived-never-constant) and the `+N more` form. An interaction never yields; when it cannot fit beside the composer, it replaces the controls, as it does today.

## Layout planner

One pure function turns the terminal size and the presentation state into row counts for every region. Regions claim rows in the order below; on a short terminal, the last claimant yields first. The input's first row is always granted.

| Claim order | Region | Rows |
|---|---|---|
| 1 | Composer, first row | 1 |
| 2 | Header | 1 |
| 3 | Status | 1 |
| 4 | Interaction | Its need; never yields |
| 5 | Box top edge, then bottom edge | 1 each |
| 6 | Subagents row, then background row | 1 each, while present |
| 7 | Gap above the controls | 1 |
| 8 | Further draft rows | Up to the window maximum, keeping the transcript's minimum |
| 9 | Quit feedback, completion, usage, pending, attachments, command, notice | Each its derived limit |
| 10 | Transcript hint row | 1 |
| 11 | Transcript viewport | The rest |

This order reproduces the TypeScript collapse: gap, bottom edge, top edge, status, and then header give way before the input. Claiming the edges together means a short terminal loses the open edge first, and the box closes only when both edges have rows. A width change never changes a row count by itself, because every chrome row is one row at any width.

## Transcript viewport

Fullscreen's transcript is a viewport over immutable committed batches and the current live rows, as [the fullscreen design](../../../apps/tui/DESIGN-LAYOUT.md#21a-fullscreen-transcript) describes. The Rust viewport keeps the same obligations:

- **Bounded work.** A row is presented only when visited, with at most 32 cached presentations, keyed by row identity, width, and presentation policy. Following output measures from the newest row up; reading measures from the anchor down. Neither mode measures the whole history.
- **One reading anchor.** The anchor is a row identity, a character offset, and a line within the row. It holds through appends, composer growth, resize, and the conversion of a streamed row into committed fragments.
- **Same keys.** PgUp and PgDn move a viewport less four rows. Ctrl+↑ and Ctrl+↓ jump between prompts. Ctrl+Home goes to the beginning, and Ctrl+End follows output. The mouse wheel uses the TypeScript acceleration. Open sheets and interactions receive navigation keys first.
- **No duplicate output.** A settled answer block appears once, whether it is still live or already committed.

## Architecture

The design mirrors the TypeScript split between a pure presentation package and an application that owns effects. Its crate names follow the roadmap's [target architecture](README.md#target-architecture).

### Crates

| Crate | Owns | May depend on |
|---|---|---|
| `bake-tui-view` | Editor, presentation state and its update function, key bindings, view models, layout planner, widgets, palette, frame glyphs, and English copy | `ratatui-core`, `ratatui-widgets`, `unicode-segmentation`, `unicode-width`; no Crossterm, I/O, threads, or clock reads |
| `bake-tui` | Terminal lease, input decoding, output, signals, the event loop, and the runtime port | `bake-tui-view`, `ratatui`, Crossterm, `signal-hook` |

The crate boundary makes purity a compile-time fact: `bake-tui-view` cannot reach the terminal because it does not link Crossterm. Both Ratatui sub-crates already appear in `rust/Cargo.lock` at the pinned Ratatui version, so the split adds no new third-party crate. Terminal leases move to `bake-host` when that crate exists.

### State and updates

The view crate receives every input as data, applies it, and returns effects; it never performs them.

```rust
/// Everything the frontend applies, in arrival order.
pub enum Msg {
    Key(KeyInput),          // Bake's own key type, decoded from Crossterm by bake-tui
    Paste(String),
    Mouse(MouseInput),
    Resize { cols: u16, rows: u16 },
    Tick(Duration),         // time since the loop started; the view reads no clock
    Runtime(RuntimeUpdate), // committed batch, live frame, status, projection change, request opened or closed
}

/// Requests the terminal owner performs on the view's behalf.
pub enum Effect {
    Submit(Submission),     // follow-up, steer, or queue, chosen from the agent's status
    Cancel(AgentId),
    Answer(RequestId, Answer),
    Query(QueryId, Query),  // completion and file discovery; stale answers are dropped by id
    Unstage(AttachmentId),
    Quit,
}

pub fn update(state: &mut State, msg: Msg) -> Vec<Effect>;
pub fn render(state: &mut State, frame: &mut Frame);
```

`render` changes only the composer window, because the rows the frame grants the composer decide where the window follows the caret. `State` holds presentation state only: drafts, focus, open sheets, window and viewport positions, completion selection, notices, and the quit timer's deadline. Agent status, inboxes, permissions, context pressure, goals, and the transcript stay with their runtime owners and arrive through `RuntimeUpdate`, following the [one-authority rule](../../../apps/tui/DESIGN.md#3b-one-authority-per-fact). Key bindings live in one table that maps a focus and a `KeyInput` to an action, so Bake, not a widget, owns every shortcut.

### Event loop

The UI thread owns `State` and the terminal. Other threads only send messages to it.

- **Sources.** A reader thread waits on Crossterm input and sends decoded messages. It wakes every 50 ms only to check whether to stop, so shutdown can join it before raw mode ends; a blocking read could not be interrupted, and it would hold Crossterm's reader lock against any query the UI thread makes. The runtime sends updates through the runtime port. A signal thread forwards SIGINT, SIGTERM, and SIGHUP. The UI thread never polls.
- **Batching.** The loop waits for the next message or the next deadline: a shimmer beat, an elapsed-second change, the quit timer, or a pending frame. It then applies a `Tick` and every waiting message in order, up to 256, and draws once.
- **Frame policy.** A batch that contains input is drawn at once. A batch of runtime updates alone is drawn at most once every 16 ms. The activity line is redrawn on each shimmer beat, or each second with motion off.
- **Output.** Every frame is written between synchronized-update markers (DEC mode 2026), and Ratatui's buffer diff writes only changed cells. A slow terminal blocks only the UI thread; the reader thread keeps queueing keys, and the next batch applies all of them.
- **Debris.** While the lease is held, nothing else writes to stdout or stderr. Diagnostics go to a file sink, and captured messages appear as notices, as [the debris rules](../../../apps/tui/DESIGN-LAYOUT.md#83-debris--someone-else-writes-to-the-terminal) require.

### Runtime port

`bake-tui` reaches the agent runtime through one port that accepts effects and produces `RuntimeUpdate` messages. Until the native runtime exists, a fixture port replays synthetic sessions and answers effects from a script. That lets the transcript, panels, and modes be built and tested before scope 09 without connecting a model. The fixture port is development tooling and does not count toward native parity.

### Terminal ownership

`TerminalSession` keeps its contract: it records each mode before requesting it and restores exactly those modes in reverse order on every exit path. It turns autowrap off, so a row the terminal draws wider than measured is clipped instead of scrolling the fixed screen, and it records an open synchronized update so a failure mid-frame still ends it. Fullscreen adds mouse reporting (SGR modes 1000, 1002, and 1006). Keyboard-enhancement protocols stay off until they pass their own terminal qualification.

### Room for inline mode

Fullscreen-first must not block inline mode. These rules keep the views independent of the screen mode:

- Row presentation produces lines at a width and knows nothing about the screen mode.
- The planner takes the screen mode as an input, so inline mode can add its held-height and cursor-row rules.
- Committed rows are immutable and identified, so an inline writer can print each row exactly once.

Qualifying a renderer for inline mode remains open work; this page does not choose between Ratatui's inline viewport and a Bake-owned writer.

## Preview status

The [Rust preview](../../../rust/README.md) implements the [composer shape](#shape), `wrapDraft`'s wrapping without dictionary word boundaries, the caret-independent layout, the persistent window with hidden-row counts on the edges, D1's window height, the notice above the header, the frame fallback, the text [activity line](#activity-line) with its shimmer, for a sample turn and a sample compaction that Ctrl+T steps through, the [modes table](#modes) for idle, running, compacting, and inspection, batched input with one synchronized repaint per batch, and autowrap turned off while it owns the screen. These gaps from the oracle remain:

| Preview behavior | Oracle behavior | Source |
|---|---|---|
| Tab opens the sample-agent list | Tab accepts completion; Ctrl+G opens the subagent sheet, and Down from an empty composer selects the subagents row | `keys.rs`, `BINDINGS` |
| A word in Thai, Lao, Khmer, or Myanmar splits between graphemes | Those scripts break at dictionary word boundaries | `editor.rs`, `layout` |
| The caret is the terminal cursor | D2 proposes the same; the oracle draws a reverse-video cell | `render.rs`, `render` |
| Every wake draws a frame | A batch of runtime updates alone draws at most every 16 ms | `terminal.rs`, `run_loop`; there is no runtime port yet |
| The idle placeholder names editing keys: `Type a draft · Alt+Enter newline · Ctrl+Z undo` | `Ask anything · / commands · @ files` | `copy.rs`, `PLACEHOLDER`; the preview has no commands or file mentions |
| Enter shows the no-model notice in every mode, and Alt+↑ does nothing | Enter starts, steers, or queues a turn by mode; Alt+↑ sends steering now | `state.rs`, `composer_key`; there is no runtime port yet |
| The completion and masked sign-in modes do not exist | Each has its own row in the modes table | `mode.rs`, `Mode`; they arrive with completion and sign-in |

## Delivery slices

These slices are ordered PRs inside [scope 14](README.md#14--terminal-engine-and-rendering). Each keeps `bun run dev:rust` runnable and the shipped TypeScript frontend unchanged.

1. **Split and update loop.** Implemented. `bake-tui-view` holds the `Msg`/`Effect` update function and the key-binding table; `bake-tui` decodes input and runs the channel-driven loop. The preview's tests moved with the code, and the screen is unchanged. `Msg` has no mouse or runtime variant until slices 4 and 6 add them, and `Effect` has only `Quit`.
2. **Composer parity.** Implemented for the idle, running, compacting, and inspection rows of the modes table, driven by the sample activity; the box, wrapping, tab stops, caret column, window, and edge labels came before it. The completion and sign-in rows wait for slice 5 and provider login.
3. **Chrome.** The layout planner, the header's activity line and summary, the rules with frame glyphs, status fitting by rank, and the standing rows.
4. **Transcript.** The fixture port, row presentation for prose, user turns, and tool cards, the fullscreen viewport with its anchor and keys, and live rows.
5. **Panels.** Completion with a sample catalog, notices, double-press Ctrl+C, pending input, and attachments.
6. **Pointer and resilience.** Mouse wheel, click-to-caret, selection and copy, resize during streams, and broken-pipe handling.
7. **Inline qualification.** The renderer experiment that [the terminal direction](terminal.md#rust-crate-selection) requires.

## Verification

[The terminal direction](terminal.md#acceptance-scenarios) owns the acceptance scenarios; these layers support them.

- **Pure tests.** Editor and wrap cases ported from the TypeScript [editor tests](../../../apps/tui/packages/ui/tests/editor.test.ts) at every width from 4 to 29. Invariants: no row exceeds its width, no wrapped row opens with a space, the row count does not depend on the caret, and the caret stays inside the window.
- **Cross-runtime comparison.** Wrap and caret fixtures run through both `wrapDraft` and the Rust wrapper in the [comparison harness](../../../conformance/README.md), so a parity claim rests on shared input rather than on reading two implementations.
- **Buffer tests.** Ratatui `TestBackend` renders of every mode and panel at 40×12, 80×24, and 120×36, every size up to 12×8, the D1 heights, a resize sequence that keeps the caret inside the box, and both frames, in color and under `NO_COLOR`.
- **PTY tests.** The existing `bun run test:rust:pty` scenarios, which check autowrap and synchronized-output restoration, extended with cursor-position reads after edits and resizes, mouse restoration, and a composer that grows and shrinks while output arrives.
- **Performance.** Keystroke-to-frame latency with a 256 KiB draft, and frame cost while streaming, measured with an extended [performance diagnostic](../../../apps/tui/packages/app/performance/README.md) before any claim is made.

## Open decisions

| Decision | Options | Recommendation |
|---|---|---|
| Thai, Lao, Khmer, and Myanmar word boundaries | ICU4X `icu_segmenter` with compiled data, or a smaller dictionary | ICU4X, after measuring binary size and licensing; needs a dependency qualification record |
| Window maximum (D1) | Five rows always, or `clamp(rows / 5, 5, 12)` | The formula; it matches the oracle below 30 rows |
| Frame override | Environment detection only, or a setting like the TypeScript `composerFrame` | A setting, for terminals whose environment describes them wrongly; the preview has none |
| Caret drawing (D2) | Terminal cursor, or a reverse-video cell | Terminal cursor, with reverse video kept as a setting if a terminal misplaces it |
| Crate split | `bake-tui-view` plus `bake-tui`, or one crate with module rules | The split; only a crate boundary keeps Crossterm out of the pure code |
| Inline renderer | Ratatui inline viewport, or a Bake-owned writer | Decide by the slice 7 PTY experiment |

## Dev Note

Non-authoritative. The 16 ms runtime-only frame interval and the D1 formula are starting values to be tuned with measurements. A future difference worth testing is dimming the transcript while an approval is open in fullscreen, so the decision reads as modal; the oracle collapses the live region instead, and that is not part of this proposal.
