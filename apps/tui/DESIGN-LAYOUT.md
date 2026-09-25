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
│ live region       current turn, streaming      │  10 rows, 40% of a tall one
│ thinking window   newest reasoning rows        │  0-3 rows + 1 blank, while reasoning streams
│ interaction       approval / prompt overlay    │  <= 8 rows, modal
│ notice            command output, errors       │  <= 6 rows, dismissible
│ header            turn, spinner, goal          │  1 row, fixed; the summary after
│ rule              bare line over the input     │  1 row, fixed
│ composer          input                        │  1-5 rows, elastic
│ base rule         bare line under the input    │  1 row, fixed
│ status            one line, under the base rule│  1 row, fixed
└───────────────────────────────────────────────┘
```

Budgets sum to 25 at maximum, which exceeds a 24-row window — deliberately. They are priorities, not reservations. §4 defines how they collapse.

### 2.1 Transcript — the terminal owns scrolling

Committed rows go to `<Static>` and are never re-rendered. Consequences accepted on purpose:

- **No scrollable pane, no alt-screen.** Native scrollback, terminal search, mouse selection, and copy all keep working. A custom pager would break every one of them to gain nothing a terminal does not already do well.
- **No retroactive edit.** A row is final once written. Compaction rewrites therefore append a notice rather than mutating history (`DESIGN.md` §4.3, Option A).
- **Wrapping is baked at emit width.** A row wrapped at 100 columns keeps those breaks after a resize to 60. Ink cannot reflow what it has already released, and the alternative — keeping the transcript dynamic so it can reflow — violates L1 immediately. Emit-time wrapping is the cost of native scrollback.

### 2.2 Live region — the only elastic tall element

Holds the in-flight turn: streaming assistant text, running tool calls, step progress. Streaming reasoning is not drawn here; the thinking window over the header carries it ([§2.2a](#22a-the-header--one-steady-row-for-the-whole-turn)).

Streaming output is unbounded by nature, so the live region does not hold it. The application prints settled Markdown blocks of the answer to the transcript, and the live region draws the unfinished block and any call still streaming ([§8.1](#81-the-input-rests-on-the-bottom-row)). What remains is still **tail-windowed** to `N = min(max(10, 40% of rows), rows - reserved)` rows, section by section (`LIVE_BUDGET`, `LIVE_SHARE`): ten on an ordinary terminal, a growing share of a tall one, whose empty upper half otherwise folded a running step for no reason, and never more than the chrome leaves. Only the newest section is cut, and an older one is shown whole or not at all.

### 2.2a The header — one steady row for the whole turn

The input is framed by two bare rules, exactly the terminal's width, one over the draft and one under it. They only say where typing lands. What the session is doing has one row of its own, the header, directly over the upper rule: the turn's spinner, word, phase, and elapsed time at the draft's column, and the goal at the right edge. The question "is it still working?" and the question "what is it working toward?" are answered on one row next to the input, and the rules never change:

```
  so the loader has to resolve the home before it reads the profile, and
  the session store opens its journal after that

⠠⠞⠁ Kneading…  thinking · 12s            ● Goal active  round 3/256 · Ship it
────────────────────────────────────────────────────────────────────────────
> ▌Enter steers the next step                                esc interrupts
────────────────────────────────────────────────────────────────────────────
  Model: deepseek/chat  Context: ~15k/128k (12%)  ~/bake
```

The word is drawn from the locale's `activityWords` when the turn starts, seeded by the session and its transcript length, and kept until the turn ends. It does not follow the phase, so the label stays one label while the details beside it change. The phase comes from the newest live row: `thinking` while reasoning streams, `writing` while the answer streams, and `running <tool>` while a call streams or has committed without its result. `/stop` replaces the word with `Stopping` in red. Manual compaction takes the header the same way, as `Compacting…` and its phase. The word is drawn in the running orange; the indicator is a six-column, three-row dot wave packed into three Braille characters beside it, shifting one dot column right every two 150 ms beats in a continuous 1.8-second loop. Screen readers use a static ASCII `>` in place of the wave.

The spinner and the seconds are the only things on the header that move, and the rules never move at all: a moving run the width of the terminal, directly over the input, would distract from the draft while saying nothing the spinner does not. The row is redrawn only on beats where the glyph or the seconds change; without a clock, or with motion off, it holds still.

The glyph takes the rail's first column, the column where an action's marker and the goal block's head sit, so the turn reads as the head of the work above it, level with the tool calls it is running. The draft and the status line start two columns in, at the rail's width. The goal is right-aligned, apart from the turn, because the two answer different questions. `headerRoom` gives the turn its label first: the goal's details are cut from their end while a few cells of them still fit, then left out so `● Goal active` stands alone, and below that the goal is dropped whole rather than left as a fragment. With neither a turn nor a goal the header is a blank row: it keeps its place so the input never moves when either appears. The goal's states are `● Goal active` in orange with its round count while the goal may continue, `○ Goal on hold` or `○ Goal paused` in yellow while it waits on `/goal resume`, `✗ Goal blocked` in red with its reason, and `✓ Goal complete` in green, each followed by the objective. The header carries the goal only as a fallback, though. While a goal is set, `Goal` in `packages/ui/src/goal.tsx` holds it as a block among the panels above the header, drawn as the task list is, and the header's right side stays empty:

```
● Goal active  round 3/32
└ Port the release pipeline to signed bundles for every platform and verify each
  checksum before publishing anything
  /goal pause holds · /goal edit <objective> changes · /goal clear ends
```

The head is the phase's glyph and word in its colour, with the rounds while the goal is active or complete. The objective hangs from it, wrapped to at most two rows and cut with an ellipsis after that, since a paragraph belongs to `/goal`, not to the chrome. A blocked goal's reason takes a branch above the objective in red. Under the tree, a dim row names the `/goal` actions that apply in this phase. The block claims its rows (`goalRows`) after the thinking window and before the task list, because the goal is what the tasks work toward and a human set it. When rows run short it gives up the actions first, then the objective's second row, then the reason, keeping the head and the objective's first row. Only when it gets no row at all does the goal return to the header's right edge, cut as described above. The rules' glyphs are the style resolved from the terminal ([§4](#the-frame-is-chosen-from-the-terminal-not-assumed)), because a full-width run of East Asian Ambiguous glyphs is the one place such a character accumulates error across a row.

The rows above the header are the thinking window: at most `THINKING_ROWS` (3) of the newest wrapped rows of the streaming reasoning, a dim italic paragraph at the rail as reasoning is in the transcript, with no verb, so they are the same text the transcript will keep. One blank row (`THINKING_GAP`) separates the window from the header, so the turn's word, phase, and elapsed time read as their own row rather than the paragraph's last line; the blank belongs to the window, coming and going with it, and is the first row given up when only one is free. `thinkingRows` in `packages/ui/src/activity.ts` uses the shared Markdown formatter, wraps its text to the available terminal columns, and keeps the newest rows. Completing Markdown syntax may reflow the live preview within its row limit. The window disappears once the answer or a call starts. Reasoning arrives faster than anyone reads it. Drawn whole, it grew the live region to its limit and then scrolled every row of it with each token. A single ticker line held its height but froze on a long line, since its newest line was the one being written past the edge, and showed raw `**` and backticks. The window grows to three rows and then holds, so the reader sees the thought moving at a height that does not change once it is full. It claims its rows after the live region, so on a short terminal it gives way to the output. An open interaction hides it and the chrome, because the question is what the turn is waiting on.

When the turn ends, the header stays and says how the turn went:

```
✓ Completed  42s · edited 1 · ran 2 · read 3 · 1 failed
```

The glyph and label follow the recorded turn end: a green `✓ Completed`, a yellow `■` followed by why the turn stopped (`Interrupted`, `Blocked`, `Output token limit reached`), and a red `✗ Failed` for an error, whose message can run to paragraphs and stays in the transcript. A completed turn draws no line of its own in the transcript; `- Completed` there repeated this row. The elapsed time is the header's clock when the turn ended. The counts are the turn's committed calls grouped by past-tense verb, with edits first, followed by the number of calls that failed. They are read from the transcript rather than tallied as the turn ran, so they agree with the session log however results arrived. The running label and the summary are the same row, so the input never moves between them, and a turn that has not spoken yet costs no row at all. The summary holds until the next turn starts. A resumed session shows its newest ended turn the same way, without a time, because no clock watched it run; that turn is read from the transcript once, when the surface mounts, so later commits never read history again. Before any turn has ended, the turn's side of the header is empty.

### 2.3 Interaction — modal, and it wins

Approvals and prompts take priority over the live region. A user answering "may I run `rm -rf`?" does not need concurrent token streaming; they need the command, the cwd, and the choices, unambiguously. When an interaction is open the live region collapses to a single summary line.

### 2.4 Notice — transient, bounded, dismissible

Command results, errors, hints. Cleared by the next submit. Bounded per §5.

### 2.5 Status — one line, degrades by priority

Fields in display order; access and then thinking reserve their width before the model, then supporting fields drop right-to-left as width shrinks:

```
Model: <name>  plan  Access workspace-write  Think high  context  in  out  cache hit  cwd
```

There is no state word. The header over the composer says what the session is doing, in colour and in words, and the composer's placeholder and hint say whether Enter starts a turn, steers one, or is refused; a third copy under the composer repeated them on every frame. The model leads because it is what the row exists to say, followed by plan mode while it is on or pending. The input, output, and cache-hit totals are the session's billed tokens, summed over every request. They appear once a provider has reported usage; cache hit is the share of billed input read from the provider's cache, rounded down, and appears only when the provider reports cache traffic, so a provider without a cache shows no false 0%. The model and context fields, labels and values alike, use the terminal's normal foreground. Plan mode, input/output totals, the cache-hit label, and cwd stay dim. The cache-hit percentage carries a semantic colour: it is green from 70%, yellow from 30%, and red below (`cacheTone` in `palette.ts`), beside a dim label, so the number alone still says it under `NO_COLOR`. The context and token fields are bounded and never cut mid-number, since `cache hi` or a clipped meter is a different number. When the full context reading does not fit, a complete `ctx ~N%` reading can take width from the model name while keeping its label and the access and thinking badges; at smaller widths it yields whole. The cwd is the one unbounded field, and it truncates from its start to keep the workspace's name.

The access badge uses the session’s permission projection, retaining the full built-in mode name at 40 columns even when the model must truncate. Its label stays dim, while its value is blue for `read-only`, green for `workspace-write`, red for `danger-full-access`, and yellow for custom or automatic policies. Profiles without the projection omit it. The ocean blue `Think` badge shows an explicit model effort or the adapter's advertised default. A provider default without an advertised level is named as such; a model without reasoning metadata has no badge. At 40 columns, access and a short level such as `high` fit together; longer defaults yield as a whole so access remains visible and the row never wraps.

Left-packed, two spaces apart, stopping where the fields stop — not justified to both edges, per [§6a](#chrome-separates-by-framing-the-input-not-by-aligning-the-status-line). It sits directly under the base rule, with no blank row between them, and starts at the draft's column, as the header's label does. Never wraps to two lines; a wrapped status line silently costs a row of live region and can tip L1.

### 2.6 Composer — a cursor-following window

The composer displays one to five physical rows under the rule, with the prompt marker at the left edge and the draft at the rail's column. There is no box and no fill: the draft is in the terminal's own foreground, so it reads on any theme, and placeholder and hint text are dim. It wraps the draft at its actual available width, including the marker rail and contextual hint, before choosing the visible rows. Moving Home, End, or through the middle of a wrapped paragraph keeps the drawn caret visible.

`wrapDraft` wraps the way an editor does. Rows break after whitespace, and the whitespace at a break hangs past the row instead of opening the next one, so no wrapped row starts with a space the user did not type. Wide characters are break opportunities of their own. A word longer than a row starts a new row and is split where each row ends. Tabs are drawn as spaces to the next four-column stop, because `string-width` measures a tab as zero columns while the terminal moves to its own stop, which leaves cells where the layout did not put them and stale text under them. The layout ignores the caret and is one column narrower than the row, and the caret is drawn into that column, so moving through the text never moves a word. Beside a hint, that column is the first of the two that separate the draft from it, so the hint costs no more than it did. The hint appears beside the caret, and `^` marks text above the window. Wide characters use terminal cell widths. IME candidate placement at the caret is not yet implemented.

`chromeFor` accounts for the gap, the header, the rule, the first draft row, the base rule, and the status line. On a short terminal the gap yields first, then the base rule, then the rule, then the status line, then the header, which says whether a turn is running; the input row remains visible. At 40×4, the three available rows hold the header, the input, and the status line. Every piece is one row at any width, so a width change never moves the input vertically. Additional draft rows are reserved before panels claim the remaining space.

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
- **Gutter: 2 columns**, one glyph plus one space, giving every row kind a constant left rail so every row starts in the same column.
- Text measurement is Ink's (`string-width`), which handles CJK wide cells and emoji correctly. Never use `.length` for layout arithmetic.

### The frame is chosen from the terminal, not assumed

The composer's rule is a full-width run of box-drawing characters (`─`), and the welcome card is framed in them (`╭─╮`). Two kinds of terminal cannot draw them:

- One that is **not encoding UTF-8** writes the bytes through as mojibake, so the frame becomes punctuation on every row.
- One configured to draw **East Asian Ambiguous** characters two cells wide draws a full-width horizontal run at twice the width Ink measured. The frame wraps, and Ink's own row arithmetic is wrong from then on — this is the damaging case. Every other Ambiguous character on this surface (the turn marker, the selection pointer, the caret) sits alone in a fixed-width rail, where a terminal that draws it wide shifts one row by one column; a border run accumulates that error across the whole line.

`resolveFrame` in `packages/app/src/frame.ts` decides once, before the first frame, and `@dsh-tui/ui` takes the answer as a prop — the presentation layer reads no environment. Encoding and `TERM` come from `LC_ALL`/`LC_CTYPE`/`LANG` in POSIX order; ambiguous width is a terminal *preference* and cannot be detected, so a CJK character locale (or `--locale zh`) stands in for it. `composerFrame` in the profile overrules the lot, which is the only answer for a terminal the environment describes wrongly.

The ASCII fallback is laid out against the same widths and spends the same rows.

### The composer yields structure before it yields content

The hint costs whatever its locale needs. It is structure around the one field on the surface the user is actually composing in, so it gives way before the draft does, at a named width rather than by shrinking; the header's goal is cut before its turn is (§2.2a):

| Below | What goes | Why not shrink it |
| --- | --- | --- |
| `HINT_MIN_COLUMNS` (60) | the composer's right slot | a hint truncated to fit has stopped being help, and the key it names still works unnamed |

Above that width the hint never shrinks, so the draft takes whatever is left and wraps around it. The hint is drawn outside the clipped stack and aligned to its bottom, so it sits beside the caret's row wherever wrapping puts it — inside, it lands beside the first row of a wrapped line and notches the paragraph's top right while the caret row runs to the full width.

### ASCII only, and actions are named rather than pictured

Symbol glyphs are a measurement risk before they are a style question. `string-width` reports `U+2699` as one cell and `U+2699 U+FE0F` as two, and a terminal with an emoji font may draw the bare codepoint double-width anyway. Ink measures with that same library, so when the terminal disagrees, every column after the glyph shifts and **nothing in the layout engine can detect it** — `renderToString` measures it the same wrong way. Characters below `0x80` cannot disagree.

So the render vocabulary is a **verb column**: a named row, its text, and any output aligned beneath it. An action is the exception, named by its tool as a call, `Bash(rg -n …)`, with its output hung from it on a `⎿` in the verb column ([a call and its result are one zone](#a-call-and-its-result-are-one-zone)).

```
> Find where the session controller registers commands
  The registry is the list, so discovery should read it.
● Bash(rg -n "commands.register" -g '*.ts')
  ⎿      packages/app/src/controller.ts:45
         packages/app/src/controller.ts:52
● Read(packages/app/src/controller.ts)
  Two registrations, both through ctx.effect.
> Ask anything, / for commands
ready   deepseek/chat                       ctx 12%   turn 3   0f3a9c
```

`run`, `read`, `plan`, `ask`, `error` read at a glance, survive every font and locale, and stay legible pasted into a bug report. Approvals join the same grammar instead of inventing their own marks, which is why `ask` is a verb rather than a symbol. Reasoning takes no verb: it is the model's prose rather than an action, so it is a paragraph at the rail like the answer, dim and italic, and the answer's `<` marks where the reply begins. A label on every block would make the chat a log of actions, and the output column would wrap the working-out narrower than the answer beside it.

| Row kind | Marker | Columns | Color |
| --- | --- | --- | --- |
| user | `>` | text at 2 | default, bold |
| assistant | none | text at 2 | default |
| action | `●`, then `Tool(argument)` | head at 2 | marker orange and blinking on and off while running, green when done, red on failure; tool name bold |
| action output | `⎿` on its first line, at 2 | aligned at 9 | `output` grey, red on failure; green/red for diffs; `+N more lines` and `⎿` dim |
| reasoning | none | text at 2, wrapping at terminal width | dim, italic |
| interaction | `ask` | as a verb row | yellow |
| error | `error` or a red verb | as a verb row | red |
| list selection | `*` | marker at 0 | ocean blue |
| composer | `>` | text at 2 | ocean blue, bold |

`*` marks a selection and a current value; `>` is the composer prompt and a user's own words. They are never swapped: two identical markers a row apart look like one list.

`prototype/ascii.mjs` renders the vocabulary and fails if any rendered character is above `0x80`.

## 5. Bounding unbounded content

Any dynamic region rendering a list of harness-owned length needs the same treatment. The rule:

> Render at most `budget` lines. When more exist, render `budget - 1` and a final `+N more` line. When the full list matters, commit it to the **transcript** — static, scrollable, free — instead of holding it in the dynamic region.

This is the principled fix for `/help` and `/model`: a command's full output belongs in scrollback, where the terminal can scroll it, not in a dynamic overlay that is capped by L1. The notice region is for *short*, transient feedback — "loading models…", a sign-in URL — not for catalogs, and not for a command's outcome, which commits under the command row.

"Free" is about the buffer, not about the reader. A row costs the transcript nothing to keep and costs the reader a scroll to get past, so the escape hatch holds for content a user asked to see — they typed `/help` — and fails for content that arrives whether or not they wanted it. Tool output is the second kind, and [§7b](#a-results-output-is-previewed-not-replayed) bounds it.

## 6. Color, motion, accessibility

- **Color is semantic only, from one palette**: `palette.ts` holds five saturated semantic tones and the two neutral tones of a result's preview zone, and every component names a role rather than a hue, so a marker, its verb, and the header agree. Orange is `running`: the header's word and spinner, a running action's marker, and manual compaction. Green is `done`: a finished action, a completed turn's summary, and an added line. Red is `failed`: a failure, an error, a removed line, and the word of a turn being stopped. Cyan is `asking`: the composer prompt, a selection, staged attachments, and an action in progress on the task list. Yellow is `waiting`: a question, an approval, a picker, queued input, a notice, and a turn that stopped rather than failed. Dim marks reasoning and secondary interface metadata such as line counts and a call's description, and tool arguments use normal brightness. A result's preview has no background: its plain text is in the `output` grey, which is quieter than an answer and brighter than dim, so output is supporting material without receding to the level of the working-out. The surface draws no background anywhere; failures, diffs, and syntax colour keep their own tones in the preview. Never decorative, and never the only carrier of a state: every tone is paired with a word, a glyph, or motion, so a 16-color or `NO_COLOR` terminal loses no meaning. Honor `NO_COLOR` and non-TTY.
- **Code in a diff is the one exception**: syntax colour says what kind of token a run is, not what state anything is in, and it comes from a highlighting theme rather than the palette. It is laid over the side's tone and never replaces it: the sign, the line number, punctuation, and plain words keep green or red, so a line still shows as added or removed however much of it is highlighted. [An edit shows what changed](#an-edit-shows-what-changed) has the rest.
- **Weight carries structure**: an action's marker and tool name are bold, and so is `error`, so a scan down the left of the transcript lands on each action rather than on its arguments.
- **Spinners only on a TTY** with color enabled. Under `useIsScreenReaderEnabled`, replace motion with discrete state transitions — a screen reader announcing a spinner frame-by-frame is unusable. The UI package reads no clock. `App` times and animates the header only when the terminal owner passes a `clock` prop, and animates it only while `motion` is not false. The runner always passes the clock and turns motion off under `NO_COLOR`, so the header's wave rests centered, a running action's `●` does not blink, and only the elapsed seconds advance, once a second. Without a clock, or under a screen reader, the header also leaves out the elapsed time and changes only when the phase does.
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

### Prose and tool output follow terminal width

Assistant replies and reasoning wrap at the current terminal width less their two-column rail. Indented tool output wraps after the verb column; the label joins the text when the terminal is too narrow to hold both columns. Neither has a fixed maximum width. Width changes remeasure live rows and the thinking preview; committed scrollback is replayed when the terminal needs to reanchor after resize.

### A line wraps, never truncates

A line the user cannot finish reading is worse than a ragged one. Long output lines wrap; they are never cut with an ellipsis. Horizontal truncation is reserved for surfaces where the full value is one keystroke away — a completion row or a picker — never for a line of a result that already cost a tool call.

How many lines of a result are drawn at all is a separate question, answered by [§7b](#a-results-output-is-previewed-not-replayed). Every line that is drawn is drawn whole.

A wrapped row never opens with a space. A word that ends exactly at the width leaves the space after it for the next row, where it would indent that row by one column. `softBreaks` in `packages/ui/src/present.ts` turns each such space into the line break it stands for before the line is drawn. It keeps the text's length so styled spans still line up, and leaves the text as it was when wrapping rewrites anything else, such as a tab.

### A call and its result are one zone

A call and its result draw as one block, opened by a `●` at the rail and headed as the call itself: the tool's name, its words capitalized and run together (`toolLabel`: `bash` is `Bash`, `read_file` is `ReadFile`), and its argument in parentheses, `Bash(cargo check --workspace 2>&1 | head -20)`. The argument is the tool presenter's title; a tool that declares no card is headed by the argument field it acts on (`command`, `file_path`, `path`, `pattern`, `url`, `query`), or its fields as `key: value`, never by raw JSON. While the call runs, the marker blinks: shown in orange, then hidden, a clean on and off with nothing at half brightness, and a space in its cell so the head never moves. When the result arrives the marker holds, green, or red on failure. A title's first word is dropped when it only names the tool's action again, as `Grep` does in `Grep(Grep TODO)`; a command keeps every word. No second row announces the outcome, and no call id is drawn; the id identified a result's row with its call, and there is no longer a separate row to identify. Output indents to the output column beneath the head, and its first line carries a dim `⎿` in the verb column that hangs it from the head, so no whitespace divides a call from what it produced. A changed line's number keeps the verb column instead, since the number is what reads against the code.

`Actions` in `packages/ui/src/actions.ts` does the merge. It holds each call until its step ends (`step/end`, or the model's next message or the turn end in a log without one) and releases the step's calls together, in call order, so a fast second call cannot print above a slow first one. The live region draws the held calls in the shape they will print in; nothing is committed and later drawn again. The shape holds from the first streamed call: the calls the model is still streaming, the calls its committed message announced but the loop has not dispatched yet, and the calls running or finished are one block, so two streamed calls are already `run 2` with both branches, and the count never drops while the loop reaches each call in turn. A block that changed shape on the way would give up rows the frame holds blank until history next prints ([§8.1](#the-frame-never-rises)), and since reasoning prints nothing while it streams, those rows would stand as a gap between the step's block and the thinking window under it.

A step that made two or more calls prints them as one block, because they were one decision the model made:

```
● ran 2 · edited 1 · 1 failed
├ Bash(bun test tests/parser.test.ts)  exit 1
│ ⎿      bun test v1.3.0
│        +4 more lines
│         1 fail
├ Edit(src/parser.ts)  +1 −1
│     14 - const parts = line.split(",")
│     14 + const parts = splitQuoted(line, ",")
└ Bash(cat missing.txt)
  ⎿      cat: missing.txt: No such file or directory
```

The head counts the calls by verb in the order they first appear, present tense while any runs and past tense after, then how many failed, in red. Its `●` is the step's state: blinking while a call runs, then green, or red when one failed. Each call hangs from it on a branch in the rail, `├` and `└` for the last. The branch takes the place of the call's `●` and its colour, green or red, but holds still while its call runs: a blinking branch would open a gap in the tree, and the head already blinks for the step. A dim `│` carries a call's lines down to the next call. Nothing moves out of its column, since the tree is drawn in the rail. No blank row separates the calls; the stem is what joins them. A step with one call draws it on its own.

A running step taller than the live region fits itself to the window rather than being cut from the top (`fittedGroup` in `packages/ui/src/present.ts`). Cut like prose, the block lost its head first, which is the line that says what the step is doing and the marker that says it is still running. Instead the block gives up detail oldest first: each finished call folds to its head line, the oldest first, so the output just read stays in view longest; once every finished call is one line, the oldest calls fold into a single dim `+N earlier calls` branch until the rest fit. The head, the newest calls, and every running call stay. On a window too short even for that, the block keeps what says the most per row, in order: the head, the newest call's head line, and the count of the rest, giving up its opening blank first. The window is measured in wrapped rows at the current width, so the same rules hold for a narrow split pane and a wide monitor, and they are re-applied on every resize. Nothing is lost: the step prints to history whole once it ends.

Tool arguments use normal brightness, and a result's preview draws its plain text in the `output` grey, with no background ([§6](#6-color-motion-accessibility)). The marker and tool name are bold; a call's description and a `+N more lines` count are dim. Failures retain red emphasis throughout, and diffs retain red/green emphasis, under their syntax colour. Among reasoning, actions, and answers, only reasoning is dimmed, and its text is italic as well, in the transcript and the thinking window alike. Dim alone put the working-out in the same weight as a call's description, one column away from it.

A call's own lines sit under its head: the rest of a multi-line command, then the card's description and the input a card chose to show. They are bounded as output is, first and last lines around a count, and at least one line of each always shows, so a script the model wrote prints a few rows and a description survives `resultLines: 0`. An input the title already names, such as a search's pattern, is left out. A structured input is drawn a field or an item to a line, and an item of plain values is shown as its values, as a task list's are. A card's fenced block is drawn as its code, since two lines of backticks around a failed command's error are the one piece of markdown this surface would otherwise print.

### Indentation separates columns; a blank row separates sections

Within a turn the levels are: rail glyph at column 0, prose and reasoning at 2, tool output at 9. Indentation carries every separation it can, and it carries most of them — an answer at the rail is never confused with output under a verb.

Two things it cannot carry. Two actions in a row share the verb column, so the second, drawn directly under the previous call's output, looks like more of that output. And reasoning shares the rail with the answer that follows it: the answer stops being dim and italic, which under `NO_COLOR` is the only difference left — the working-out and the conclusion would look like one paragraph. The answer carries no marker of its own; its full-brightness text at the rail, and the blank row that opens it, are what say the reply has begun. A final answer ends with one dim row of its own, `856 tokens · 42.3 tok/s`, directly under it and continuing its section: the provider's output tokens over the logged time from the first streamed token to the finish, so the rate is the generation speed and not the wait for the model to start. A step that ends in tool calls, an interrupted answer, and a log with no usage or no measurable time draw no rate rather than a guessed one.

So **a blank row opens each section**: a reasoning block, a tool call or a step's group of them, a slash command, and the answer. Nothing else gets one. A result continues the call above it, and a command's outcome continues the command, so neither ever floats away from what produced it. A section with no content produces no blank either, since a blank belongs to the lines under it; an empty block is the everyday case while a turn is still streaming, and on its own it would be a reserved row spent on nothing.

That is one blank per section rather than one per row. Separating every row would stripe the transcript, and striping is noise once a session is long — exactly when the transcript matters most. In scrollback the cost falls on an unbounded buffer; in the live region it is one row of a window capped at `LIVE_BUDGET` whatever it contains.

```
● Use the bash tool to run exactly: echo TERMINAL_OK

  The user wants me to run a command and then reply.

● Bash(echo TERMINAL_OK)
  ⎿      TERMINAL_OK

  The command ran. I should reply with just "DONE".

  DONE
```

## 6a. Separating the chat area, the status line, and the composer

The three regions must be three kinds of thing at a glance. A terminal offers few ways to say that, and they do not cost the same. `prototype/separation.mjs` renders the candidates with their row cost.

### Spacing is cheap above and expensive below

A blank line inside the transcript is charged to scrollback, which is unbounded. A blank line in the dynamic region is charged to the L1 budget the live region needs — on a 10-row window that is a tenth of the screen. The same pixel of whitespace has two very different prices depending on which side of the boundary it falls.

So: **spend rows on structure inside the chat area, spend none on chrome.**

### Chrome separates by framing the input, not by aligning the status line

| Candidate | Chrome cost | Verdict |
| --- | --- | --- |
| status as one more sentence | 2 rows | looks like another chat row; its stray leading space looks like a mistake |
| full-width rule | 3 rows | draws attention to a line carrying no information |
| justified status bar | 2 rows | superseded: on a wide terminal the justification opens a gap the width of the screen between the model and the next field, and the row stops reading as one bar |
| boxed composer alone | 4 rows | the box says where typing lands, but its top edge butts against the last line of the answer |
| blank, status line, boxed composer | 5 rows | superseded: the status line stood between the answer and the input, so returning to the prompt crossed metadata every time |
| blank, boxed composer, status line | 5 rows | superseded: the box spent two rows on structure alone, and the turn header above it spent a third saying what the session was doing |
| blank, solid padded surface, status line | 5 rows | superseded: a filled block looks like another tool's input bar, and says nothing about the session |
| blank, rule carrying the turn, composer, padding, status line | 5 rows | superseded: the padding row carried no information, and the goal had nowhere to show |
| blank, rule carrying the turn, composer, rule carrying the goal, status line | 5 rows | superseded: two labelled rules look like two headers, one of them under the input |
| **blank, header carrying the turn and the goal, rule, composer, rule, status line** | **6 rows** | **adopted** |

A bare full-width rule on its own draws attention to a line carrying no information. The adopted chrome gives the information its own row, the header, and lets two bare rules do the one thing a rule is good at: frame the draft, so the draft is one band between them ([§2.2a](#22a-the-header--one-steady-row-for-the-whole-turn)):

```
  DONE

✓ Completed  42s · ran 2                    ● Goal active  round 3/256 · Ship it
──────────────────────────────────────────────────────────────────────────────
> ▌Ask anything, / for commands
──────────────────────────────────────────────────────────────────────────────
  Model: deepseek/chat  Context: ~15k/128k (12%)  in 42k  out 3.1k  cache hit 81%  ~/bake
```

The rules are the separator, so the status line does not also have to be one. They say where typing lands without a box or a word of copy, and under `NO_COLOR` they are still lines. The draft and the status line start at one column, so the input and its footer form one block without an enclosing shape; the header's glyph stands a rail to the left of them, with the markers of the work it heads.

The input is the row the user returns to after reading an answer, and the header directly over its rule is the row checked on the way: whether the agent is still working, how the last turn went, and the goal it is working toward, all on one row. The base rule under the draft keeps the status line clear of it, so the status line sits directly under it as a footer, with no blank row between them, and nothing below it can be mistaken for output.

The blank row above does the one thing the header cannot: without it the header sits directly under the last line of the answer and looks like part of that answer. Anything that belongs to the input rather than to the conversation — completion, a notice, queued input, quit feedback — is drawn between the blank and the header, so the blank opens the whole stack instead of splitting it. Before any turn has ended and without a goal, the header is blank too, and the two blank rows are the chrome's height holding still.

Six rows is the largest chrome cost this document accepts, and `CHROME_ROWS` in `packages/ui/src/layout.ts` charges all six against every other region's budget. The rows the blank, the header, and the two rules add come off the live region on a window shorter than 17 rows and off nothing at all above that, because the live region is capped at `LIVE_BUDGET` first.

### The composer is the same family, one weight heavier

A user message opens with `›`; the composer opens with `❯`. Same chevron, more weight: *what you said* against *where you speak*. Related roles should look related; only their weight should say which is active.

### Quit feedback stays beside the input

A slash command prints as it was typed, flush with the left edge, and its outcome hangs from it on a `└` branch, as a step's calls hang from their head:

```
< Done, the parser handles quoted commas now.

/model deepseek/deepseek-v4-pro high
└ Model set for the next turn: deepseek/deepseek-v4-pro (high)

/foo
└ Unknown command: /foo
```

The slash takes the rail's first column, so the row breaks the left edge the way a command breaks the conversation and reads without colour; the name is bold, as an action's verb is, and the arguments are the user's own words at full weight. `command/done` projects its text as a notice placed `command`, which draws the branch in place of a `note` verb, red with its text when the command failed; later lines of a long outcome, such as `/help`, sit under the first at the text column, and the outcome wraps at the full width, as tool output does, since it is as often a table as a sentence. A notice no command produced keeps its verb.

`Ctrl-C` arms quitting and displays one row of feedback directly above the composer, for `doubleInterruptMs` (two seconds by default) or until any other key dismisses it. The runner owns the timer and armed state independently of command notices, so neither replaces the other.

### The right slot is contextual, never decorative

The composer's right edge can hold `↵ send`, `esc interrupt` while a turn runs, or `↑↓ select` while an overlay is open. It must stay contextual. A permanent hint teaches nothing after the first day and becomes noise on a surface the user looks at hundreds of times a day — the same reason nothing here animates on keystroke.

### Turn boundaries, not message boundaries

One blank line opens each user turn inside the transcript. Separating every message would stripe the screen. Separating turns marks each turn when scrolling back through a long session, which is the actual task.

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

### Pickers scroll, and hold still while they do

A picker does offer scrolling, so its edges say `↑ N more` and `↓ N more`, each spending one of the picker's rows; at two rows there is no room, and the `i/n` position on the key line carries it. The window moves only when the selection would leave it, never recentres, so walking inside it moves the pointer and nothing else. Columns — label, status, description — are sized over every choice rather than the visible ones for the same reason. An action that must stay reachable, such as a new session under a long history, is `pinned` below the scrolled list instead of at its end. Every picker, and every approval, question, and sign-in panel, reads in one order: a bold title, the input after `>`, the choices, and dim keys last. `packages/ui/src/choices.ts` holds the filter and scroll rules; `choices.test.ts` checks that the selection is always shown and the rows never exceed the limit.

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

Reasoning streams in the thinking window over the header while it happens. When the step commits, the transcript keeps a preview, the way it keeps a result's: the first `resultLines` wrapped rows, a dim italic paragraph at the rail, then a dim `+N more lines`.

```
  The user wants the loader to stop reading `DSH_HOME` twice. Let me think
  about where the profile is resolved first, because the session store opens
  its journal after that.
  +9 more lines
```

It is worth watching live and rarely worth re-reading; the full text stays in the session log, which is the durable record under **Model-visible ⇔ logged**. Replaying it whole into scrollback buried the answer under the working-out. The count is of wrapped rows, the rows the reader would have scrolled past, not of source lines. Blank rows at the end of the preview are dropped rather than shown before the count. Reasoning that would hide a single row is drawn whole, since the count costs the row it saves, and at `resultLines: 0` the preview is its size alone, `12 lines`.

`present` in `packages/ui/src/present.ts` takes the wrap from `wrappedRows` in `line.tsx`, so the count and the rows the transcript draws come from one measurement.

### A result's output is previewed, not replayed

A tool result once committed every line it returned, so one `ls -la` wrote seventy rows of scrollback and one glob wrote a hundred paths. Measured through Ink's real render loop, that single commit is 5.6 kB across five frames, and it scrolls the terminal by the length of the output every time a tool runs.

The next design held the head of the output in the live region until the model answered, and committed only the outcome. That fixed the length and broke the motion. The body appeared under the call, then vanished when the answer began, and on a full screen the composer rose by its height and fell back as the answer streamed.

So the transcript commits the outcome and a preview together, once:

```
● Bash(bun test)  exit 1
  ⎿      bun test v1.3.0
         tests/parser.test.ts:
         +38 more lines
          11 pass
          1 fail
● Read(packages/ui/src/app.tsx)  412 lines
● Grep(splitFields in src)  3 matches
```

Command output, source reads, search and web results, edit diffs, and failures are previewed. Output shows its first lines and its last, `resultLines` in all, with a dim `+N more lines` between them, because what a command ends with, a test summary or the error it stopped on, is as often the news as what it opened with. Blank lines at either end of the output, or beside the count, are left out. A diff shows its first changed lines, counting only those, as a patch reads from the top. A count that would stand for one line is replaced by that line. At `resultLines: 0`, these results collapse to their headline and size. Source reads and search matches carry syntax colour without including line-number prefixes in the grammar; gaps between matches reset grammar state. Valid JSON and JSONL output use the JSON grammar, while plain logs colour diagnostic labels, file paths, and URLs. Failed output stays red. Colour does not change the displayed text or row count.

A card's own summary rides on the head too, where no bound hides it: a search's count (`3 matches`), a partial read's window (`1–40/900 lines`), a fetch's status, and a command's non-zero `exit` or `signal`, which is red. It replaces the line count, which says less. A fetch's address is drawn only when a redirect made it differ from the one the call named. A count of one takes the locale's singular: `1 line`, `1 match`. A headline that only repeats the call's title is dropped. Nothing on screen is held and later removed, so a result never moves the composer after it prints, and a resumed session draws exactly what the live one did.

The card's headline is held apart from the card's lines for exactly this reason. `70 lines` does not say which file was edited, and `Edit packages/ui/src/app.tsx` is the one line of an applied diff that still means something after the hunks have scrolled away.

The full text is in the session log, which is the durable record under **Model-visible ⇔ logged** — the same guarantee that lets reasoning collapse. Nothing is lost; it is one `dsh` session read away rather than one scroll away, and the answer stays on screen.

`resultLines` is a validated `Config` field, defaulting to 4, because how much of a result belongs in scrollback is deployment-varying in the same way reasoning is: a debugging session wants more, and a deployment reading its transcripts out of a log rather than off the screen wants `0`.

### An edit shows what changed

An edit's card once drew each hunk as its tool sent it: the path, the three context lines above the change, the change, and the context below. Under a four-line preview, the context filled the preview and the change was counted in `+14 more lines`, so an edit showed every line except the one it changed, and the path it repeated per hunk was already on the head. Now it reads:

```
● edited packages/app/src/startup.ts  +2 −2
      13 -   const home = process.env.DSH_HOME ?? defaultHome()
      13 +   const home = resolvedHome ?? process.env.DSH_HOME ?? defaultHome()
         ⋯
      31 -   return undefined
      31 +   return null
```

- **The head says how big the change was.** `+N −M` rides on it in each side's tone, including at `resultLines: 0`, where it is all an edit draws.
- **Only changed lines are drawn.** `diffLines` in `packages/ui/src/cards.ts` diffs each hunk's sides line by line, so two changes a patch joined into one hunk stay two, and `⋯` stands between changes for the unchanged lines they skip. A path heads a file's lines only when an edit touched several files.
- **Each line is numbered on its own side**, in the verb column, from the `oldStart` and `newStart` a producer records on its `FileDiff`. A producer that does not record them, and a session written before they existed, draws the same lines without numbers.
- **The preview counts changed lines.** `resultLines` bounds the lines that are part of the change; a `⋯` or a path rides along and never ends a preview.
- **The words an edit changed are reversed** in the side's tone. A removed line and the added line in its place are compared word by word. A pair that shares less than half of the shorter line was replaced rather than edited, and is not marked.
- **Code is highlighted** by the file's language. `packages/app/src/syntax.ts` wraps Shiki with the JavaScript regex engine and Solarized Dark, whose accents are the same in its light and dark variants, so they read on either background. Its base tones are left to the side's tone. The languages an agent edits most are loaded before the first frame, which costs about 70 ms beside a session's own startup, and any other loads its grammar the first time one is drawn. Presentation is synchronous, so until a grammar is ready its lines draw in the side's tone, and a committed row prints once, so an edit drawn before that stays uncoloured in scrollback.

### Reasoning is distinguished by geometry, not color

Reasoning sits at **column 4**, the answer at **column 2**. Dim alone merges the two under `NO_COLOR` and for a screen reader; indentation survives both. This is why §6's "color is semantic only" is not sufficient on its own — semantic color still needs a non-color carrier.

### The live region shows the call, not only the words

A turn that reasons, calls a tool, and then answers spends most of its wall time inside the call. If the live region draws only text and reasoning, that stretch is blank: the surface looks idle at the moment the agent is busiest, and the action first appears already finished, committed to scrollback.

So the live region projects tool-call blocks alongside text and reasoning, in stream order, and `LiveBlocks` in `packages/app/src/live.ts` is what folds the chunks into them. It is deliberately not `BlockAssembler.interruptedBlocks()`, which answers a different question — what an interrupted attempt may safely finalize into a durable message — and drops every call because interruption precedes dispatch. Display writes nothing and carries no such obligation.

`end` still drops the whole live copy, per the table above: it is published once the assistant message has committed, so the transcript already holds the rows these stood in for.

### Tool arguments: never render partial JSON

`tool-call-delta` carries `argumentsDelta`, so arguments arrive character by character. Rendering them live shows the user `{"comm` and then `{"command": "rg -n \"comm`, which looks like a malfunction. A streaming call keeps its name and puts an ellipsis where the argument goes — and keeps the ellipsis after `block-end` too, because the complete arguments are raw JSON and the committed row presents them properly a moment later:

```
● Bash(...)                      while the call streams

● Bash(echo TERMINAL_OK)         committed, through the tool's presenter
  ⎿      TERMINAL_OK
```

### Each tool presents as its own summary

A call is shown as the thing it does: `bash` as its command, a search as its pattern, a file tool as its path. The fallback is an argument count, never a JSON dump. Presenters stay pure and live with the tool, matching the repository rule that every tool's UI presentation is designed up front.

## 8. Robust rendering: no flicker, no jumping

Two distinct failures, two distinct causes. Neither is fixed by drawing faster.

### 8.1 The input rests on the bottom row

The composer rests on the terminal's bottom rows from the first frame. `frameOutput` in `packages/app/src/output.ts` moves the cursor to the bottom row before Ink draws, so the first frame grows upward from there and whatever the shell printed scrolls up above the session line. Committed history prints through `Static` above the frame, the live region follows it, and the chrome follows the live region with one blank row between. A fresh session keeps its session line directly above the input on the bottom rows. When optional `AppProps.version` is supplied and the session surface mounts with an empty committed transcript outside child inspection, a `BAKE v<root version>` welcome card prints once through `Static` in place of that heading, carrying the same session line inside it, so the block costs no row beyond the one the heading takes; resumed history has no banner and prints the heading alone. The card is at most 64 columns wide, draws the line style resolved from the terminal, and drops its border below `FRAME_MIN_COLUMNS` (40). It belongs to terminal scrollback, not the dynamic region, and each line that prints scrolls history up by its rows rather than moving the input down.

An input that followed the newest line down the screen, as Claude Code places its prompt, moved on every printed line until history filled the screen, and moved again whenever the frame shrank. A tall terminal spent most of a session in that phase.

> **Layout invariant L2.** The composer rests on the terminal's bottom rows, with the status line and Ink's cursor row beneath it, at every size and in every state. Above it are the rule, the header, the thinking window, and input-owned panels (completion, notices, queued input, quit feedback), a blank row, and the newest transcript or live line. The blank is one row unless the frame is holding rows something above the input gave up; those rows are blank too, until printed history takes them.

#### The frame never rises

A frame drawn on the bottom row stays there while it and the history printed above it in the same render fill at least the rows the previous frame did. Most shrinking is made up that way: an answer's line leaves the live region as it prints, a step's calls leave it as they commit when the step ends, and reasoning leaves the thinking window as its preview prints. What is not made up — a completion menu or picker closing, a notice clearing, a task finishing, a queued message leaving the panel — would lift the composer by the rows it gave up and drop it back as the next lines printed.

So `useHeldHeight` in `packages/ui/src/app.tsx` gives the frame a minimum height: its previous height less the rows this render prints. The printed rows are measured the way `RowView` draws them, before the render that prints them, because a floor corrected afterwards would already have moved the composer once. The rows the content does not fill are a blank spacer under the live output and over the controls. What streams stays against the history it continues, the thinking window and panels stay against the input, and the next rows to print take the spacer's rows instead of scrolling the screen. The spacer never reaches scrollback, because printed history replaces it in place. The held height is capped by the frame's own budget, so L1 holds.

A new session's blank rows are above its heading rather than in the frame. They reach scrollback once, between the shell's output and the session, which is the one cost of starting at the bottom.

The live region has no reserved height. It grows a row per streamed row up to `budget.live`, then keeps its newest rows. `tailLines` chooses them in wrapped rows, measured the way Ink wraps `Text`, so the window the lines are chosen for is the one they are drawn in.

#### A streaming answer prints as it completes

The answer does not wait for its commit to reach the transcript. `Printed` in `packages/app/src/printed.ts` prints settled Markdown blocks of the streaming answer to `Static`. A paragraph settles after a blank line; lists, tables, and fences wait for a successor block or the message commit. A text or reasoning block prints whole once a later block starts. Nothing past the first tool call prints, because the call commits through its own event. The live region keeps the unfinished block within its row budget. When the message commits, `reconcile` drops from it what already printed, so each line appears once. A block the commit changed prints again whole, since duplication stays readable.

This removes two failures of drawing the answer in the live region. An answer longer than the window was shown clipped until it committed. And every row the window gained or lost moved the composer on a full screen. Printed a line at a time, the text scrolls into the terminal's history the way the terminal would scroll it, while the frame holds one line and does not change height. `placement.spec.tsx` streams twelve wrapped paragraphs this way and asserts the input row never moves.

Printing is display, not record. The session log commits the message as before, and a resume draws it whole. Scrollback cannot be unwritten, so lines printed from an attempt that never commits stay; the transcript follows them with a notice that they were discarded.

#### What remains in the window

Only the newest section is cut; an older one is shown whole or not at all. A cut section keeps its opening blank, drawn outside the clip, and gets back the connector or reply marker the cut removed. Answer prose fills the window exactly, with its oldest line clipped from the top the way a terminal scrolls. Stopping at whole lines would leave the window a row or two short whenever the next line wraps. Tool output keeps whole lines. The window claims its rows from the shared budget only while it has rows to draw: a turn that has not produced output yet leaves those rows to the panels.

Growth scrolls history up by the rows it adds, and nothing the frame gives up moves the input. Existing history is neither traversed nor reprinted during typing or streaming, and the frame stays under `rows - 1` (L1) because every region claims from one budget.

#### Narrowing and growing taller repaint

Ink erases the previous frame by counting its lines. A terminal that reflows on resize has already re-wrapped each full-width row of that frame — the composer's rule — onto two rows, so the count falls short and the rows it misses stay on screen above the new frame. A terminal that grows taller adds its rows under the frame, unless it pulls history down from scrollback, and leaves the composer off the bottom row. Ink clears the terminal and replays history only when a frame overflows the viewport, so for the one render after the terminal narrows or grows taller `App` draws a spacer of viewport height above the controls. That frame is cleared and replayed; the next one, overflowing no longer, is cleared and replayed again at the normal size. A repaint therefore costs two replays of the transcript, which is what a resize cost when the frame filled the screen.

Each replay starts on the bottom row: `frameOutput` returns the cursor there after Ink's screen clear, so history shorter than the screen scrolls up to meet the frame rather than leaving it part way down. A repaint holds no rows, because it draws history and the frame together.

`packages/ui/tests/placement.spec.tsx` replays Ink's actual ANSI writes in a terminal emulator at 80×24, 40×10, and 120×40, placed as the runner places them. It checks that the input rests on the bottom rows under the newest line through repeated turns, commits, menus, feedback, wrapped Unicode, a full screen, and narrowing and widening resizes. Ink may clear and repaint during a resize; ordinary streamed turns do not trigger full-screen clears.

### 8.2 Flicker — a frame is observed half-drawn

Ink 7 wraps writes in DEC mode 2026 synchronized-update markers, so a supporting terminal presents each frame atomically:

```js
export const bsu = '\u001B[?2026h'
export const esu = '\u001B[?2026l'
export function shouldSynchronize(stream, interactive) {
  return 'isTTY' in stream && stream.isTTY && (interactive ?? !isInCi)
}
```

It also throttles with `leading: true, trailing: true`, so many state updates in one tick coalesce into one write. Both are automatic and neither needs configuration — but mode 2026 is terminal-dependent (kitty, WezTerm, iTerm2, Ghostty, Windows Terminal support it; Apple Terminal does not). On a terminal without it, the defense is to write less. The runner and the development harness render with Ink's `incrementalRendering`, so a frame rewrites only the lines that changed: a spinner tick rewrites the header's row, and a streamed token rewrites its own line, not every row of the controls.

Incremental rendering does not cover a frame that prints to `Static`. Ink erases the whole dynamic region, writes the new rows, and draws the region again, in separate writes. A streaming answer prints a row each time a line finishes ([§8.1](#a-streaming-answer-prints-as-it-completes)), so on a terminal without mode 2026 the rule, the composer, and the status line could be seen erased once per line: the input's frame blinked out and back as the answer streamed. `frameOutput` in `packages/app/src/output.ts` stands between Ink and the terminal. It holds every write Ink makes in one render and writes them as one. Within that write, the erase becomes a move to the region's top, each row is cleared as it is drawn over, and what is left of the old frame below the new one is cleared at the end. The screen it leaves is the one the erase would have left, and at no point is more than the row being drawn blank. Writes it does not recognize, including Ink's clear-and-replay on overflow, pass through unchanged, and stderr shares its queue so the two streams keep their order.

Ink's incremental renderer rewrites a changed row followed by a newline, but steps over an unchanged row with Cursor Next Line (`CSI E`), which stops at the terminal's bottom row instead of scrolling. The frame rests on the bottom row, so when it grows by a row and an unchanged row falls past the old bottom — a blank row, such as the gap that opens the chrome — that row is never added: the frame is drawn a row short, Ink's line count no longer matches the screen, and its next erase takes a row of history with it. `scrolling` in `frameOutput` writes each Cursor Next Line as a carriage return and newline, which is the same move everywhere above the bottom row and scrolls at it.

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

Taken together: the composer and status line rest on the bottom rows from the first frame to the last. History scrolls up as it prints, the live area grows under it, and rows a closing panel gives up stay blank above the controls until the next printed rows take them. Nothing on the input's rows moves except what the user types.

## 9. Verification

Layout claims are mechanically checkable and should be gated:

| Claim | How it is proven |
| --- | --- |
| L1 holds in every state | render each state at 80×24 and 40×10, assert dynamic height `<= rows - 1` |
| append cost is O(1) | `packages/ui/tests/scale.spec.tsx` (raw stdout capture, not `frames`) |
| no full-clear in a normal turn | assert `ansiEscapes.clearTerminal` never appears in captured stdout for a scripted turn |
| status never wraps | render at 40, 80, 200 columns, assert one line |
| the status line cuts no bounded field | `packages/ui/tests/line.spec.tsx` narrows a full row and asserts the cache-hit field goes whole while the path keeps its tail |
| billed tokens follow the provider | `packages/app/tests/context.spec.ts` reports no totals before a request, then input and output without a cache field, then a cache hit once the provider reports cache reads; `status.spec.tsx` draws them in English and Chinese; the PTY `fresh` scenario asserts the recorded turn's `in 5.9k  out 115  cache hit 48%` |
| motion shares one beat and draws only what changes | `packages/ui/tests/live.spec.tsx` runs the header and two running actions on one timer, draws at most once a beat and fewer than 70% of the frames their separate timers drew, draws a header without motion once a second, and keeps no timer for a header with nothing running; `live.spec.tsx` also blinks a running action's marker between the orange `●` and a space; `styles.spec.tsx` asserts the rule under the running header stays one dim run and the cache-hit tones |
| L2: the input rests on the bottom row | `packages/ui/tests/placement.spec.tsx` checks actual terminal rows across streaming, commits, idle transitions, menus, feedback, a full screen, and resizing; `live.spec.tsx` checks the frame grows a row per streamed row and no further than the live budget; `packages/app/tests/output.spec.tsx` runs a turn with its panels through `frameOutput` and asserts the input is on the bottom row from the first frame and after each resize replay; `output.test.ts` checks `anchor` byte for byte; the PTY `rendering` scenario asserts the built profile opens on and returns to the bottom rows |
| the frame never rises | `packages/ui/tests/live.spec.tsx` clears live output without printing and asserts the frame keeps its height, blank above the controls, until committed rows take it; `placement.spec.tsx` closes menus, notices, queued input, and quit feedback without the input row moving, then prints history into the held rows; `output.spec.tsx` clears a notice and shrinks a task list mid-turn |
| a running turn shows what it is doing | `packages/app/tests/live.spec.ts` streams reasoning, a call, and an answer through `LiveBlocks` and asserts all three appear, in order, with no arguments rendered |
| a windowed section stays named | `packages/ui/tests/present.test.ts` cuts a long block below its head and asserts `tailLines` restores the connector onto what is left, shows an older section whole or not at all, and counts wrapped rows; `live.spec.tsx` asserts a wrapped answer keeps its blank and holds its height once full |
| a printed line never erases the controls | `packages/app/tests/output.test.ts` checks the rewrite byte for byte and passes through writes it does not recognize; `output.spec.tsx` streams an answer on a full screen, replays every render a row at a time in a terminal emulator, and asserts that written directly at least three rows go blank part-way, that through `frameOutput` at most one does, and that both end with identical scrollback |
| a streaming answer prints once | `packages/app/tests/printed.spec.ts` prints settled blocks, keeps paragraph breaks across delta boundaries, compares streamed Markdown with replay, and reconciles the commit; `session.spec.ts` streams through the controller, resumes to one message, and marks an abandoned attempt's lines discarded; `placement.spec.tsx` holds the input row through twelve printed paragraphs on a full screen |
| the composer cannot outgrow its budget | `packages/ui/tests/line.spec.tsx` renders a 4,000-character paste at several widths and asserts the row count against `COMPOSER_BUDGET`, with the caret on the last row |
| a draft reflows only with its width | `packages/ui/tests/editor.test.ts` wraps drafts at every width from 4 to 29 and asserts no row exceeds it or opens with a hanging space, that no caret position moves a word, and that tabs expand; `line.spec.tsx` moves the caret over a wrapped draft beside a hint and asserts identical rows; `placement.spec.tsx` pastes a wrapped draft with a long path and a tab, resizes across 40 columns, and asserts every row stays between the rule and the base rule at the prompt column and no tab reaches the terminal |
| the chrome fits every supported width | `packages/ui/tests/line.spec.tsx` renders `Chrome` from 1 to 200 columns bare, running, and ended, and asserts no row exceeds the width, both rules are bare and exactly the width, the header puts the turn at the draft column and the goal at the right edge, cutting the goal before the turn, the height never changes with width alone, the rows yield gap, base rule, rule, status, then header on short terminals, and the hint drops at its threshold; `headerRoom` is checked cell for cell; `styles.spec.tsx` asserts in truecolour that the rules are dim and the draft keeps the terminal's foreground; `output.test.ts` checks `scrolling` byte for byte |
| the rule and the frame match the terminal | `packages/app/tests/frame.test.ts` resolves every environment that cannot draw box characters; `line.spec.tsx` draws the rule in ASCII; `welcome.spec.tsx` draws both card styles |
| the header's label holds for the turn | `packages/ui/tests/live.spec.tsx` keeps one word through thinking, writing, a commit, and a running tool; grows a thinking window to its rows over the header and then holds the frame height while reasoning streams; checks with a fake clock that the spinner and elapsed time advance and the interval is disposed when the turn ends; and checks that with motion off only the seconds change. `activity.test.ts` covers the pure pieces |
| the thinking window moves without jumping | `packages/ui/tests/activity.test.ts` keeps the newest rows across paragraphs without blank rows, strips markdown markers, and streams a thought a character at a time asserting each window either grew in place or scrolled by one row; `placement.spec.tsx` bounds the header at `1 + THINKING_ROWS` rows |
| reasoning is previewed in scrollback | `packages/ui/tests/present.test.ts` keeps the first `resultLines` wrapped rows and counts the rest, ends the preview on text rather than a blank row, draws a block whole when the count would hide one row, and reduces to a size at `0` |
| a wrapped row never opens with a space | `packages/ui/tests/present.test.ts` asserts `softBreaks` breaks at the space that would open a row while keeping the text length, and leaves fitting or tab-expanded text unchanged |
| the header holds the finished turn | `packages/ui/tests/live.spec.tsx` ends a clocked turn and asserts the summary takes the header's label at the same frame height, survives a notice, reports failures, clears when the next turn starts, and summarizes a replayed session's last turn untimed; `activity.test.ts` covers the counts and outcomes |
| a call and its result are one block | `packages/ui/tests/fold.test.ts` merges results into their calls, releases out-of-order results in call order, and settles held calls; `styles.spec.tsx` and `actions.spec.tsx` assert the running, done, and failed markers, `Tool(argument)` heads with a `⎿` connector, and no call ids |
| an edit shows what changed | `packages/ui/tests/cards.test.ts` draws only changed lines, numbers each side past the other's insertions, separates joined changes and hunks with `⋯`, heads files only when there are several, draws no numbers without `oldStart`, and marks the words of an edited line but not a replaced one; `present.test.ts` puts `+N −M` on the head at `resultLines: 0`, numbers lines in the gutter, counts only changed lines against the preview and never ends it on a gap, reverses changed words, highlights one side's run at a time, and keeps a failure red; `styles.spec.tsx` pins the size, the gutter, and the reversed words in truecolour; `packages/app/tests/syntax.spec.ts` highlights a preloaded language before the first frame, leaves another plain until its grammar loads, and draws nothing once closed; `packages/fs/tool-fs/tests/diff.spec.ts` records and validates `oldStart` and `newStart` |
| every indicator takes its colour from the palette | `packages/ui/tests/styles.spec.tsx` forces truecolour and pins a running, a finished, and a failed marker to their `PALETTE` tones; `present.test.ts` asserts `styleOf` returns the palette's done, failed, and asking tones, so no component can reintroduce a named colour |
| quit feedback sits above the input | `packages/ui/tests/live.spec.tsx` checks one row of feedback above the input alongside command notices |
| no foreign terminal writes | scripted turn asserts nothing reaches stderr while mounted |

The third is the direct regression test for §1 and the one most worth adding first. The last two are the regression tests for §8.1 and §8.3, which are what the user actually perceives as quality.
