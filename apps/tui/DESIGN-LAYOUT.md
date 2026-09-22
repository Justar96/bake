# TUI layout and structure

How the terminal surface is composed, and the measured constraints that decide it. Companion to `DESIGN.md`, which owns state authority (§3b) and event projection (§4.3). This file owns geometry, render cost, and visual language.

Ink version in this fork: **7.1.1**.

## 1. The constraint everything else follows from

Ink 7 writes a frame one of two ways, chosen by the height of the **dynamic** (non-`<Static>`) region:

| Dynamic height vs. viewport rows | What Ink writes per frame |
| --- | --- |
| `< rows` | erase the previous dynamic region, rewrite it |
| `>= rows` | `clearTerminal` + **the entire accumulated static output** + the dynamic region |

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

### Committed history survives resize

One `Static` instance owns the entire displayed session. Ink clears its accumulated static output when that instance changes, so remounting it for every appended row loses earlier history on a resize that requires a screen replay. The transcript adapter supplies `length` and `slice(index)` from immutable batches, matching Ink 7's consumption of its items. It reads only the unprinted suffix and keeps the static node stable until a session switch.

## 2. Region model

Six regions, top to bottom. Only the first is static.

```
┌───────────────────────────────────────────────┐
│ transcript        <Static>, terminal-owned     │  unbounded, written once
├───────────────────────────────────────────────┤
│ live region       current turn, streaming      │  <= 10 rows, elastic
│ turn header       word, phase, newest thought  │  1-2 rows; 1 after, as the summary
│ interaction       approval / prompt overlay    │  <= 8 rows, modal
│ notice            command output, errors       │  <= 6 rows, dismissible
│ composer          input                        │  1-5 rows, elastic
│ status            one line                     │  1 row, fixed
└───────────────────────────────────────────────┘
```

Budgets sum to 25 at maximum, which exceeds a 24-row window — deliberately. They are priorities, not reservations. §4 defines how they collapse.

### 2.1 Transcript — the terminal owns scrolling

Committed rows go to `<Static>` and are never re-rendered. Consequences accepted on purpose:

- **No scrollable pane, no alt-screen.** Native scrollback, terminal search, mouse selection, and copy all keep working. A custom pager would break every one of them to gain nothing a terminal does not already do well.
- **No retroactive edit.** A row is final once written. Compaction rewrites therefore append a notice rather than mutating history (`DESIGN.md` §4.3, Option A).
- **Wrapping is baked at emit width.** A row wrapped at 100 columns keeps those breaks after a resize to 60. Ink cannot reflow what it has already released, and the alternative — keeping the transcript dynamic so it can reflow — violates L1 immediately. Emit-time wrapping is the cost of native scrollback.

### 2.2 Live region — the only elastic tall element

Holds the in-flight turn: streaming assistant text, running tool calls, step progress. Streaming reasoning is not drawn here; the turn header carries it ([§2.2a](#22a-turn-header--one-steady-line-for-the-whole-turn)).

Streaming output is unbounded by nature, so the live region does not hold it. The application prints each finished line of the answer to the transcript as it completes, and the live region draws only the line still arriving and any call still streaming ([§8.1](#81-the-input-follows-the-newest-line)). What remains is still **tail-windowed** to `N = min(10, rows - reserved)` rows, section by section. Only the newest section is cut, and an older one is shown whole or not at all.

### 2.2a Turn header — one steady line for the whole turn

While a turn runs, the row above the composer's frame names it:

```
  the config loader reads DSH_HOME before the profile
✻ Kneading…  thinking · 12s
```

The word is drawn from the locale's `activityWords` when the turn starts, seeded by the session and its transcript length, and kept until the turn ends. It does not follow the phase, so the header reads as one thing that holds while the details beside it change. The phase comes from the newest live row: `thinking` while reasoning streams, `writing` while the answer streams, and `running <tool>` while a call streams or has committed without its result. `/stop` replaces the word with `Stopping` in red. The word is drawn in the header's warm accent, and while a clock runs a lighter band three characters wide crosses it, one character per spinner frame, from off the left edge to off the right.

The row above it is the reasoning ticker. It shows the newest non-empty line of the streaming reasoning, dim and cut at the terminal width, and disappears once the answer or a call starts. Reasoning arrives faster than anyone reads it. Drawn row by row, it grew the live region to its limit and then scrolled every row of it with each token. One line replaced in place shows that the model is working, and roughly on what, at a height that does not change. It sits above the header rather than under it, with the output it summarizes, so the header is always the row resting on the input and nothing streams beneath it. The full text still commits to the transcript with the rest of the step.

The header claims its rows after the live region, so on a short terminal it gives way to the output. An open interaction hides it, because the question is what the turn is waiting on.

When the turn ends, its row stays and says how the turn went:

```
✓ Completed  42s · edited 1 · ran 2 · read 3 · 1 failed
```

The glyph and label follow the recorded turn end: a green `✓ Completed`, a yellow `■` followed by why the turn stopped (`Interrupted`, `Blocked`, `Output token limit reached`), and a red `✗ Failed` for an error, whose message can run to paragraphs and stays in the transcript. A completed turn draws no line of its own in the transcript; `- Completed` there repeated this row. The elapsed time is the header's clock when the turn ended. The counts are the turn's committed calls grouped by past-tense verb, with edits first, followed by the number of calls that failed. They are read from the transcript rather than tallied as the turn ran, so they agree with the session log however results arrived. Removing the header at the end of the turn moved the input up a row and left the outcome to be found in scrollback; kept, the row answers the question the running header posed, in the same place. It holds until the next turn starts. A resumed session shows its newest ended turn the same way, without a time, because no clock watched it run; that turn is read from the transcript once, when the surface mounts, so later commits never read history again.

### 2.3 Interaction — modal, and it wins

Approvals and prompts take priority over the live region. A user answering "may I run `rm -rf`?" does not need concurrent token streaming; they need the command, the cwd, and the choices, unambiguously. When an interaction is open the live region collapses to a single summary line.

### 2.4 Notice — transient, bounded, dismissible

Command results, errors, hints. Cleared by the next submit. Bounded per §5.

### 2.5 Status — one line, degrades by priority

Fields in priority order, dropped right-to-left as width shrinks:

```
status  model  context  turn  cwd  session
```

Left-packed, two spaces apart, stopping where the fields stop — not justified to both edges, per [§6a](#chrome-separates-by-framing-the-input-not-by-aligning-the-status-line). It sits under the composer's frame, indented to the prompt inside it, as the frame's footer. Never wraps to two lines; a wrapped status line silently costs a row of live region and can tip L1.

### 2.6 Composer — a cursor-following window

The composer displays one to five physical rows inside a full-width frame. It wraps the draft at its actual available width, including the marker rail and contextual hint, before choosing the visible rows. Moving Home, End, or through the middle of a wrapped paragraph keeps the drawn caret visible. The hint appears beside the caret, and `^` marks text above the window. Wide characters use terminal cell widths. IME candidate placement at the caret is not yet implemented.

`chromeFor` accounts for the frame, status, spacing, and first draft row. On a short terminal, spacing yields first, then status, then the frame; the input row remains visible. At 40×4, the three available rows hold the frame and one input row. Below 40 columns, the frame is omitted. Additional draft rows are reserved before panels claim the remaining space.

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

### The frame is chosen from the terminal, not assumed

The composer is framed in box-drawing characters (`╭─╮`), which two kinds of terminal cannot draw:

- One that is **not encoding UTF-8** writes the bytes through as mojibake, so the frame becomes punctuation on every row.
- One configured to draw **East Asian Ambiguous** characters two cells wide draws a full-width horizontal run at twice the width Ink measured. The frame wraps, and Ink's own row arithmetic is wrong from then on — this is the damaging case. Every other Ambiguous character on this surface (the turn marker, the selection pointer, the caret) sits alone in a fixed-width rail, where a terminal that draws it wide shifts one row by one column; a border run accumulates that error across the whole line.

`resolveFrame` in `packages/app/src/frame.ts` decides once, before the first frame, and `@dsh-tui/ui` takes the answer as a prop — the presentation layer reads no environment. Encoding and `TERM` come from `LC_ALL`/`LC_CTYPE`/`LANG` in POSIX order; ambiguous width is a terminal *preference* and cannot be detected, so a CJK character locale (or `--locale zh`) stands in for it. `composerFrame` in the profile overrules the lot, which is the only answer for a terminal the environment describes wrongly.

The ASCII fallback is laid out against the same widths and spends the same rows, so `CHROME_ROWS` is one number for every terminal.

### The composer yields structure before it yields content

The frame costs four columns — two of border, two of padding — and the hint costs whatever its locale needs. Both are structure around the one field on the surface the user is actually composing in, so both give way before it does, at a named width rather than by shrinking:

| Below | What goes | Why not shrink it |
| --- | --- | --- |
| `HINT_MIN_COLUMNS` (60) | the composer's right slot | a hint truncated to fit has stopped being help, and the key it names still works unnamed |
| `FRAME_MIN_COLUMNS` (40) | the border and padding | at §4's supported minimum the frame is a tenth of the line being typed on; the rail and the status line below already say where input lands |

Above those widths the hint never shrinks, so the draft takes whatever is left and wraps around it. The hint is drawn outside the clipped stack and aligned to its bottom, so it sits beside the caret's row wherever wrapping puts it — inside, it lands beside the first row of a wrapped line and notches the paragraph's top right while the caret row runs to the full width.

### ASCII only, and actions are named rather than pictured

Symbol glyphs are a measurement risk before they are a style question. `string-width` reports `U+2699` as one cell and `U+2699 U+FE0F` as two, and a terminal with an emoji font may draw the bare codepoint double-width anyway. Ink measures with that same library, so when the terminal disagrees, every column after the glyph shifts and **nothing in the layout engine can detect it** — `renderToString` measures it the same wrong way. Characters below `0x80` cannot disagree.

So the render vocabulary is a **verb column**: a named action, its argument, and any output aligned beneath it.

```
> Find where the session controller registers commands
  think  The registry is the list, so discovery should read it.
  run    rg -n "commands.register" -g '*.ts'
         packages/app/src/controller.ts:45
         packages/app/src/controller.ts:52
  read   packages/app/src/controller.ts
  Two registrations, both through ctx.effect.
> Ask anything, / for commands
ready   deepseek/chat                       ctx 12%   turn 3   0f3a9c
```

`run`, `read`, `think`, `ask`, `error` read at a glance, survive every font and locale, and stay legible pasted into a bug report. Reasoning and approvals join the same grammar instead of inventing their own marks, which is why `think` and `ask` are verbs rather than symbols.

| Row kind | Marker | Columns | Color |
| --- | --- | --- | --- |
| user | `>` | text at 2 | default, bold |
| assistant | none | text at 2 | default |
| action | `●`, then a verb | verb at 2, argument at 9 | marker accent and pulsing while running, green when done, red on failure; verb bold |
| action output | none | aligned at 9 | default, red on failure; red/green for diffs; `+N more lines` dim |
| reasoning | `think` | as a verb row | dim |
| interaction | `ask` | as a verb row | cyan |
| error | `error` or a red verb | as a verb row | red |
| list selection | `*` | marker at 0 | cyan |
| composer | `>` | text at 2 | cyan, bold |

`*` marks a selection and a current value; `>` is the composer prompt and a user's own words. They are never swapped: two identical markers a row apart read as one list.

`prototype/ascii.mjs` renders the vocabulary and fails if any rendered character is above `0x80`.

## 5. Bounding unbounded content

Any dynamic region rendering a list of harness-owned length needs the same treatment. The rule:

> Render at most `budget` lines. When more exist, render `budget - 1` and a final `+N more` line. When the full list matters, commit it to the **transcript** — static, scrollable, free — instead of holding it in the dynamic region.

This is the principled fix for `/help` and `/model`: a command's full output belongs in scrollback, where the terminal can scroll it, not in a dynamic overlay that is capped by L1. The notice region is for *short* feedback — "model set for the next turn" — not for catalogs.

"Free" is about the buffer, not about the reader. A row costs the transcript nothing to keep and costs the reader a scroll to get past, so the escape hatch holds for content a user asked to see — they typed `/help` — and fails for content that arrives whether or not they wanted it. Tool output is the second kind, and [§7b](#a-results-output-is-previewed-not-replayed) bounds it.

## 6. Color, motion, accessibility

- **Color is semantic only**: red failed, yellow awaiting user, cyan asking, green an added line or a finished action, dim reasoning and secondary interface metadata such as line counts and a call's description. The turn header takes its own warm accent (`ACCENT` in `activity.ts`), because it means none of those. Tool arguments and output use normal brightness. Never decorative. Honor `NO_COLOR` and non-TTY.
- **Weight carries structure**: an action's marker, verb, and tool name are bold, and so is `error`, so a scan down the left of the transcript lands on each action rather than on its arguments.
- **Spinners only on a TTY** with color enabled. Under `useIsScreenReaderEnabled`, replace motion with discrete state transitions — a screen reader announcing a spinner frame-by-frame is unusable. The UI package reads no clock. `App` times and animates the turn header only when the terminal owner passes a `clock` prop, and animates it only while `motion` is not false. The runner always passes the clock and turns motion off under `NO_COLOR`, so the header's glyph rests at `✻`, its word does not glint, a running action's `●` does not pulse, and only the elapsed seconds advance, once a second. Without a clock, or under a screen reader, the header also leaves out the elapsed time and changes only when the phase does.
- **Resize** via `useWindowSize`, which re-renders on `SIGWINCH`. Recompute budgets from it; never cache `columns`/`rows`.
- **Paste** via Ink's `usePaste`, which owns bracketed-paste mode and keeps pasted text off the `useInput` channel. Our `terminal.ts` enables paste mode for the pre-mount window; these must not fight over the same escape sequence — one owner, chosen explicitly.

## 7. What this rules out

Recorded so the questions do not get relitigated:

- **Split panes / sidebars.** Every column spent on chrome is taken from prose and tool output, which are the product. A terminal is not a window manager.
- **A scrollable transcript pane.** §2.1.
- **Full-screen alt-screen mode.** It discards scrollback on exit, so the session vanishes when the program does. The transcript surviving exit is the point.
- **Progress bars for model output.** Token counts are not a denominator; there is no total to divide by.

## 6. Spacing and zones inside the chat area

`prototype/chat.mjs` renders one turn at 80 and 160 columns.

### Prose is measured; tool output is not

Prose wraps at **88 columns** however wide the terminal is. A 160-character monospace line is hard to track back to its start, and terminals get arbitrarily wide. Tool output takes the full width instead, because wrapping a log or a diff to a narrow measure destroys the column alignment that makes it scannable.

This is the one place where the chat area deliberately does not fill the window, and the reason is legibility rather than decoration.

### A line wraps, never truncates

A line the user cannot finish reading is worse than a ragged one. Long output lines wrap; they are never cut with an ellipsis. Horizontal truncation is reserved for surfaces where the full value is one keystroke away — a completion row or a picker — never for a line of a result that already cost a tool call.

How many lines of a result are drawn at all is a separate question, answered by [§7b](#a-results-output-is-previewed-not-replayed). Every line that is drawn is drawn whole.

### A call and its result are one zone

A call and its result draw as one block, opened by a `●` at the rail. While the call runs, the marker pulses in the accent and the verb is present tense: `run`, `read`, `find`. When the result arrives, the same row changes in place: the marker turns green, or red on failure, and the verb becomes past tense: `ran`, `read`, `edited`, `found`, `got`. No second row announces the outcome, and no call id is drawn; the id identified a result's row with its call, and there is no longer a separate row to identify. Output indents to the output column beneath the head, so no whitespace divides a call from what it produced.

`Actions` in `packages/ui/src/actions.ts` does the merge. It holds each call until its result and releases the merged rows in call order, so a fast second call cannot print above a slow first one, and it settles every held call when the model's message or the turn end arrives. The live region draws the held calls; nothing is committed and later drawn again.

Tool arguments and output use normal brightness. The marker and verb are bold; a call's description and a `+N more lines` count are dim. Failures retain red emphasis throughout, and diffs retain red/green emphasis. Among reasoning, actions, and answers, only reasoning is dimmed.

### Indentation separates columns; a blank row separates sections

Within a turn the levels are: rail glyph at column 0, prose at 2, tool output and reasoning at 9. Indentation carries every separation it can, and it carries most of them — an answer at the rail is never confused with output under a verb.

Two things it cannot carry. Two actions in a row share the verb column, so a `think` drawn directly under the previous call's output sits at that output's indent and reads as more of it. And nothing at all divides reasoning from the answer that follows it: the answer moves two columns left and stops being dim, which under `NO_COLOR` is a two-column shift and nothing else — the working-out and the conclusion read as one paragraph.

So **a blank row opens each section**: a reasoning block, a tool call, and the answer. Nothing else gets one. A result continues the call above it, and a command's notice continues the command, so neither ever floats away from what produced it. A section with no content produces no blank either, since a blank belongs to the lines under it; an empty block is the everyday case while a turn is still streaming, and on its own it would be a reserved row spent on nothing.

That is one blank per section rather than one per row. Separating every row would stripe the transcript, and striping reads as noise once a session is long — exactly when the transcript matters most. In scrollback the cost falls on an unbounded buffer; in the live region it is one row of a window capped at `LIVE_BUDGET` whatever it contains.

```
● Use the bash tool to run exactly: echo TERMINAL_OK

  think  The user wants me to run a command and then reply.

  run    {"command": "echo TERMINAL_OK"}
         TERMINAL_OK

  think  The command ran. I should reply with just "DONE".

  DONE
```

## 6a. Separating the chat area, the status line, and the composer

The three regions must read as three kinds of thing at a glance. A terminal offers few ways to say that, and they do not cost the same. `prototype/separation.mjs` renders the candidates with their row cost.

### Spacing is cheap above and expensive below

A blank line inside the transcript is charged to scrollback, which is unbounded. A blank line in the dynamic region is charged to the L1 budget the live region needs — on a 10-row window that is a tenth of the screen. The same pixel of whitespace has two very different prices depending on which side of the boundary it falls.

So: **spend rows on structure inside the chat area, spend none on chrome.**

### Chrome separates by framing the input, not by aligning the status line

| Candidate | Chrome cost | Verdict |
| --- | --- | --- |
| status as one more sentence | 2 rows | reads as another chat row; its stray leading space looks like a mistake |
| full-width rule | 3 rows | draws the eye to a line carrying no information |
| justified status bar | 2 rows | superseded: on a wide terminal the justification opens a gap the width of the screen between the model and the next field, and the row stops reading as one bar |
| boxed composer alone | 4 rows | the box says where typing lands, but its top edge butts against the last line of the answer |
| blank, status line, boxed composer | 5 rows | superseded: the status line stood between the answer and the input, so the eye crossed metadata on every return to the prompt |
| **blank, boxed composer, status line** | **5 rows** | **adopted** |

The composer sits inside a full-width box directly under the newest line, one blank row opens it, and the status line is a plain left-packed list under the box, indented to the prompt:

```
  DONE

╭─────────────────────────────────────────╮
│ > Ask anything, / for commands            │
╰─────────────────────────────────────────╯
  ready  deepseek/chat  ctx 12%  turn 3  0f3a9c
```

The box is the separator, so the status line does not also have to be one. A closed shape reads as chrome before a single word inside it is read, it says where typing lands without color or copy, and it survives `NO_COLOR` and a screen reader. Unlike a bare rule it encloses something, so the row it spends is structure rather than decoration. Its border is chosen from the terminal ([§4](#the-frame-is-chosen-from-the-terminal-not-assumed)).

The input is the row the eye returns to after reading an answer, so nothing but the blank stands between them. The status line is read far less often, and under the box it reads as the box's caption: the indentation ties it to the prompt above it, and nothing below it can be mistaken for output.

The blank row buys the one thing the box cannot do: without it the box's top edge sits directly under the last line of the answer, and the input reads as attached to the output rather than as the place the next turn starts. Anything that belongs to the input rather than to the conversation — completion, a notice, queued input, quit feedback — is drawn between the blank and the box, so the blank opens the whole stack instead of splitting it.

Five rows is the largest chrome cost this document accepts, and `CHROME_ROWS` in `packages/ui/src/layout.ts` charges all five against every other region's budget. The rows the blank and the frame add come off the live region on a window shorter than 16 rows and off nothing at all above that, because the live region is capped at `LIVE_BUDGET` first.

### The composer is the same family, one weight heavier

A user message opens with `›`; the composer opens with `❯`. Same chevron, more weight: *what you said* against *where you speak*. Related roles should look related; only their weight should say which is active.

### Quit feedback stays beside the input

`Ctrl-C` arms quitting and displays one row of feedback directly above the composer. The runner owns the timer and armed state independently of command notices, so neither replaces the other.

### The right slot is contextual, never decorative

The composer's right edge can hold `↵ send`, `esc interrupt` while a turn runs, or `↑↓ select` while an overlay is open. It must stay contextual. A permanent hint teaches nothing after the first day and becomes noise on a surface the user looks at hundreds of times a day — the same reason nothing here animates on keystroke.

### Turn boundaries, not message boundaries

One blank line opens each user turn inside the transcript. Separating every message would stripe the screen; separating turns gives the eye a place to land when scrolling back through a long session, which is the actual task.

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

### Keep completion on top of the composer

Completion results occupy a bounded panel directly on top of the input's frame, below the blank that opens the chrome. Opening the panel pushes the composer down by its height, or scrolls history up once the screen is full; closing it gives the rows back. Candidate descriptions truncate to one row, and overflow counts remain inside the shared budget.

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

Reasoning streams as the turn header's one-line ticker while it happens, and commits to the transcript as a single `thought for 8s` line. It is worth watching live and rarely worth re-reading; the full text stays in the session log, which is the durable record under **Model-visible ⇔ logged**. Replaying it into scrollback would bury the answer under the working-out.

How much to keep is deployment-varying, so it is a validated `Config` field (`summary` / `full` / `hidden`), not a constant — a debugging session wants `full`, a demo wants `hidden`.

### A result's output is previewed, not replayed

A tool result once committed every line it returned, so one `ls -la` wrote seventy rows of scrollback and one glob wrote a hundred paths. Measured through Ink's real render loop, that single commit is 5.6 kB across five frames, and it scrolls the terminal by the length of the output every time a tool runs.

The next design held the head of the output in the live region until the model answered, and committed only the outcome. That fixed the length and broke the motion. The body appeared under the call, then vanished when the answer began, and on a full screen the composer rose by its height and fell back as the answer streamed.

So the transcript commits the outcome and a preview together, once:

```
● ran    ls -la
         total 280
         drwxr-xr-x@  0 ...
         +66 more lines
● read   packages/ui/src/app.tsx  412 lines
```

A command's output, an edit's diff, and any failure are previewed: the first `resultLines` lines follow at the output column, and a dim `+N more lines` counts the rest. A read, a search, or a fetch reports only its size, on the head line, because the text it returned is what the model reads rather than what the reader needs. A headline that only repeats the call's title is dropped. Nothing on screen is held and later removed, so a result never moves the composer after it prints, and a resumed session draws exactly what the live one did.

The card's headline is held apart from the card's lines for exactly this reason. `70 lines` does not say which file was edited, and `Edit packages/ui/src/app.tsx` is the one line of an applied diff that still means something after the hunks have scrolled away.

The full text is in the session log, which is the durable record under **Model-visible ⇔ logged** — the same guarantee that lets reasoning collapse. Nothing is lost; it is one `dsh` session read away rather than one scroll away, and the answer stays on screen.

`resultLines` is a validated `Config` field, defaulting to 4, because how much of a result belongs in scrollback is deployment-varying in the same way reasoning is: a debugging session wants more, and a deployment reading its transcripts out of a log rather than off the screen wants `0`.

### Reasoning is distinguished by geometry, not color

Reasoning sits at **column 4**, the answer at **column 2**. Dim alone merges the two under `NO_COLOR` and for a screen reader; indentation survives both. This is why §6's "color is semantic only" is not sufficient on its own — semantic color still needs a non-color carrier.

### The live region shows the call, not only the words

A turn that reasons, calls a tool, and then answers spends most of its wall time inside the call. If the live region draws only text and reasoning, that stretch is blank: the surface looks idle at the moment the agent is busiest, and the action first appears already finished, committed to scrollback.

So the live region projects tool-call blocks alongside text and reasoning, in stream order, and `LiveBlocks` in `packages/app/src/live.ts` is what folds the chunks into them. It is deliberately not `BlockAssembler.interruptedBlocks()`, which answers a different question — what an interrupted attempt may safely finalize into a durable message — and drops every call because interruption precedes dispatch. Display writes nothing and carries no such obligation.

`end` still drops the whole live copy, per the table above: it is published once the assistant message has committed, so the transcript already holds the rows these stood in for.

### Tool arguments: never render partial JSON

`tool-call-delta` carries `argumentsDelta`, so arguments arrive character by character. Rendering them live shows the user `{"comm` and then `{"command": "rg -n \"comm`, which reads as a malfunction. A streaming call keeps its verb and its name and puts an ellipsis where the argument goes — and keeps the ellipsis after `block-end` too, because the complete arguments are raw JSON and the committed row presents them properly a moment later:

```
● run    bash                    while the call streams

● ran    echo TERMINAL_OK        committed, through the tool's presenter
         TERMINAL_OK
```

### Each tool presents as its own summary

A call reads as the thing it does: `bash` as its command, a search as its pattern, a file tool as its path. The fallback is an argument count, never a JSON dump. Presenters stay pure and live with the tool, matching the repository rule that every tool's UI presentation is designed up front.

## 8. Robust rendering: no flicker, no jumping

Two distinct failures, two distinct causes. Neither is fixed by drawing faster.

### 8.1 The input follows the newest line

The dynamic frame is sized to its content. Committed history prints through `Static`, the live region follows it, and the chrome follows the live region with one blank row between. Nothing pads the frame, so on a fresh session the input sits directly under the session heading, and a short answer leaves the input directly under the answer — the way Claude Code places its prompt.

> **Layout invariant L2.** The newest transcript or live line, one blank row, and the composer's frame are adjacent at every size and in every state, with the turn header and input-owned panels (completion, notices, queued input, quit feedback) drawn between the blank and the frame. Once the screen is full the terminal scrolls history, and the composer rests at the bottom with the status line and Ink's cursor row beneath it.

The live region has no reserved height. It grows a row per streamed row up to `budget.live`, then keeps its newest rows. `tailLines` chooses them in wrapped rows, measured the way Ink wraps `Text`, so the window the lines are chosen for is the one they are drawn in.

#### A streaming answer prints as it completes

The answer does not wait for its commit to reach the transcript. `Printed` in `packages/app/src/printed.ts` prints each finished line of the streaming answer to `Static` as its newline arrives. A text or reasoning block prints whole once a later block starts. Nothing past the first tool call prints, because the call commits through its own event. The live region keeps only the line still arriving. When the message commits, `reconcile` drops from it what already printed, so each line appears once. A block the commit changed prints again whole, since duplication stays readable.

This removes two failures of drawing the answer in the live region. An answer longer than the window was shown clipped until it committed. And every row the window gained or lost moved the composer on a full screen. Printed a line at a time, the text scrolls into the terminal's history the way the terminal would scroll it, while the frame holds one line and does not change height. Once history fills the screen the composer stays on the bottom row. `placement.spec.tsx` streams twelve wrapped paragraphs this way and asserts the input row never moves.

Printing is display, not record. The session log commits the message as before, and a resume draws it whole. Scrollback cannot be unwritten, so lines printed from an attempt that never commits stay; the transcript follows them with a notice that they were discarded.

#### What remains in the window

Only the newest section is cut; an older one is shown whole or not at all. A cut section keeps its opening blank, drawn outside the clip, and gets back the verb or reply marker the cut removed. Answer prose fills the window exactly, with its oldest line clipped from the top the way a terminal scrolls. Stopping at whole lines would leave the window a row or two short whenever the next line wraps. Tool output keeps whole lines, so its verb stays on screen. The window claims its rows from the shared budget only while it has rows to draw: a turn that has not produced output yet leaves those rows to the panels.

Growth and commits move the input down; a panel closing, or live output committing into fewer rows, moves it back up. That motion is the cost of keeping the input next to what was just said, and it is the motion the user caused. Existing history is neither traversed nor reprinted during typing or streaming, and the frame stays under `rows - 1` (L1) because every region claims from one budget.

#### Narrowing repaints

Ink erases the previous frame by counting its lines. A terminal that reflows on resize has already re-wrapped each full-width row of that frame — the composer's borders — onto two rows, so the count falls short and the rows it misses stay on screen above the new frame. Ink clears the terminal and replays history only when a frame overflows the viewport, so for the one render after the terminal narrows `App` draws a spacer of viewport height above the controls. That frame is cleared and replayed; the next one, overflowing no longer, is cleared and replayed again at the normal size. A narrowing therefore costs two replays of the transcript, which is what a resize cost when the frame filled the screen.

`packages/ui/tests/placement.spec.tsx` replays Ink's actual ANSI writes in a terminal emulator at 80×24, 40×10, and 120×40. It checks the input's adjacency to the newest line through repeated turns, commits, menus, feedback, wrapped Unicode, a full screen, and narrowing and widening resizes. Ink may clear and repaint during a resize; ordinary streamed turns do not trigger full-screen clears.

### 8.2 Flicker — a frame is observed half-drawn

Ink 7 wraps writes in DEC mode 2026 synchronized-update markers, so a supporting terminal presents each frame atomically:

```js
export const bsu = '\u001B[?2026h'
export const esu = '\u001B[?2026l'
export function shouldSynchronize(stream, interactive) {
  return 'isTTY' in stream && stream.isTTY && (interactive ?? !isInCi)
}
```

It also throttles with `leading: true, trailing: true`, so many state updates in one tick coalesce into one write. Both are automatic and neither needs configuration — but mode 2026 is terminal-dependent (kitty, WezTerm, iTerm2, Ghostty, Windows Terminal support it; Apple Terminal does not). On a terminal without it, the defense is to write less. The runner and the development harness render with Ink's `incrementalRendering`, so a frame rewrites only the lines that changed: a spinner tick rewrites the header row, and a streamed token rewrites its own line, not every row of the controls.

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

Taken together: during a turn the transcript holds still, the live area grows under it, and the composer and status line follow the newest line down until the screen is full, then rest at the bottom. The only other vertical motion is the transcript committing rows at turn boundaries — motion the user caused and expects.

## 9. Verification

Layout claims are mechanically checkable and should be gated:

| Claim | How it is proven |
| --- | --- |
| L1 holds in every state | render each state at 80×24 and 40×10, assert dynamic height `<= rows - 1` |
| append cost is O(1) | `packages/ui/tests/scale.spec.tsx` (raw stdout capture, not `frames`) |
| no full-clear in a normal turn | assert `ansiEscapes.clearTerminal` never appears in captured stdout for a scripted turn |
| status never wraps | render at 40, 80, 200 columns, assert one line |
| L2: the input follows the newest line | `packages/ui/tests/placement.spec.tsx` checks actual terminal rows across streaming, commits, idle transitions, menus, feedback, a full screen, and resizing; `live.spec.tsx` checks the frame grows a row per streamed row and no further than the live budget |
| a running turn shows what it is doing | `packages/app/tests/live.spec.ts` streams reasoning, a call, and an answer through `LiveBlocks` and asserts all three appear, in order, with no arguments rendered |
| a windowed section stays named | `packages/ui/tests/present.test.ts` cuts a long block below its verb line and asserts `tailLines` restores the verb onto what is left, shows an older section whole or not at all, and counts wrapped rows; `live.spec.tsx` asserts a wrapped answer keeps its blank and holds its height once full |
| a streaming answer prints once | `packages/app/tests/printed.spec.ts` prints finished lines, keeps paragraph breaks, and reconciles the commit; `session.spec.ts` streams through the controller, resumes to one message, and marks an abandoned attempt's lines discarded; `placement.spec.tsx` holds the input row through twelve printed paragraphs on a full screen |
| the composer cannot outgrow its budget | `packages/ui/tests/line.spec.tsx` renders a 4,000-character paste at several widths and asserts the row count against `COMPOSER_BUDGET`, with the caret on the last row |
| the chrome fits every supported width | `packages/ui/tests/line.spec.tsx` renders `Chrome` at 24, 40, 60, 80 and 200 columns and asserts no row exceeds the width, and that the hint and frame drop at their named thresholds |
| the frame matches the terminal | `packages/app/tests/frame.test.ts` resolves every environment that cannot draw box characters; `line.spec.tsx` asserts both styles reach the edges and spend identical rows and columns |
| the turn header holds for the turn | `packages/ui/tests/live.spec.tsx` keeps one word through thinking, writing, a commit, and a running tool; holds the frame height through 30 lines of reasoning; checks with a fake clock that the spinner and elapsed time advance and the interval is disposed when the turn ends; and checks that with motion off only the seconds change. `activity.test.ts` covers the pure pieces |
| the header's row holds the finished turn | `packages/ui/tests/live.spec.tsx` ends a clocked turn and asserts the summary takes the header's row at the same frame height, survives a notice, reports failures, clears when the next turn starts, and summarizes a replayed session's last turn untimed; `activity.test.ts` covers the counts and outcomes |
| a call and its result are one block | `packages/ui/tests/fold.test.ts` merges results into their calls, releases out-of-order results in call order, and settles held calls; `styles.spec.tsx` and `actions.spec.tsx` assert the running, done, and failed markers, past-tense verbs, and no call ids |
| quit feedback sits above the input | `packages/ui/tests/live.spec.tsx` checks one row of feedback above the input alongside command notices |
| no foreign terminal writes | scripted turn asserts nothing reaches stderr while mounted |

The third is the direct regression test for §1 and the one most worth adding first. The last two are the regression tests for §8.1 and §8.3, which are what the user actually perceives as quality.
