# Layout implementation plan

Turns `DESIGN-LAYOUT.md` into staged work. The visual reference is `prototype/frames.mjs`, which renders every state through Ink's own layout engine and fails if any dynamic region exceeds `rows - 1`.

```sh
node tui/prototype/frames.mjs            # all scenes, 80 cols, with color
node tui/prototype/frames.mjs --plain    # no ANSI, for docs and diffs
node tui/prototype/frames.mjs --rows 10  # prove the collapse order on a short window
node tui/prototype/stability.mjs         # measure screen movement across a streaming turn
node tui/prototype/realloop.mjs          # same claims against Ink's real render loop
node tui/prototype/overlays.mjs          # completion and picker geometry
```

`realloop.mjs` is the one that settles arguments: it drives a live `render()` through a fake TTY, captures every byte, and reproduces the L1 violation on demand. All three exit non-zero on failure and are safe to wire into `check.sh`.

The prototype imports no fork source. It resolves Ink and React out of `packages/ui` and can therefore run while that package is being edited.

## What the prototype already settled

Three decisions came out of building it rather than out of discussion:

1. **The rail needs a fixed-width box, not flex shrink.** A two-column `<Box width={2} flexShrink={0}>` beside a growing content box keeps wrapped continuation lines hanging at the text column. Letting the gutter and content share a shrinking row misaligns every wrap — visible in the first prototype run, where `without` and `rather` slid back under the glyph.
2. **A multi-line notice is one block, not N bulleted lines.** The glyph marks where the block starts; continuation lines keep the indent without repeating it.
3. **Status at 80 columns drops `session` on its own.** Six fields plus separators overrun 80, so the priority-drop rule is load-bearing at ordinary widths, not just narrow ones.

## Stages

### S1 — L1 and L2 regression gate (do first)

The one test that makes every later stage safe. The method is already proven in `prototype/realloop.mjs`; this stage ports it onto the real component tree.

- Capture raw stdout for a scripted turn; assert no full-screen clear appears.
- Assert each committed transcript row is written exactly once.
- Assert frame height is constant while a turn runs (L2).
- Assert dynamic height `<= rows - 1` for each state at 80×24 and 40×10.
- Wire all three prototype scripts into `check.sh`.

Measured cost of not having this gate, on one identical turn: transcript rows redrawn 8 times instead of once, 7 screen clears instead of none, and 6.6× the bytes at a 200-row transcript. Catches today's `/help` and `/model` overflow and every future unbounded overlay.

### S2 — `Row` and the rail

Replace ad-hoc row rendering with the prototype's `Row`: fixed rail box, kind-keyed glyph and color, hanging wrap. Gutter table is `DESIGN-LAYOUT.md` §4.

Depends on the other agent's `Transcript`/`Row` refactor landing; this stage adopts whatever type they settle on.

### S3 — Bounded regions

Apply the §5 rule to every dynamic list.

- `Notice` gains a budget and the `+N more` line.
- Reroute `/help` and `/model` full output into the **transcript**, where it is static, scrollable, and free. The notice keeps only short feedback.
- This is what makes S1 pass for the commands shipped today.

### S4 — Live region, and L2 stability

Tail-window the in-flight turn to `min(10, rows - reserved)` lines. Clipped lines are not lost; they commit to the transcript at turn end. Collapse to one line whenever an interaction is open.

Carries invariant L2 (`DESIGN-LAYOUT.md` §8.1): **pad the live area to its budget while a turn runs, release at idle.** Measured, this removes all five rows of screen travel from a seven-frame turn. This is the single change that makes the TUI feel calm, and it is cheap — padding, not architecture.

Gate: `prototype/stability.mjs` asserts zero height change and exits non-zero otherwise.

### S5 — Status line

Priority-ordered fields, dropped right-to-left, never wrapping. Adds the `turnBoundary` turn/step counter — the last `DESIGN.md` §3b projection not yet surfaced. `turnBoundary` exposes `lastTurn`; a step *number* is not in the projection, so step display needs `step/start` payloads or it stays turn-only.

### S5b — Completion and picker geometry

Applies to the `Completion`/`FileCatalog`/`Picker` work already in flight (`DESIGN-LAYOUT.md` §7a).

- `Picker`'s `limit` prop comes from `useWindowSize()`, not a constant: `rows - chrome - header - 1`. At 10 rows that is 5 items; a constant tuned for 24 rows overflows and clears the screen.
- Anchor the completion overlay **below** the composer so the caret does not move as results change.
- Hold the last loaded row count through a re-query; do not hold to the limit.
- Paths truncate from the start; path rows drop the description column.
- Gate: `prototype/overlays.mjs` checks L1 at 10/16/24/40 rows and L2 across loading, loaded, empty and error.

### S6 — Interaction overlay

Bordered modal, keyboard-first, never yields under collapse. Outranks the live region because an unanswerable prompt is a deadlock.

### S6b — Terminal ownership (no debris)

Enforce that nothing but Ink writes to the terminal while mounted (`DESIGN-LAYOUT.md` §8.3).

- Inject the TUI's own sink into `app-boot`'s four `warn` parameters so the default `process.stderr.write` never reaches the terminal.
- Enable Ink's `patchConsole` for plugin `console.*`; it does not cover direct `process.stderr.write`, so the injection above is still required.
- Surface captured output as a notice plus a transcript row, or route it to a log file. Never drop it.
- Gate: a scripted turn asserts nothing reaches stderr while mounted.

This is correctness, not polish — a single stray warning corrupts the display for the rest of the session.

### S7 — Resize and input polish

- `useWindowSize` for `SIGWINCH`; recompute budgets, never cache `columns`/`rows`.
- `useCursor` to publish cursor position for IME composition — required for CJK, which `--locale zh` implies.
- **Resolve the paste overlap.** Ink's `usePaste` owns bracketed-paste mode, and `app/src/terminal.ts` enables it for the pre-mount window. Two owners of `\x1b[?2004h` is a bug waiting to happen; pick one and document it.

### S8 — Accessibility and degradation

- `useIsScreenReaderEnabled`: discrete state transitions instead of spinner frames.
- Honor `NO_COLOR` and non-TTY; color stays semantic (§6).
- Below ~10 rows, draw interaction-or-composer plus status only.

## Sequencing

S1 gates everything. S3 is the only stage that fixes a shipped defect, so it follows immediately. S2 and S4–S6 are independent once the row type settles.

The three stages that decide whether this feels professional are **S4 (L2 stability)**, **S6b (no debris)**, and **S3 (bounded regions)** — they remove, in order, the jumping, the corruption, and the screen-clearing. S7 and S8 are polish, except S7's paste overlap, which is correctness.

## Open questions

- **Who owns bracketed paste** — Ink or `terminal.ts`? (S7)
- **Step numbering**: surface `turn` only, or read `step/start` payloads for `turn · step`? (S5)
- **Emit-width wrapping**: accepted in §2.1 as the cost of native scrollback. Revisit only if resize-reflow complaints appear in practice.
