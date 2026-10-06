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
| Esc | Dismisses the notice | Returns to the composer | Returns to the composer |
| Ctrl+C | Quits immediately | Quits immediately | Quits immediately |

The parent draft, caret, and undo history are unchanged by opening the list or inspecting an agent.

## Supported subset

- **Editing:** grapheme-safe insertion and deletion, including emoji ZWJ sequences, combining marks, regional-indicator flags, CJK, and Thai. When a deletion joins its neighbors into one grapheme, such as removing the newline between a letter and a combining mark, the caret moves to the joined grapheme's edge in the direction of the deletion and no text is dropped. Typing runs undo together; each paste, newline, or run of deletions is its own step. Undo keeps 64 steps; the draft holds at most 256 KiB, and longer input is cut at a grapheme boundary with a notice.
- **Input cleanup:** CRLF and CR become LF. Control characters other than LF and tab are dropped so they cannot become escape sequences. A tab is kept and drawn as four cells.
- **Paste:** bracketed paste inserts text without submitting.
- **Wrapping:** the draft wraps at grapheme boundaries using the same cell widths the renderer draws with; the terminal cursor sits at the caret. The composer shows up to five rows and scrolls so the caret row stays visible.
- **Layout:** fullscreen alternate screen at any size. Short terminals drop the gap, rules, agents row, status, and header before the first composer row; the body takes what remains. Every resize event clears the screen and repaints the whole frame, even when a burst of resizes ends at the previous size.

Not implemented: model connection, sessions, real agents, inline (scrollback) mode, word wrapping and word movement, vertical caret movement, kill/yank, history recall, completion, attachments, redo, mouse, keyboard-enhancement protocols, and the double-press Ctrl+C quit.

## Terminal ownership

`TerminalSession` in `bake-tui` is the only code that changes terminal modes: raw mode, the alternate screen, bracketed paste, and cursor visibility. It records each mode before requesting it and restores exactly those modes, in reverse order, attempting every step even when one fails:

- On quit or a handled signal, the preview closes the session explicitly. A restoration failure is reported (`bake-rs: preview failed: ...`, exit status 1) rather than discarded.
- On a loop error, the session is still closed; the loop error is reported, and a restoration error behind it is not.
- On a setup failure, only the modes already requested are restored.
- On a panic, a panic hook restores the terminal before the panic message prints. Dropping the session also restores it; restoration runs at most once, so these fallbacks never repeat it.

On Unix, SIGINT, SIGTERM, and SIGHUP are handled while the preview runs. Raw mode turns the Ctrl+C key into input, so SIGINT comes only from another process. The loop restores the terminal and exits with status 128 plus the signal number: 130, 143, and 129. Input is polled with a 250 ms timeout so pending signals are observed without busy-waiting. After the terminal is restored the handlers are unregistered, but signal-hook does not reinstate the previous disposition, so those signals are ignored in the moment before the process exits. A second signal during restoration does not interrupt it, and SIGKILL or SIGSTOP cannot be handled at all.

## Layout of this workspace

| Crate | Contents |
|---|---|
| `crates/bake-tui` | `editor` (pure draft and wrapping), `app` (preview state and keys), `render` (Ratatui drawing), and the terminal owner |
| `crates/bake-cli` | The `bake-rs` binary: argument parsing, terminal checks, exit statuses |

Dependencies are pinned exactly in `Cargo.toml` and locked in `Cargo.lock`: Ratatui 0.30.2 with only its `crossterm` feature, Crossterm 0.29.0, unicode-segmentation 1.13.3, unicode-width 0.2.2, and on Unix signal-hook 0.3.18, which Crossterm already uses. `ratatui-crossterm` enables Crossterm's default features; `cargo tree --locked -i crossterm` shows one Crossterm version.

## Checks

```sh
cd rust
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo build --workspace --locked
```

Unit tests cover grapheme editing (including deletions that join neighboring graphemes), input cleanup, undo bounds, draft limits, wrapping and caret placement, refused submission, read-only inspection, draft restoration after navigation, selection by id, and rendering at 40×12, 80×24, 120×36, and every size up to 12×8 with Ratatui's `TestBackend`. A unit test covers the stream check for each combination of terminal and non-terminal input and output, and `crates/bake-cli/tests/cli.rs` runs the built binary without a terminal to check help, version, argument errors, and the refusal to start. Terminal-mode restoration in a real PTY is checked by the repository's PTY scenarios, not by these tests.

From the repository root, `bun run preflight --only native` runs those Cargo checks and `bun run test:rust:pty` against the built binary. The PTY driver checks composer input, caret and undo preservation through inspection, refused submission, multiline paste, resize, and terminal restoration after Ctrl+C, SIGINT, SIGTERM, and SIGHUP. A Linux process-stop barrier exercises a shrink and grow that reach the application as one resize. That barrier is skipped on macOS; all native PTY scenarios are skipped on Windows until ConPTY coverage is implemented. Cargo checks run on Linux, macOS, and Windows in CI.
