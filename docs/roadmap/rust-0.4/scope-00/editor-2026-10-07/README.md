# Native composer qualification: 2026-10-07

## Summary

Use a pure Bake-owned editor model with Ratatui rendering. The compiled `ratatui-textarea` 0.9.3 probe produced 15 mismatches and three passes against selected Bake editor cases. Its cursor, deletion, width, and wrapping primitives do not meet the current composer requirements. This record informs the [terminal direction](../../terminal.md); it neither implements the native frontend nor closes a Rust scope.

## Table of Contents

- [Decision and observations](#decision-and-observations)
- [Reproduce the probe](#reproduce-the-probe)
- [Evidence limits](#evidence-limits)
- [Dev Note](#dev-note)

## Decision and observations

The comparison uses the [editor tests](../../../../../apps/tui/packages/ui/tests/editor.test.ts) and [caret tests](../../../../../apps/tui/packages/ui/tests/caret.test.ts) at `7bacdd4cd3702cbaedf634a14d820bc10ba4142b`. The [probe source](src/main.rs) names the owning test for each assertion. [Complete observations](observations.txt) retain every expected and actual result, including the passes.

| Operation | Bake requirement | Observed widget behavior |
|---|---|---|
| Move right across `👩🏽‍💻` | Stop after the whole grapheme | Stop after `👩`, inside the sequence |
| Backspace after `👩🏽‍💻` | Remove the whole grapheme | Remove only `💻`, leaving the rest of the sequence |
| Delete before `e` plus combining acute | Remove the base and combining mark together | Remove only `e` |
| Place the caret after `x👩🏽‍💻` | Cell 3 | Cell 7, although the Ratatui buffer renders the emoji in two cells |
| Wrap Thai and Lao text | Preserve the current dictionary word breaks | Break inside words |
| Move down across a short row | Restore the remembered target column on the next long row | Keep the short row's column |

The CJK wrapping case, movement onto a wide CJK character, and ASCII path/punctuation word stops passed. A CJK word-motion case failed because the widget did not preserve Bake's inner word stop. The full output also records hanging whitespace, wrapped-row navigation, and Thai/Lao grapheme cases.

Default bindings differ too: Enter inserts a newline, Ctrl-U undoes, and Ctrl-J deletes to the line head. Bake can intercept those keys; binding differences alone do not reject the widget. The primitive mismatches require control over editor state and layout. Keep draft text, grapheme-boundary cursor positions, remembered columns, grouped undo, the kill ring, and placeholder atoms in one pure Bake model. Use one width calculation for wrapping, drawing, and the caret.

This decision rejects the tested widget version as the composer model. Ratatui rendering and Crossterm terminal control retain their own [adoption conditions](../../terminal.md#rust-crate-selection). A later widget version needs a new qualification record before adoption.

## Reproduce the probe

The manifest, lockfile, source, and output here are frozen experiment inputs and observations. Copy this directory to an ignored scratch directory before running Cargo. These files are not a workspace member, a shipped dependency, or a required CI test; the [scope-01 workspace](../../README.md#01--rust-workspace-and-native-eval-arm) remains planned.

The executed environment was Linux x64, rustc 1.99.0 (`b940084d7`, LLVM 23.1.1), and cargo 1.99.0 (`5f94df478`). The locked graph resolves `ratatui-textarea` 0.9.3, Ratatui 0.30.2, Crossterm 0.29.0, `unicode-width` 0.2.2, and `unicode-segmentation` 1.13.3. The dependency tree contains one Crossterm version.

The following commands were executed in `.preflight/rust-next/editor-probe` with these inputs:

```sh
cargo build --locked
cargo tree -i crossterm
cargo run --locked -q
```

Build passed. The run exited **1**, deliberately reporting the 15 compatibility mismatches. A zero exit would mean all selected cases matched; changing the expected values to obtain it would change the acceptance requirements. The initial buffer reader mistakenly included the unused trailing cell of a wide CJK character. Correcting that reader made the CJK wrapping case pass; the other 15 mismatches remained. Its exploratory output stays in ignored scratch and is not pooled with this record.

The lockfile SHA-256 is `a1ceb303513e063e0785b42b1ca051f7d6c9c8afa86b85d54a89e7dab606d1df`. Keep committed records intact and put future measurements in a new record.

## Evidence limits

These are pure widget and buffer observations. They do not qualify a real terminal, IME placement, mouse behavior, resize, inline scrollback, restoration, Windows ConPTY, or performance. The probe does not exercise attachment atoms, kill-ring integration, undo grouping, selection, or the full editor suite. No live model call or shipped runtime change occurred.

The executable probe uses version 0.9.3. A source comparison with 0.9.2 found identical cursor, word, wrap, screen-map, and history modules; the changed files concerned gutter width and scroll-offset documentation. That comparison is supporting source evidence, not a second executed version result.

## Dev Note

Non-authoritative: the Rust editor still needs a dictionary word-break dependency for Thai, Lao, Khmer, and Myanmar. Evaluate that dependency against the existing editor fixtures when implementing scope 14.
