# Bake Rust terminal preview

## Summary

Run Bake's native fullscreen composer, edit multiline Unicode drafts, and inspect sample agents while preserving your draft. This is an opt-in preview for the [Bake 0.4 Rust roadmap](../docs/roadmap/rust-0.4/README.md).

The preview shows sample content only. It does not connect to a model, read credentials or the Bake home, write sessions, or start agents. The shipped TypeScript terminal is unchanged and remains the default. This preview does not complete any roadmap scope.

## Table of Contents

- [Run it](#run-it)
- [Keys](#keys)
- [Supported subset](#supported-subset)
- [Terminal ownership](#terminal-ownership)
- [Layout of this workspace](#layout-of-this-workspace)
- [Checks](#checks)

## Run it

Prerequisites: [rustup](https://rustup.rs) and the platform linker it needs: a C compiler toolchain on Linux and macOS (`build-essential` or equivalent, or the Xcode Command Line Tools), and the Visual Studio C++ Build Tools (MSVC) on Windows. `rust-toolchain.toml` pins Rust 1.99.0 with rustfmt and Clippy; run Cargo from this directory so rustup selects it.

From the repository root, `bun run dev:rust` builds and opens the preview. Directly with Cargo:

```sh
cd rust
cargo run --locked -p bake-cli -- preview    # fullscreen preview; needs an interactive terminal
cargo run --locked -p bake-cli -- --help     # also printed when no arguments are given
cargo run --locked -p bake-cli -- --version
```

The binary is `bake-rs`. `preview` refuses to start, before changing any terminal mode, when standard input or output is not a terminal (exit status 1). Unknown commands and extra arguments fail with a diagnostic (exit status 2).

## Keys

| Key | Composer | Sample-agent list | Inspector |
|---|---|---|---|
| Text, paste | Insert at the caret | Shows the list keys | Ignored, with a read-only notice |
| Enter | Keeps the draft and shows "Model connection is not available in this preview" | Inspects the selected agent | Read-only notice |
| Alt+Enter, Ctrl+J | Newline | | |
| Left, Right | Move by grapheme | | |
| Home, End | Start or end of the logical line | | |
| Backspace, Delete | Delete one grapheme | | |
| Ctrl+Z | Undo | | |
| Up, Down | | Select by agent id | |
| Tab | Opens the list | Returns to the composer | Returns to the list |
| Esc | Stops the sample turn or compaction and dismisses the notice | Returns to the composer | Returns to the composer |
| Ctrl+T | Steps the sample: a turn, then a compaction, then none | | |
| PgUp, PgDn | Page the transcript | | |
| Ctrl+↑, Ctrl+↓ | Bring the previous or next prompt to the top of the transcript | | |
| Ctrl+Home, Ctrl+End | Go to the transcript's start, or follow its newest line | | |
| Ctrl+C | Quits immediately | Quits immediately | Quits immediately |

The parent draft, caret, and undo history are unchanged by opening the list, inspecting an agent, or moving through the transcript.

## Supported subset

- **Editing:** grapheme-safe insertion and deletion, including emoji ZWJ sequences, combining marks, regional-indicator flags, CJK, and Thai. When a deletion joins its neighbors into one grapheme, such as removing the newline between a letter and a combining mark, the caret moves to the joined grapheme's edge in the direction of the deletion and no text is dropped. Typing runs undo together; each paste, newline, or run of deletions is its own step. Undo keeps 64 steps; the draft holds at most 256 KiB, and longer input is cut at a grapheme boundary with a notice.
- **Input cleanup:** CRLF and CR become LF. Control characters other than LF and tab are dropped so they cannot become escape sequences. A tab is kept and drawn to the next four-cell stop.
- **Paste:** bracketed paste inserts text without submitting.
- **Wrapping:** the draft wraps as the TypeScript composer's `wrapDraft` does: rows break after spaces and tabs, blanks at a break hang past the row, a wide character is a word of its own, and a word longer than a row splits between graphemes. Rows never depend on the caret, so moving it never moves a word or changes the composer's height; a caret after a full row sits in the cell after the text. Wrapping, tab stops, and the caret share the cell widths the renderer draws with, and the terminal cursor sits at the caret.
- **Composer box:** from 12 columns, a rounded box frames the draft, `│ ❯ text │`, in the two rows the rules above and below it would take. The header, notice, agents row, and status line are inset two cells to align with the box's contents. A terminal without a UTF-8 locale, with no `TERM` or `TERM=dumb`, or with a Chinese, Japanese, or Korean character locale gets `+`, `-`, `|`, and a `>` prompt; Windows Terminal's `WT_SESSION` counts as UTF-8. Narrower than 12 columns, the box keeps only its top and bottom edges as plain rules.
- **Window:** the composer shows up to five rows, or a fifth of the terminal's height up to 12 rows on a terminal of 30 rows or more. The window keeps its position and moves only when the caret would leave it; after a width change the caret keeps its row in the window where the new layout allows. Dim `^` and `v` mark hidden rows, and the box's edges count them, `+N above` and `+N below`. - **Modes:** the composer reads differently while idle, while the sample turn runs, while the sample compaction runs, and while an agent is inspected. Only the empty draft's placeholder, the bottom edge's hint, and the prompt's tone change, so a mode change never rewraps the draft or moves the caret. The empty draft reads `Type a draft · Alt+Enter newline · Ctrl+Z undo` while idle, `Enter steers the next step · Alt+↑ sends now` during a turn, `Compacting… Enter queues · Esc cancels` during compaction, and `Read-only · Esc returns to parent` when inspection finds it empty. The bottom edge reads `Enter sends` over a draft while idle, `Esc interrupts` during a turn, nothing during compaction, and `draft kept · Esc returns` during inspection, where the prompt and draft are dim. Narrower than 60 columns, the placeholder keeps only its first part and only the inspection label remains. A hidden-row count takes the bottom edge before any hint. Enter in every mode shows the no-model notice, and Alt+↑ does nothing.
- **Activity line:** Ctrl+T starts a sample of the header a running turn shows, as text alone: a word picked from 32 baking verbs, then its phase and elapsed time, `Kneading…  thinking · 3s`. The phase steps through sample phases every four seconds. A band of light sweeps across the word every 70 ms, blending from orange to a pale glint when `COLORTERM` is `truecolor` or `24bit`, and stepping through yellow, light yellow, and bright white otherwise. A second Ctrl+T replaces it with a sample compaction, `Compacting history…  preparing · 0s`, with its own clock and phases, in blue blending toward light blue, or blue, light blue, and bright white. Under `NO_COLOR` the word is bold and still, and only the seconds change. Nothing runs during either sample.
- **Layout:** fullscreen alternate screen at any size. From the bottom up: the agents row, the composer box, the bar on the box's top edge, the notice, and the transcript body. Rows go to the first composer row, the bar, the box's top edge, its bottom edge, the agents row, the gap, further draft rows while the body keeps three rows, and the notice, in that order, so a short terminal gives up the notice, the gap, the agents row, the bottom edge, the top edge, and the bar first; the box closes only when both edges fit. Events already waiting are applied together and drawn once, so a burst of keys or resizes repaints once. Every resize clears the screen and repaints the whole frame inside one synchronized update, even when a burst of resizes ends at the previous size.
- **Transcript:** the body above the controls is a viewport over a fixed sample session; nothing in it ran. It opens with the preview's title and introduction, then two turns. The user's words run beside a cyan bar, `▎ Run the parser tests`; reasoning is dim and italic and answers are plain, both at column 2. Each call is one row, `✓ Bash  bun test tests/parser.test.ts`, with its state mark (green `✓` done, red `✗` failed, orange `●` running), the tool name bold in an aligned column, and a summary right-aligned: its own, such as `exit 1` or `412 lines`, or else its output's line count. Output hangs from a dim `│` gutter, red when the call failed. A result longer than five lines shows its first two and last two lines around `⋯ 4 more lines`. A blank row opens each turn, reasoning block, group of calls, and answer; a call that follows a call joins its group. The classic frame draws `|`, `+`, `x`, `*`, and `...` instead. Lines wrap at the viewport's width, never truncate. The viewport follows the newest line until PgUp, Ctrl+↑, or Ctrl+Home moves it; while reading, its position holds, and paging down to the bottom or Ctrl+End follows again. A page is the viewport less four lines, and at least half of it. On a body of four rows or more, the foot of the viewport is a hint row: `PgUp scroll · Ctrl+↑ prompts` right-aligned while following and the history is taller than the viewport, `↓ Latest · Ctrl+End` while reading. Only the rows the viewport visits are laid out.
- **Agents:** the standing row under the box reads `Agents  2 samples · none running`, with `Tab` at the right. Tab opens the agent list in the body: `Fixed examples · nothing is running`, then a card for each agent, its name, its id right-aligned, and the first line of what it does; the selected card is marked `▸` (`>` with the classic frame) and drawn in cyan. Enter inspects the selected agent: its name and id, `Read only · typing never reaches an agent or your draft` in yellow, what it does, and that the draft is kept. The bar reads `Agents  ↑↓ select · Enter inspect · Esc back` or `Inspecting  Tab agents · Esc draft`.
- **Bar:** one row directly above the box holds what the session is doing on the left and the status line right-aligned beside it, `Kneading…  thinking · 3s        deepseek-v4-flash high  ctx ~11%  ~/bake ⎇ main`. The left side is the sample's activity, the last sample turn's outcome, held until the next turn starts (`✓ Completed  8s` when Ctrl+T moves it on to a compaction, `■ Interrupted  3s` when Esc stops it), the agent list's title and keys, or the inspected agent; idle, it is empty. As the row narrows, the left side's phase and time give way first, then status fields by rank, then the status line whole; the activity word is never cut. Right-aligned, the status does not move when the activity starts or ends. The agents row's `Tab` key draws only on a terminal 60 columns or wider and only whole.
- **Status line:** minimal and consolidated into three fields: the model with its thinking level, `deepseek-v4-flash high`; the context window's occupancy, `ctx ~11% (15.2k/128k)`; and the location, the working directory under home with the git branch after it, `~/bake ⎇ main`. A reading the session lacks is left out; the preview has no model, so it reads `no model  ~/bake ⎇ main`. The branch comes from the repository's `HEAD` file at startup, a detached commit shows as `(1a2b3c4)`, and the classic frame writes `on main` instead of `⎇ main`. Fields give way by the TypeScript status line's ranks: the context's absolute count; then the directory, which takes only what the other fields leave, cut from its start so the branch stays, until fewer than six of its cells fit and the branch stands alone; then the branch; then the thinking level. The model is cut last, to eight cells, and the context percentage never yields. The level and the context reading take the TypeScript palette's warming tones.

Not implemented: model connection, sessions, real agents, inline (scrollback) mode, dictionary word breaks for Thai, Lao, Khmer, and Myanmar, a setting that overrides the frame style, word movement, vertical caret movement, kill/yank, history recall, completion, attachments, redo, mouse, keyboard-enhancement protocols, and the double-press Ctrl+C quit.

## Terminal ownership

`TerminalSession` in `bake-tui` is the only code that changes terminal modes: raw mode, the alternate screen, bracketed paste, autowrap, cursor visibility, and synchronized output. Autowrap is off while the preview runs, so a row the terminal draws wider than measured is clipped instead of scrolling the screen. Each frame is written between synchronized-update markers (DEC mode 2026), and an update left open by a failure is ended on restore. It records each mode before requesting it and restores exactly those modes, in reverse order, attempting every step even when one fails:

- On quit or a handled signal, the preview closes the session explicitly. A restoration failure is reported (`bake-rs: preview failed: ...`, exit status 1) rather than discarded.
- On a loop error, the session is still closed; the loop error is reported, and a restoration error behind it is not.
- On a setup failure, only the modes already requested are restored.
- On a panic, a panic hook restores the terminal before the panic message prints. Dropping the session also restores it; restoration runs at most once, so these fallbacks never repeat it.

On Unix, SIGINT, SIGTERM, and SIGHUP are handled while the preview runs. Raw mode turns the Ctrl+C key into input, so SIGINT comes only from another process. A signal thread forwards each one to the loop as it arrives; the loop restores the terminal and exits with status 128 plus the signal number: 130, 143, and 129. After the terminal is restored the handlers are unregistered, but signal-hook does not reinstate the previous disposition, so those signals are ignored in the moment before the process exits. A second signal during restoration does not interrupt it, and SIGKILL or SIGSTOP cannot be handled at all.

An input thread reads terminal input as it arrives, decodes it into the view's messages, and sends them to the loop. It checks every 50 ms whether to stop, and the preview joins it before leaving raw mode, so it reads nothing meant for the shell. If the thread fails or ends unexpectedly, the loop reports `terminal input stopped` and restores the terminal. Between messages the loop sleeps until the next one, or until the sample activity's next shimmer beat or second.

## Layout of this workspace

| Crate | Contents |
|---|---|
| `crates/bake-tui-view` | Pure presentation, with no terminal backend, threads, or clock: `editor` (draft and wrapping), `composer` (box geometry, window, and edges), `frame` (glyph choice from environment values), `activity` (activity words, shimmer, and colour levels), `keys` (Bake's key type and the binding table), `mode` (the composer's modes), `layout` (the row planner), `status` (the status line's consolidated fields and their fitting by rank), `copy` (product text), `transcript` (sample rows, their presentation, and the viewport), `state` (presentation state, `Msg`, `update`, and `Effect`), and `render` (Ratatui drawing) |
| `crates/bake-tui` | The terminal owner, Crossterm input decoding, the branch read from `HEAD`, the input and signal threads, and the event loop that drives `bake-tui-view` |
| `crates/bake-cli` | The `bake-rs` binary: argument parsing, terminal checks, exit statuses |
| `crates/bake-conformance` | A synthetic fixture runner for the [migration comparison harness](../conformance/README.md); no agent runtime |

Dependencies are pinned exactly in `Cargo.toml` and locked in `Cargo.lock`: Ratatui 0.30.2 with only its `crossterm` feature, its own `ratatui-core` 0.1.2, which `bake-tui-view` uses directly, Crossterm 0.29.0, unicode-segmentation 1.13.3, unicode-width 0.2.2, and on Unix signal-hook 0.3.18, which Crossterm already uses. `ratatui-crossterm` enables Crossterm's default features; `cargo tree --locked -i crossterm` shows one Crossterm version.

The conformance runner uses Serde 1.0.229 and serde_json 1.0.151 for its separate, versioned test input. These dependencies do not connect the preview to a model or session store.

## Checks

```sh
cd rust
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo build --workspace --locked
```

Unit tests cover grapheme editing (including deletions that join neighboring graphemes), input cleanup, undo bounds, draft limits, word wrapping, tab stops, rows that do not depend on the caret at every width below 30, the composer window and its resize behavior, box geometry at every width up to 200, the frame choice, the activity word, shimmer sweep, colour levels, and narrowing, the key-binding table and Crossterm decoding, the row planner's claim order, transcript presentation, result previews, paging, prompt jumps, and following, status fitting by rank (the TypeScript `fitStatus` cases), turn summaries, refused submission, read-only inspection, draft restoration after navigation, selection by id, the event loop's batching, signal, input-failure, and timed-redraw paths with a scripted screen, and rendering at 40×12, 80×24, 120×36, and every size up to 12×8 with Ratatui's `TestBackend`. A unit test covers the stream check for each combination of terminal and non-terminal input and output, and `crates/bake-cli/tests/cli.rs` runs the built binary without a terminal to check help, version, argument errors, and the refusal to start. Terminal-mode restoration in a real PTY is checked by the repository's PTY scenarios, not by these tests.

From the repository root, `bun run preflight --only native` runs those Cargo checks and `bun run test:rust:pty` against the built binary. The PTY driver checks composer input, caret and undo preservation through inspection, refused submission, multiline paste, resize, a sample activity whose elapsed time advances without input and draws no spinner glyph, and terminal restoration, including autowrap and synchronized output, after Ctrl+C, SIGINT, SIGTERM, and SIGHUP. A Linux process-stop barrier exercises a shrink and grow that reach the application as one resize. That barrier is skipped on macOS; all native PTY scenarios are skipped on Windows until ConPTY coverage is implemented. Cargo checks run on Linux, macOS, and Windows in CI.

The native preflight group also runs `bun run test:rust:conformance` on all three operating systems. It compares the built Rust fixture runner with the TypeScript arm and independently observes final files. See the [harness guide](../conformance/README.md) for its synthetic scope, deliberate mismatches, and report format.
