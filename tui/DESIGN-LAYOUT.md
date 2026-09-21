# TUI layout and structure

How the terminal surface is composed, and the measured constraints that decide it. Companion to `DESIGN.md`, which owns state authority (§3b) and event projection (§4.3). This file owns geometry, render cost, and visual language.

Ink version in this fork: **7.1.1**.

## 1. The constraint everything else follows from

Ink 7 writes a frame one of two ways, chosen by the height of the **dynamic** (non-`<Static>`) region:

| Dynamic height vs. viewport rows | What Ink writes per frame |
| --- | --- |
| `<= rows` | erase the previous dynamic region, rewrite it |
| `> rows` | `clearTerminal` + **the entire accumulated static output** + the dynamic region |

The second path is in `build/ink.js`:

```js
// renderInteractiveFrame
const isFullscreen = isTty && outputHeight >= viewportRows;
...
this.options.stdout.write(ansiEscapes.clearTerminal + this.fullStaticOutput + outputToRender);
```

`fullStaticOutput` is every transcript row ever emitted, concatenated in memory (`this.fullStaticOutput += staticOutput`). So the moment the dynamic region reaches terminal height, per-frame cost stops being O(1) and becomes O(whole transcript) — the exact failure `packages/ui/tests/scale.spec.tsx` exists to prevent. `<Static>` alone does not buy the guarantee; **`<Static>` plus a bounded dynamic region** does.

`prototype/realloop.mjs` drives Ink's real `render()` through a fake TTY and captures every byte. One identical turn, 24-row viewport:

| | transcript row draws | full-screen clears |
| --- | --- | --- |
| dynamic region within budget | 1, 1 | 0 |
| dynamic region 30 lines | 8, 8 | 7 |

The cost scales with history, not with the turn — bytes written for the same turn:

| transcript rows | compliant | overflowing | ratio |
| --- | --- | --- | --- |
| 2 | 1,433 | 5,373 | 3.7× |
| 50 | 2,143 | 11,053 | 5.2× |
| 200 | 4,491 | 29,837 | 6.6× |

A long session is exactly where the penalty is worst, which is the opposite of what a user expects and why this is an invariant rather than a guideline.

> **Layout invariant L1.** The dynamic region renders at most `rows - 1` lines at every width, for every state, with no exceptions. Every component below is specified as a height budget because of this.

L1 is not a performance nicety. Crossing it clears the user's screen and replays the transcript on each keystroke, destroying native scrollback position mid-turn.

### Known current violations

Both ship today and both are mine:

- `/help` notices one line per registered command.
- `/model` notices one line per route in the catalog.

On an 80×24 window a composition with ~20 models exceeds the budget on its own. §5 fixes this class rather than these two instances.

## 2. Region model

Five regions, top to bottom. Only the first is static.

```
┌───────────────────────────────────────────────┐
│ transcript        <Static>, terminal-owned     │  unbounded, written once
├───────────────────────────────────────────────┤
│ live region       current turn, streaming      │  <= 10 rows, elastic
│ interaction       approval / prompt overlay    │  <= 8 rows, modal
│ notice            command output, errors       │  <= 6 rows, dismissible
│ status            one line                     │  1 row, fixed
│ composer          input                        │  1-5 rows, elastic
└───────────────────────────────────────────────┘
```

Budgets sum to 25 at maximum, which exceeds a 24-row window — deliberately. They are priorities, not reservations. §4 defines how they collapse.

### 2.1 Transcript — the terminal owns scrolling

Committed rows go to `<Static>` and are never re-rendered. Consequences accepted on purpose:

- **No scrollable pane, no alt-screen.** Native scrollback, terminal search, mouse selection, and copy all keep working. A custom pager would break every one of them to gain nothing a terminal does not already do well.
- **No retroactive edit.** A row is final once written. Compaction rewrites therefore append a notice rather than mutating history (`DESIGN.md` §4.3, Option A).
- **Wrapping is baked at emit width.** A row wrapped at 100 columns keeps those breaks after a resize to 60. Ink cannot reflow what it has already released, and the alternative — keeping the transcript dynamic so it can reflow — violates L1 immediately. Emit-time wrapping is the cost of native scrollback.

### 2.2 Live region — the only elastic tall element

Holds the in-flight turn: streaming assistant text, running tool calls, step progress.

Streaming output is unbounded by nature, so it is **tail-windowed**: show the last N lines, where `N = min(10, rows - reserved)`. Earlier lines are not lost — they land in the transcript when the turn commits. The window is a viewport onto text the user will see in full, one moment later.

### 2.3 Interaction — modal, and it wins

Approvals and prompts take priority over the live region. A user answering "may I run `rm -rf`?" does not need concurrent token streaming; they need the command, the cwd, and the choices, unambiguously. When an interaction is open the live region collapses to a single summary line.

### 2.4 Notice — transient, bounded, dismissible

Command results, errors, hints. Cleared by the next submit. Bounded per §5.

### 2.5 Status — one line, degrades by priority

Fields in priority order, dropped right-to-left as width shrinks:

```
status · model · context · turn · cwd · session
```

Never wraps to two lines; a wrapped status line silently costs a row of live region and can tip L1.

### 2.6 Composer — one to five rows

Grows with content to five rows, then scrolls internally. Cursor position is published via `useCursor` so IME composition lands in the right cell — required for CJK input, which `--locale zh` implies we support.

## 3. Priority and collapse

When budgets exceed the viewport, regions yield in this order (first to yield listed first):

1. **notice** — truncates to its `+N more` form, then to one line
2. **live region** — shrinks toward 3 lines, then to a single status-like line
3. **composer** — shrinks toward 1 line
4. **interaction** — never yields; if it cannot fit, it is the only dynamic content drawn
5. **status** — never yields; 1 row is the floor

Rationale: an unanswerable question is a deadlock, so interaction outranks everything. Status is the cheapest orientation per row in the whole layout.

A window under ~10 rows cannot honor this. There, draw interaction-or-composer plus status and nothing else.

## 4. Width

- **Minimum supported: 40 columns.** Below that, drop the gutter and render prose only.
- **Gutter: 2 columns**, one glyph plus one space, giving every row kind a constant left rail so the eye tracks a single column.
- Text measurement is Ink's (`string-width`), which handles CJK wide cells and emoji correctly. Never use `.length` for layout arithmetic.

| Row kind | Gutter | Color |
| --- | --- | --- |
| user | `›` | default |
| assistant | ` ` | default |
| tool call | `⚙` | dim |
| tool result | ` ` | dim |
| error | `✗` | red |
| notice | `•` | yellow |
| interaction | `?` | cyan |

## 5. Bounding unbounded content

Any dynamic region rendering a list of harness-owned length needs the same treatment. The rule:

> Render at most `budget` lines. When more exist, render `budget - 1` and a final `+N more` line. When the full list matters, commit it to the **transcript** — static, scrollable, free — instead of holding it in the dynamic region.

This is the principled fix for `/help` and `/model`: a command's full output belongs in scrollback, where the terminal can scroll it, not in a dynamic overlay that is capped by L1. The notice region is for *short* feedback — "model set for the next turn" — not for catalogs.

## 6. Color, motion, accessibility

- **Color is semantic only**: red failed, yellow awaiting user, cyan asking, dim supporting detail. Never decorative. Honor `NO_COLOR` and non-TTY.
- **Spinners only on a TTY** with color enabled. Under `useIsScreenReaderEnabled`, replace motion with discrete state transitions — a screen reader announcing a spinner frame-by-frame is unusable.
- **Resize** via `useWindowSize`, which re-renders on `SIGWINCH`. Recompute budgets from it; never cache `columns`/`rows`.
- **Paste** via Ink's `usePaste`, which owns bracketed-paste mode and keeps pasted text off the `useInput` channel. Our `terminal.ts` enables paste mode for the pre-mount window; these must not fight over the same escape sequence — one owner, chosen explicitly.

## 7. What this rules out

Recorded so the questions do not get relitigated:

- **Split panes / sidebars.** Every column spent on chrome is taken from prose and tool output, which are the product. A terminal is not a window manager.
- **A scrollable transcript pane.** §2.1.
- **Full-screen alt-screen mode.** It discards scrollback on exit, so the session vanishes when the program does. The transcript surviving exit is the point.
- **Progress bars for model output.** Token counts are not a denominator; there is no total to divide by.

## 7a. Overlays: completion and pickers

Completion popups and choice pickers draw from unbounded sources — every registered command, every skill, every file in the workspace — into the dynamic region that §1 caps. `prototype/overlays.mjs` renders them and checks the rules below.

### The item limit is derived, never constant

```
rows   no header   with header
  10           6             5
  24          20            19
  60          56            55
```

`limit = rows - chrome - header - 1`, where chrome is status, composer, and one line of margin. A constant tuned on an 80×24 window overflows a split pane, and overflow is the one failure that clears the user's screen. Compute it from `useWindowSize()`.

### Anchor overlays below the composer

Results change on every keystroke. An overlay above the input pushes the line being typed into up and down as the user types; below it, the caret stays put and the overlay grows downward into space the user is not reading. This is the layout's one deliberate inversion of reading order, and it exists because the caret outranks the list.

### Hold the last loaded height through a re-query, not the limit

Height across `loading → loaded → empty → error`:

```
unheld : 2 9 2 2   moves
held   : 9 9 9 9   stable
```

Holding to the **limit** would leave a twenty-row hole under three matches. Holding to the **last loaded row count** keeps the display still across a transient re-query while letting the overlay shrink when the result set genuinely shrinks. L2 applies to states the user did not cause; it does not mean freezing a list that legitimately got shorter.

### Truncate paths from the start

`packages/app/src/module-7.ts` truncated at the end reads `packages/app/src/module…`, hiding the only part that distinguishes it. Paths use `wrap="truncate-start"`; names with descriptions keep a fixed name column so descriptions align. A path row drops the description column entirely rather than repeating `workspace file` forty times.

### Overflow marker

`+N more — keep typing to narrow` tells the user both that the list is cut and what to do about it. A bare count implies scrolling that is not offered.

## 7b. Reasoning stream and tool use

`agent/assistant-stream` delivers `AssistantStreamFrame`s carrying `StreamChunk`s, which separate `reasoning-delta` from `text-delta` and stream tool arguments as partial JSON in `tool-call-delta`. `prototype/stream.mjs` implements the fold from frames to view and checks the rules below.

### Three stream rules that are correctness, not taste

The frame type carries a `revision` that "restarts at 1" on replacement, and an `end` outcome of `committed` or `abandoned`. Each has one right rendering:

| Frame | Rule | Bug if ignored |
| --- | --- | --- |
| `start` with a new `revision` | clear what the previous attempt drew | a retry appends, so the answer appears twice |
| `chunk` with a stale `revision` | ignore it | a late chunk from a replaced attempt corrupts the new one |
| `end` → `committed` | drop the live copy; the session event owns it now | the same text renders live *and* in the transcript |
| `end` → `abandoned` | drop the live copy; commit nothing | abandoned text leaks into scrollback |

The reducer is pure and total, so all four are testable without a model.

### Reasoning is watched, not re-read

Reasoning streams dim and tail-windowed while it happens, and commits to the transcript as a single `thought for 8s` line. It is worth watching live and rarely worth re-reading; the full text stays in the session log, which is the durable record under **Model-visible ⇔ logged**. Replaying it into scrollback would bury the answer under the working-out.

How much to keep is deployment-varying, so it is a validated `Config` field (`summary` / `full` / `hidden`), not a constant — a debugging session wants `full`, a demo wants `hidden`.

### Reasoning is distinguished by geometry, not color

Reasoning sits at **column 4**, the answer at **column 2**. Dim alone merges the two under `NO_COLOR` and for a screen reader; indentation survives both. This is why §6's "color is semantic only" is not sufficient on its own — semantic color still needs a non-color carrier.

### Tool arguments: never render partial JSON

`tool-call-delta` carries `argumentsDelta`, so arguments arrive character by character. Rendering them live shows the user `{"comm` and then `{"command": "rg -n \"comm`, which reads as a malfunction. Until `block-end` delivers the complete block, a call renders as its name and an ellipsis:

```
⚙ bash · …                          arguments still streaming
⚙ bash · rg -n "commands.register"   complete
```

### Each tool presents as its own summary

A call reads as the thing it does: `bash` as its command, a search as its pattern, a file tool as its path. The fallback is an argument count, never a JSON dump. Presenters stay pure and live with the tool, matching the repository rule that every tool's UI presentation is designed up front.

## 8. Robust rendering: no flicker, no jumping

Two distinct failures, two distinct causes. Neither is fixed by drawing faster.

### 8.1 Jumping — the dynamic region changes height

Ink erases the previous dynamic block and rewrites it each frame. A taller block scrolls the terminal to make room and everything on screen moves; a shorter one erases lines under the user's eyes. Either way the status line and composer shift while the user is reading.

`prototype/stability.mjs` renders one streaming turn frame by frame and counts the movement:

```
growing live region    heights 3 4 5 6 7 8 8   5 changes   5 rows of travel
reserved live region   heights 8 8 8 8 8 8 8   0 changes   0 rows of travel
```

`prototype/realloop.mjs` confirms this against the real render loop, reading the heights back out of Ink's own cursor-up moves — `3 4 5 6 7 8` growing, `9 9 9 9 9 9` reserved. Stability costs about 13% more bytes (1,433 vs 1,244 for the same turn), spent rewriting the same cells rather than reflowing the screen.

> **Layout invariant L2.** While a turn is running, the dynamic region holds a constant height. The live area is padded to its budget rather than grown into.

Reserved at idle too would leave dead space below a finished answer, so the rule is scoped: **reserve while running, release at idle.** The calm state stays compact; the busy state stays still. The transition happens once per turn, at a moment the user is already expecting the display to change.

This is also why the status line must never wrap (§2.5) and why notices are bounded (§5): both silently change the dynamic height.

### 8.2 Flicker — a frame is observed half-drawn

Ink 7 wraps writes in DEC mode 2026 synchronized-update markers, so a supporting terminal presents each frame atomically:

```js
export const bsu = '\u001B[?2026h'
export const esu = '\u001B[?2026l'
export function shouldSynchronize(stream, interactive) {
  return 'isTTY' in stream && stream.isTTY && (interactive ?? !isInCi)
}
```

It also throttles with `leading: true, trailing: true`, so many state updates in one tick coalesce into one write. Both are automatic and neither needs configuration — but mode 2026 is terminal-dependent (kitty, WezTerm, iTerm2, Ghostty, Windows Terminal support it; Apple Terminal does not). On a terminal without it, the defense is to write less: L2 keeps the block a constant height, so the rewrite touches the same cells rather than reflowing the screen.

### 8.3 Debris — someone else writes to the terminal

The hazard specific to an agent TUI. Ink tracks its own cursor position to know what to erase. Any write it did not make invalidates that arithmetic, and the display corrupts from then on: doubled lines, orphaned fragments, a status line drifting up the screen.

The harness writes to stderr today. `packages/boot/app-boot/src/index.ts` has four `warn` sinks defaulting to `process.stderr.write`, and **stderr shares the terminal with stdout**. Plugins may also reach `console.*`.

Rules while the TUI owns the terminal:

1. **Nothing else writes to stdout or stderr.** Not a convention — enforced.
2. `app-boot`'s `warn` parameters are injectable; the TUI supplies its own sink rather than letting the default reach the terminal.
3. `console.*` is captured with Ink's `patchConsole`, which writes above the dynamic region correctly. Note it does **not** intercept direct `process.stderr.write` calls, so rule 2 is still required.
4. Captured output surfaces as a notice and is committed to the transcript, or goes to a log file. It is never dropped silently.

Boot-time warnings are unaffected: they fire before the plugin mounts, while the terminal is still ordinary.

### 8.4 What calm looks like

Taken together: during a turn the screen holds still, the live area fills in place, the status line stays on its row, and the composer stays where the user left the cursor. The only vertical motion is the transcript committing rows at turn boundaries — motion the user caused and expects.

## 9. Verification

Layout claims are mechanically checkable and should be gated:

| Claim | How it is proven |
| --- | --- |
| L1 holds in every state | render each state at 80×24 and 40×10, assert dynamic height `<= rows - 1` |
| append cost is O(1) | `packages/ui/tests/scale.spec.tsx` (raw stdout capture, not `frames`) |
| no full-clear in a normal turn | assert `ansiEscapes.clearTerminal` never appears in captured stdout for a scripted turn |
| status never wraps | render at 40, 80, 200 columns, assert one line |
| L2: no jumping while running | `prototype/stability.mjs`; assert zero height change across a turn's frames |
| no foreign terminal writes | scripted turn asserts nothing reaches stderr while mounted |

The third is the direct regression test for §1 and the one most worth adding first. The last two are the regression tests for §8.1 and §8.3, which are what the user actually perceives as quality.
