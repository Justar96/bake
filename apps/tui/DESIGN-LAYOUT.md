# TUI layout and structure

How the terminal surface is composed, and the measured constraints that decide it. Companion to `DESIGN.md`, which owns state authority (§3b) and event projection (§4.3). This file owns geometry, render cost, and visual language.

Ink version in this fork: **7.1.1**.

## 1. Frame height and screen ownership

Bake defaults to inline output. `--screen fullscreen` opts into Ink's alternate screen with a fixed transcript viewport and bottom controls. Both modes use the same session projections and presentation rules.

Ink 7.1.1's `shouldClearTerminalForFrame` distinguishes a frame that exactly fills the viewport from one that overflows:

| Dynamic frame | Ink 7.1.1 behavior |
| --- | --- |
| Below viewport height | Incremental updates; new `Static` output prints above the frame |
| Exactly viewport height on non-Windows terminals | Incremental updates without a trailing newline |
| Overflow, shrink from full height, or teardown from full height | Full-clear fallback; accumulated static output is replayed |
| Full height on Windows consoles | Full-clear fallback to avoid bottom-right-cell scrolling |

Inline output retains Ink's accumulated `fullStaticOutput`, so a full-clear fallback can replay the whole session. Fullscreen mounts no `Static` tree; even a fallback redraw contains only the visible viewport and controls.

> **Layout invariant L1.** Inline controls stay within `max(1, rows - 1)` rows, reserving Ink's cursor row. Fullscreen's root occupies exactly `rows` rows, with overflow clipped and input retained on a one-row terminal. A complete transcript must never become a dynamic Ink tree.

The inline repaint, after a resize, a child inspection transition, or a sheet closing, deliberately requests an overflowing frame to reanchor terminal scrollback. Fullscreen resizes its fixed rectangle and remeasures the visible transcript without this replay path.

### Committed history survives resize

In inline mode, one `Static` instance owns the displayed session. Ink clears its accumulated static output when that instance changes, so remounting it for every appended row loses earlier history on a resize that requires a screen replay. The adapter supplies `length` and `slice(index)` for admitted presentation lines. A forward cursor admits at most 512 display lines, 1024 wrapped terminal rows, and 128 Ki UTF-16 text units per batch, then awaits a render flush before continuing a backlog. A single oversized display line is admitted alone. Child inspection unmounts the parent output; returning replays the parent while preserving its composer draft.

Fullscreen's `Viewport` indexes row references from those same immutable batches and presents only visited rows. Its cache retains at most 32 source-row presentations and invalidates on width or presentation-policy changes. `Line` slices wrapped text and styles before creating Ink nodes. A single large message still needs its full text parsed, but its offscreen lines do not enter the Ink tree. Markdown table lines carry their source row offset so a paused view follows the same table row when columns wrap, switch to labeled cells, or grow as more text arrives.

## 2. Region model

Inline mode uses the following regions, top to bottom. Only the transcript is static. Fullscreen replaces the static transcript and live region with the viewport described below.

```
┌───────────────────────────────────────────────┐
│ transcript        <Static>, terminal-owned     │  unbounded, written once
├───────────────────────────────────────────────┤
│ live region       current turn, streaming      │  10 rows, 40% of a tall one; the rest while reasoning streams
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

In inline mode, committed rows go to `<Static>` and are not redrawn during normal updates:

- **Native scrolling.** Terminal scrollback, search, mouse selection, and copy remain available.
- **No retroactive edit.** A row is final once written. Compaction appends a notice rather than mutating displayed history (`DESIGN.md` §4.3).
- **Wrapping is baked at emit width.** A row wrapped at 100 columns keeps those breaks after a resize to 60. Ink cannot reflow text already released into native scrollback.

### 2.1a Fullscreen transcript

`Fullscreen` fills the terminal rectangle. Bottom controls keep their natural, budgeted height; the transcript takes the remaining space, with a one-row keyboard hint when space permits. The viewport joins committed rows and the controller's uncommitted live rows, so the existing `Printed` reconciliation prevents duplicate streamed prefixes. The newest reasoning row is drawn whole while it streams and folds to its preview once a later row follows it.

PgUp/PgDn move a viewport less four rows of overlap, and at least half a viewport. Ctrl+↑/↓ (or Ctrl+Shift+↑/↓) bring the previous or next user prompt to the top; past the first prompt they reach the beginning and past the last they follow output. Ctrl+Home goes to the beginning; Ctrl+End or paging to the bottom resumes following new output. Paging up holds a reading anchor while output arrives. The anchor tracks a character through width changes and rebases when a live row becomes committed fragments. Open sheets and interactions receive their navigation keys first. Session switches and child inspection reset the viewport to the newest output.

A fresh session shows the welcome block until conversation content arrives, then retains the session heading at the start of its transcript. Closing a panel returns space to the transcript while the input stays at the bottom. Ink owns alternate-screen entry and release; exit restores the previous shell screen. Native terminal scrollback does not contain the fullscreen conversation. Saved sessions reopen through `/resume`; transcript search is not implemented. A wheel notch on its own scrolls one row, a fast spin up to six rows a notch, and Alt five times as far; a local macOS terminal, which accelerates the wheel itself, scrolls one row a report. Following output, the hint row is right-aligned, apart from the transcript's left edge, its keys at full brightness and their actions dim. Reading history, the hint row leads with `↓ Latest · Ctrl+End`, yellow `↓ New output · Ctrl+End` once output arrives below, and truncates its keys rather than the offer. It stays one unpadded row, because a reflowing terminal rewraps a full-width row when it narrows. `INK_SCREEN_READER=true` selects inline mode.

### 2.2 Live region — inline streaming

Holds the in-flight turn: streaming assistant text, streaming reasoning, running tool calls, step progress. Reasoning streams under the step it follows ([§5](#reasoning-is-watched-not-re-read)).

Streaming output is unbounded by nature, so the live region does not hold it. The application prints settled Markdown blocks of the answer to the transcript, and the live region draws the unfinished block and any call still streaming ([§8.1](#81-the-input-rests-on-the-bottom-row)). What remains is still **tail-windowed** to `N = min(max(10, 40% of rows), rows - reserved)` rows, section by section (`LIVE_BUDGET`, `LIVE_SHARE`): ten on an ordinary terminal, a growing share of a tall one, whose empty upper half otherwise folded a running step for no reason, and never more than the chrome leaves. While reasoning is the newest row, the region also takes every row the panels leave, claimed after them. Only the newest section is cut, and an older one is shown whole or not at all.

### 2.2a The header — one steady row for the whole turn

The input is framed by two bare rules, exactly the terminal's width, one over the draft and one under it. They only say where typing lands. What the session is doing has one row of its own, the header, directly over the upper rule: the turn's spinner, word, phase, and elapsed time at the draft's column, and the goal at the right edge. The question "is it still working?" and the question "what is it working toward?" are answered on one row next to the input, and the rules never change:

```
  so the loader has to resolve the home before it reads the profile, and
  the session store opens its journal after that

⠰⣿⠆ Kneading…  thinking · 12s                           Ctrl+O ● Goal 3/256
────────────────────────────────────────────────────────────────────────────
> ▌Enter steers the next step                                esc interrupts
────────────────────────────────────────────────────────────────────────────
  chat  ctx ~12% (15k/128k)  ~/bake
```

The word is drawn from the locale's `activityWords` when the turn starts, seeded by the session and its transcript length, and kept until the turn ends. It does not follow the phase, so the label stays one label while the details beside it change. The phase comes from the newest live row: `thinking` while reasoning streams, `writing` while the answer streams, and `running <tool>` while a call streams or has committed without its result, naming the oldest such call and counting the rest of a batch running beside it, `running subagent +2`. `/stop` replaces the word with `Stopping` in red. The word is drawn in the running orange; the indicator beside it is a round ball of dough being kneaded, drawn six dots wide and four tall in three Braille characters: squashed to an oval, pressed into a low dome, folded over from the left, rounded up, then folded from the right, one frame each 150 ms beat in a 1.8-second loop. No frame has a square corner, and every frame stays centred on the bottom dot rows, so the shape changes in place rather than travelling. Screen readers use a static ASCII `>` in place of the dough.

Compaction takes the same cells, as `Compacting history…` and its phase, in the `compacting` blue instead of the running orange, and with its own dough: a sheet laminated the way compaction folds history into a summary. A flat sheet lifts its ends, stands them up, and folds them over to meet in the middle, three layers in a compact block; the block is pressed and rolled out flat again, nine frames on the same 150 ms beat. Both ends move together, so every frame is its own mirror image and rests on the bottom dot rows, and the word beside it never moves. Without motion it holds the pressed block, `⢠⣤⡄`, square where the kneaded ball is round; screen readers still get `>`. `/compact` shows it from submission until the command settles, and a turn that compacts its own context shows it from the live `compaction/start` to its `compaction/end`, then returns to the turn's word. While `/compact` runs, the placeholder says Enter queues: a prompt waits in the inbox, on the pending panel, and runs as its own turn once the compaction settles.

The spinner and the seconds are the only things on the header that move, and the rules never move at all: a moving run the width of the terminal, directly over the input, would distract from the draft while saying nothing the spinner does not. The row is redrawn only on beats where the glyph or the seconds change; without a clock, or with motion off, it holds still.

The glyph takes the rail's first column, level with an action's marker. The draft and status line start two columns in. The goal stays at the header's right edge, beside the turn's processing state or outcome. It reads `● Goal 3/256`: the glyph in its state's colour, and the name and round count in the terminal's own foreground, the count kept in every mode because it is the goal's one number. A held, paused, blocked, or finished goal keeps its glyph and the words for its phase: `○ Goal on hold` or `○ Goal paused` in yellow with `/goal resume continues` while it waits, `✗ Goal blocked` in red with its reason, and `✓ Goal complete` in green with its count. Each glyph says its state under `NO_COLOR`. A dim `Ctrl+O` leads the goal, naming the key that opens its full view; below 60 columns it goes whatever room is left, as every standing row's key does. `headerLayout` gives the turn its glyph and word first and narrows the row part by part, never mid-word, first to yield first: the goal's note (a blocked reason, or the objective when shown), cut with an ellipsis while a few cells of it read and then dropped; the ended turn's counts and rate; the `Ctrl+O` hint; the turn's phase and elapsed time; the goal's name, leaving its glyph and count (`● 3/256`); the count, leaving the glyph; and last the goal itself, dropped whole rather than left as a fragment. A goal with no count, such as a held one, gives up its words and then goes whole. With neither a turn nor a goal the header is a blank row, keeping the input in place. The objective is left to the goal's sheet: a sentence at the row's edge reads as clipped text, not as state. `/settings` turns on `Goal objective on header`, which adds it as the note. The `/goal` command shows the complete goal and its actions. The rules' glyphs are the style resolved from the terminal ([§4](#the-frame-is-chosen-from-the-terminal-not-assumed)), because a full-width run of East Asian Ambiguous glyphs is the one place such a character accumulates error across a row.

When the turn ends, the header stays and says how the turn went:

```
✓ Completed  42s · edited 1 · ran 2 · read 3 · 1 failed · 42 tok/s
```

The glyph and label follow the recorded turn end: a green `✓ Completed`, a yellow `■` followed by why the turn stopped (`Interrupted`, `Blocked`, `Output token limit reached`), and a red `✗ Failed` for an error, whose message can run to paragraphs and stays in the transcript. A completed turn draws no line of its own in the transcript; `- Completed` there repeated this row. The elapsed time is the header's clock when the turn ended. The counts are the turn's committed calls grouped by past-tense verb, with edits first, followed by the number of calls that failed. They are read from the transcript rather than tallied as the turn ran, so they agree with the session log however results arrived. The final answer's generation speed closes the row: the provider's output tokens over the logged time from the first streamed token to the finish, so the rate is the generation speed and not the wait for the model to start. It is shown only for a sample that means something, at least a second (`RATE_MIN_MS`) and 64 output tokens (`RATE_MIN_TOKENS`); four tokens over 74 ms is not a speed. A step that ends in tool calls, an interrupted answer, and a log with no usage or no measurable time report no rate rather than a guessed one, and the log keeps the numbers either way. The running label and the summary are the same row, so the input never moves between them, and a turn that has not spoken yet costs no row at all. The summary holds until the next turn starts. A resumed session shows its newest ended turn the same way, without a time, because no clock watched it run; that turn is read from the transcript once, when the surface mounts, so later commits never read history again. Before any turn has ended, the turn's side of the header is empty.

### 2.3 Interaction — modal, and it wins

Approvals and prompts take priority over the live region. A user answering "may I run `rm -rf`?" does not need concurrent token streaming; they need the command, the cwd, and the choices, unambiguously. When an interaction is open the live region collapses to a single summary line.

### 2.4 Notice — transient, bounded, dismissible

Command results, errors, hints. Cleared by the next submit. Bounded per §5.

### 2.5 Status — one line, degrades by priority

Fields in display order, lowercase and colon-free, each a dim word beside its value:

```
<model>  think high  ctx ~11% (15.2k/128k) · compacts at 80%  ⎇ main +2 ~3  in 42.3k  out 3.1k  cache hit 81%  update  cwd
```

There is no state word. The header over the composer says what the session is doing, in colour and in words, and the composer's placeholder and hint say whether Enter starts a turn, steers one, or is refused; a third copy under the composer repeated them on every frame. The model leads because it is what the row exists to say, and it needs no label. `statusFields` in `packages/ui/src/status-line.ts` builds the fields and `fitStatus` fits them, and there is one layout in every mode: which other rows stand around the composer changes nothing here.

The context reading leads with its percentage, `ctx ~11%`, since it is what a user compacts on, and brackets the absolute count after it. When the route's automatic compaction threshold is known (`ContextUsage.compactAt`), a dim mark follows: `· compacts at 80%`, or `· compacts next` once the estimate has reached it; narrow, it is `▸80%`. A profile without compaction, or a route whose threshold is unknown, draws no mark. Only the reading is coloured, never its label or the mark: it keeps the normal foreground while there is plenty of room, then warms through `CONTEXT_RAMP` in `palette.ts`, soft yellow, yellow, orange, and red from 90% of the window (`contextTone`). Without a mark the steps start at 60%, 70%, 80%, and 90%; with one, orange starts ten points below it and the steps before it lead it by ten points each. The percentage says the same under `NO_COLOR`.

The input, output, and cache-hit totals are one field: the session's billed tokens, summed over every request. They appear once a provider has reported usage; cache hit is the share of billed input read from the provider's cache, rounded down, and appears only when the provider reports cache traffic, so a provider without a cache shows no false 0%. The field narrows to `cache hit 81%`, because whether the prompt cache still works is something a user can act on and the totals are only what the session cost. Values use the terminal's normal foreground, and labels, the totals, and the cwd stay dim. Colour is kept for a reading that needs attention: a cache hit is plain while it is healthy, yellow below 70%, and red below 30% (`cacheTone`), and the git counts say what they wait on. Inside a git repository the branch follows the context reading, because it says where the session's work lands: `⎇ main +2 ~3 ?1 ↑1` (`gitField`), staged green, unstaged yellow, and conflicts (`!N`) red, while untracked paths and the distance from the upstream stay dim. A clean tree shows the branch alone. The classic frame draws it in ASCII, without the branch glyph and with `^` and `v` for ahead and behind. The thinking level reads `think high` for an explicit model effort or the adapter's advertised default; a provider default without an advertised level is named as such, and a model without reasoning metadata has none. The level warms with the effort (`thinkingTone`): `minimal` and `low` dim with their label, `medium` plain, `high` blue, `xhigh` orange, and `max` pink; an effort a provider names otherwise stays plain.

Fields keep their order as the row narrows and give way at their own rank (`RANK`), whole or to a shorter complete reading, never cut mid-number, since `cache hi` or a clipped meter is a different number. First to yield first: the context's absolute count (unless the context is at 70% or more), the input and output totals, the compaction mark's words, the cwd, the update notice, the git counts, the cache hit, the compaction mark, the branch, and the thinking level. The model is cut from its end last, down to eight cells. `ctx ~N%` never yields. From 70% the absolute count holds until the thinking level has gone, and from 90% it takes cells from the model before it goes. The cwd, already shortened against home by the application, is the row's filler: it is counted whole until its rank, then cut from its start into whatever the other fields leave so it keeps the workspace's name, and left out rather than drawn as a tail of fewer than six cells, which names nothing.

```
deepseek-v4-flash  think high  ctx ~11% (15.2k/128k)  ⎇ main +2 ~3 ?1 ↑1  in 42.3k  out 3.1k  cache hit 81%  ~/bake
deepseek-v4-flash  think high  ctx ~11%  ⎇ main +2 ~3 ?1 ↑1  cache hit 81%
deepseek-v4-flash  think high  ctx ~11%  ⎇ main  ~/bake
```

Subagents are not a status field. One dim row between the base rule and the status line counts them in the grammar every standing row shares: the transcript's `↳` in the rail, the name and the total, then the working children, the done ones, and those that cannot be read, in lowercase and without a colon, `↳ Subagents 12 · 2 working · 9 done`, and its key, `Ctrl+G`, dim at the right edge. It sits under the input because Down from an empty composer selects it. Names are the sheet's: past a few children a row of names would be cut from its end, naming some and hiding the rest. On the sheet the glyph's shape carries each child's activity, so its colour is free to say which child it is: every child takes an identity tone from `AGENT_TONES` by catalog order, hues no state uses. Past a dozen children a two-line entry each would push the working ones off the page, so the sheet opens with a bar filled by the done children, and the counts by state beside it. Each child is one row, and only the child under the pointer takes a second, dim line of how it runs and its id; children that finished cleanly are dimmed until selected, so the eye lands on those working, failed, or stopped. The sheet keeps the pointer's row and its detail line in view together. The row is one line at any width and it is claimed before any panel above the input, so the input's row does not depend on what streams over it. Every standing row's key gives way below 60 columns, as the composer's hint does.

The access boundary is not a status field. It is named where the session opens: under the session line in a fresh session's welcome block, and after the heading of a session that opens with history or a child inspection. Its label stays dim, while its value is blue for `read-only`, green for `workspace-write`, red for `danger-full-access`, and yellow for custom or automatic policies. It prints once into history, so it records the mode the session opened with; profiles without the permission projection omit it.

Left-packed, two spaces apart, stopping where the fields stop — not justified to both edges, per [§6a](#chrome-separates-by-framing-the-input-not-by-aligning-the-status-line). It sits directly under the base rule, with no blank row between them, and starts at the draft's column, as the header's label does. Never wraps to two lines; a wrapped status line silently costs a row of live region and can tip L1.

### 2.6 Composer — a cursor-following window

The composer displays one to five physical rows under the rule, with the prompt marker at the left edge and the draft at the rail's column. There is no box and no fill: the draft is in the terminal's own foreground, so it reads on any theme, and placeholder and hint text are dim. It wraps the draft at its actual available width, including the marker rail and contextual hint, before choosing the visible rows. Moving Home, End, or through the middle of a wrapped paragraph keeps the drawn caret visible. The caret is one cell in reverse video, over the character it precedes or over a blank cell after the text, so moving it leaves every character where it was. `caret.ts` writes the reverse-video codes into the text instead of using Ink's `inverse`, which Chalk drops on a terminal it reads as colourless; `NO_COLOR` keeps reverse video, which is not colour.

`wrapDraft` wraps the way an editor does. Rows break after whitespace, and the whitespace at a break hangs past the row instead of opening the next one, so no wrapped row starts with a space the user did not type. CJK characters are break opportunities of their own. Thai, Lao, Khmer, and Myanmar drafts use ICU word boundaries. A word longer than a row starts a new row and splits only between graphemes. The workspace patches to `string-width` and `slice-ansi` count the spacing cell in Thai `ำ` and Lao `ຳ`, keeping Ink measurement, truncation, and caret columns aligned. Tabs are drawn as spaces to the next four-column stop, because `string-width` measures a tab as zero columns while the terminal moves to its own stop, which leaves cells where the layout did not put them and stale text under them. The layout ignores the caret and is one column narrower than the row. The caret covers the cell it precedes, and only after a row's text, or over whitespace hanging past it, takes that last column, so moving through the text never moves a word. Beside a hint, that column is the first of the two that separate the draft from it, so the hint costs no more than it did. The hint appears beside the caret. Wide characters use terminal cell widths. IME candidate placement at the caret is not yet implemented.

The window moves only when the caret would leave it: typing at the end keeps the caret on the bottom row, and Up inside a tall draft moves the caret up the window before the text scrolls under it. Up and Down move between the rows drawn here, at the width `draftWidth` gives, so the input handler and the window agree on what a row is ([DESIGN.md §5](DESIGN.md#5-input-flow)). A draft taller than the window says what it hides at both edges. A dim `^` in the rail of the first visible row marks rows above, and a dim `v` in the rail of the last marks rows below; the prompt marker keeps the draft's first row. Where that edge row does not carry the hint, the hint's slot counts the hidden rows instead, `+N above` or `+N below`, dim and ending where the hint ends. A count wider than the hint is left out whole, and below `HINT_MIN_COLUMNS` the slot and its counts go while the rail markers stay:

```
────────────────────────────────────────────────────────────────────────────────
^ Then thread the value through startup.ts and the session store,       +2 above
  and add a regression test that sets both DSH_HOME and --home to
  prove which one wins.

  Keep the public API unchanged.▌                                    Enter sends
────────────────────────────────────────────────────────────────────────────────
```

An idle, empty composer names what it takes: `Ask anything · / commands · @ files`. The parts after the prompt are dropped whole below `HINT_MIN_COLUMNS`, or where they do not fit, never cut. The running, compacting, blocked, and inspection placeholders are single sentences and stay as they are. The composer carries no standing key hint: the line-break key is named once, on the welcome card, and `/help` ends with every composer key.

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

The composer's rule and the divider that opens each turn are full-width runs of box-drawing characters (`─`), and the welcome card is framed in them (`╭─╮`). Two kinds of terminal cannot draw them:

- One that is **not encoding UTF-8** writes the bytes through as mojibake, so the frame becomes punctuation on every row.
- One configured to draw **East Asian Ambiguous** characters two cells wide draws a full-width horizontal run at twice the width Ink measured. The frame wraps, and Ink's own row arithmetic is wrong from then on — this is the damaging case. Every other Ambiguous character on this surface (the turn marker, the selection pointer) sits alone in a fixed-width rail, where a terminal that draws it wide shifts one row by one column; a border run accumulates that error across the whole line.

`resolveFrame` in `packages/app/src/frame.ts` decides once, before the first frame, and `@dsh-tui/ui` takes the answer as a prop — the presentation layer reads no environment. Encoding and `TERM` come from `LC_ALL`/`LC_CTYPE`/`LANG` in POSIX order; Windows Terminal sets neither but draws UTF-8, so its `WT_SESSION` stands in for both. Native Windows needs neither: Node writes to the console as UTF-16 whatever the code page, so every Windows console draws the frame. Ambiguous width is a terminal *preference* and cannot be detected, so a CJK character locale stands in for it; outside Windows Terminal, Windows names that locale in the system rather than the environment, and its console host draws Ambiguous characters wide under a CJK code page. `composerFrame` in the profile overrules the lot, which is the only answer for a terminal the environment describes wrongly.

The ASCII fallback is laid out against the same widths and spends the same rows.

### The composer yields structure before it yields content

The hint costs whatever its locale needs. It is structure around the one field on the surface the user is actually composing in, so it gives way before the draft does, at a named width rather than by shrinking; the header's goal is cut before its turn is (§2.2a):

| Below | What goes | Why not shrink it |
| --- | --- | --- |
| `HINT_MIN_COLUMNS` (60) | the composer's right slot, and the hidden-row counts it holds | a hint truncated to fit has stopped being help, and the key it names still works unnamed |
| `HINT_MIN_COLUMNS` (60) | the idle placeholder's `/ commands` and `@ files` | a half-named key teaches nothing; typing `/` or `@` still opens its menu |

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
> Ask anything · / commands · @ files
ready   deepseek/chat                       ctx 12%   turn 3   0f3a9c
```

`run`, `read`, `plan`, `ask`, `error` read at a glance, survive every font and locale, and stay legible pasted into a bug report. Approvals join the same grammar instead of inventing their own marks, which is why `ask` is a verb rather than a symbol. Reasoning takes no verb: it is the model's prose rather than an action, so it is a paragraph at the rail like the answer, dim and italic, and the answer's `<` marks where the reply begins. A label on every block would make the chat a log of actions, and the output column would wrap the working-out narrower than the answer beside it.

| Row kind | Marker | Columns | Color |
| --- | --- | --- | --- |
| user | `>` | text at 2 | default, bold |
| assistant | none | text at 2 | default |
| action | the tool's icon (`✦` a skill, `↳` a subagent started, `→` a message to one, `☐` a plugin's task list, `◌` work left running in the background; `●` for any other), then `Tool(argument)` | head at 2 | marker orange and blinking on and off while running, green when done, red on failure; tool name bold |
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

- **Color is semantic only, from one palette**: `palette.ts` holds six saturated semantic tones and the two neutral tones of a result's preview zone, and every component names a role rather than a hue, so a marker, its verb, and the header agree. Orange is `running`: the header's word and spinner, and a running action's marker. Blue is `compacting`, deeper than the reference blue and the cyan of `asking`: the header's word and laminating spinner while history is compacted, by `/compact` or inside a turn, and the transcript's notice that the context was compacted. Green is `done`: a finished action, a completed turn's summary, and an added line. Red is `failed`: a failure, an error, a removed line, and the word of a turn being stopped. Cyan is `asking`: the composer prompt, a selection, staged attachments, and an action in progress on the task list. Yellow is `waiting`: a question, an approval, a picker, queued input, a notice, and a turn that stopped rather than failed. Dim marks reasoning and secondary interface metadata such as line counts and a call's description, and tool arguments use normal brightness. A result's preview has no background: its plain text is in the `output` grey, which is quieter than an answer and brighter than dim, so output is supporting material without receding to the level of the working-out. The surface draws no background anywhere; failures, diffs, and syntax colour keep their own tones in the preview. Never decorative, and never the only carrier of a state: every tone is paired with a word, a glyph, or motion, so a 16-color or `NO_COLOR` terminal loses no meaning. Honor `NO_COLOR` and non-TTY.
- **Code in a diff is the one exception**: syntax colour says what kind of token a run is, not what state anything is in, and it comes from a highlighting theme rather than the palette. It is laid over the side's tone and never replaces it: the sign, the line number, punctuation, and plain words keep green or red, so a line still shows as added or removed however much of it is highlighted. [An edit shows what changed](#an-edit-shows-what-changed) has the rest.
- **Weight carries structure**: an action's marker and tool name are bold, and so is `error`, so a scan down the left of the transcript lands on each action rather than on its arguments.
- **Spinners only on a TTY** with color enabled. Under `useIsScreenReaderEnabled`, replace motion with discrete state transitions — a screen reader announcing a spinner frame-by-frame is unusable. The UI package reads no clock. `App` times and animates the header only when the terminal owner passes a `clock` prop, and animates it only while `motion` is not false. The runner always passes the clock and turns motion off under `NO_COLOR`, so the header's wave rests centered, a running action's `●` does not blink, and only the elapsed seconds advance, once a second. Without a clock, or under a screen reader, the header also leaves out the elapsed time and changes only when the phase does.
- **Resize** via `useWindowSize`, which re-renders on `SIGWINCH`. Recompute budgets from it; never cache `columns`/`rows`.
- **Paste** via Ink's `usePaste`, which owns bracketed-paste mode and keeps pasted text off the `useInput` channel. Our `terminal.ts` enables paste mode for the pre-mount window; these must not fight over the same escape sequence — one owner, chosen explicitly.

## 7. What this rules out

Recorded so the questions do not get relitigated:

- **Split panes / sidebars.** Every column spent on chrome is taken from prose and tool output, which are the product. A terminal is not a window manager.
- **Progress bars for model output.** Token counts are not a denominator; there is no total to divide by.

## 6. Spacing and zones inside the chat area

`prototype/chat.mjs` renders one turn at 80 and 160 columns.

### Prose and tool output follow terminal width

Assistant replies and reasoning wrap at the current terminal width less their two-column rail. Indented tool output wraps after the verb column; the label joins the text when the terminal is too narrow to hold both columns. Neither has a fixed maximum width. Width changes remeasure live rows; committed scrollback is replayed when the terminal needs to reanchor after resize.

### A line wraps, never truncates

A line the user cannot finish reading is worse than a ragged one. Long output lines wrap; they are never cut with an ellipsis. Horizontal truncation is reserved for surfaces where the full value is one keystroke away — a completion row or a picker — never for a line of a result that already cost a tool call. The one exception is model-facing text no card reformatted: a successful result's line past 240 cells is cut ([below](#a-call-and-its-result-are-one-zone)), since one line of compact JSON would otherwise wrap into a screenful, and the whole text stays in the session log.

How many lines of a result are drawn at all is a separate question, answered by [§7b](#a-results-output-is-previewed-not-replayed). Every other line that is drawn is drawn whole.

A wrapped row never opens with a space. A word that ends exactly at the width leaves the space after it for the next row, where it would indent that row by one column. `softBreaks` in `packages/ui/src/present.ts` turns each such space into the line break it stands for before the line is drawn. It keeps the text's length so styled spans still line up, and leaves the text as it was when wrapping rewrites anything else, such as a tab.

### A call and its result are one zone

A call and its result draw as one block, opened by the tool's icon at the rail and headed as the call itself. `iconFor` in `packages/ui/src/icons.ts` gives a shape of its own only to the calls that change what the session knows, who does the work, or what is left to do — `✦` a skill, `↳` a subagent started, `→` a message sent to one, `☐` a plugin's task list — guesses a plugin's tool into those from the whole words of its name, rings any other call that asked for `run_in_background` with `◌` and tags its head a quiet `background`, and draws `●` for every other call, commands, reads, edits, and searches alike, since the head already names the tool. Every icon is one cell wide with no emoji presentation, like the other markers, and the head still names the tool, so the icon never carries meaning alone. Then the head: the tool's name, its words capitalized and run together (`toolLabel`: `bash` is `Bash`, `read_file` is `ReadFile`), and its argument in parentheses, `Bash(cargo check --workspace 2>&1 | head -20)`. The argument is the tool presenter's title. A call with no card is headed by `argumentsTitle` in `packages/ui/src/project.ts`: the first of `command`, `cmd`, `file_path`, `path`, `pattern`, `url`, and `query` that holds non-blank text is the headline, and any further lines of it stay under the head; otherwise its fields are, as bounded `key: value` pairs and never raw JSON, each string folded onto one line and cut at 40 cells, a short list of plain values kept as its JSON (`[1,2]`) and any other list shown as `[first, +N]`, and a record as its first field (`{name: build, …}`) unless its fields are all plain and fit. Arguments that are not a JSON object stay as the model sent them. Either way the headline's first line is cut at 96 cells (`HEADLINE_CELLS`) with an ellipsis, measured in terminal cells without splitting a grapheme. A result with no card, which includes a `generic` result card that omits its `content`, shows its model-facing text with each line of a success cut at 240 cells (`RAW_LINE_CELLS` in `packages/ui/src/present.ts`); a failure's lines stay whole, because the failure is what the reader has to read. While the call runs, the marker blinks: shown in orange, then hidden, a clean on and off with nothing at half brightness, and a space in its cell so the head never moves. When the result arrives the marker holds, green, or red on failure. A title's first word is dropped when it only names the tool's action again, as `Grep` does in `Grep(Grep TODO)`; a command keeps every word. No second row announces the outcome, and no call id is drawn; the id identified a result's row with its call, and there is no longer a separate row to identify. Output indents to the output column beneath the head, and its first line carries a dim `⎿` in the verb column that hangs it from the head, so no whitespace divides a call from what it produced. A changed line's number keeps the verb column instead, since the number is what reads against the code.

`Actions` in `packages/ui/src/actions.ts` does the merge. It holds each call until its step ends (`step/end`, or the model's next message or the turn end in a log without one) and releases the step's calls together, in call order, so a fast second call cannot print above a slow first one. The live region draws the held calls in the shape they will print in; nothing is committed and later drawn again. The shape holds from the first streamed call: the calls the model is still streaming, the calls its committed message announced but the loop has not dispatched yet, and the calls running or finished are one block, so two streamed calls are already `run 2` with both branches, and the count never drops while the loop reaches each call in turn. A block that changed shape on the way would give up rows the frame holds blank until history next prints ([§8.1](#the-frame-never-rises)), and since reasoning prints nothing while it streams, those rows would stand as a gap between the reasoning streaming under the step's block and the header.

Two process-local events decorate the held calls without being held themselves. `agent/tool-progress` carries a running call's newest output, and `CallProgress` in `packages/ui/src/progress.ts` keeps its last `LIVE_TAIL_LINES` lines, of which the call draws its newest `resultLines` under its head, one row each, cut to the width. `agent/tool-executed` marks a call finished as soon as its execution ends: its marker stops blinking and takes its outcome's colour, though its result commits later, in call order, so a fast second call reads as done while a slow first one still runs. A call's live state is dropped when its `tool/result` is logged, and every call's at the step end. It is never printed, so the transcript, and a resumed session, show only the logged result.

Background work ends after its call has. The job controller tells the agent with a notice the log keeps as plugin context, and that notice prints as the head of the call that started the job, so the two read as one job's start and end: `◌ Bash(npm run dev)  bash-1 finished · exit code: 0`. The ring takes the job's outcome: green, red when the job failed or the command exited non-zero, yellow when it was stopped. Every other kind of plugin context stays out of the transcript. While a job runs, the row under the subagents row says so ([§4.6 of the design](DESIGN.md#46-sheets-and-the-subagents-row)).

A step that made two or more calls prints them as one block, because they were one decision the model made:

```
● ran 2 · edited 1 · 1 failed
├ ● Bash(bun test tests/parser.test.ts)  exit 1
│ ⎿      bun test v1.3.0
│        +4 more lines
│         1 fail
├ ● Edit(src/parser.ts)  +1 −1
│     14 - const parts = line.split(",")
│     14 + const parts = splitQuoted(line, ",")
└ ● Bash(cat missing.txt)
  ⎿      cat: missing.txt: No such file or directory
```

The head counts the calls by verb in the order they first appear (the calls with an icon of their own count by its words: `spawn` for subagents started, `send` for messages to them, `load` for skills), present tense while any runs and past tense after, then how many failed, in red. Its marker is the step's state: blinking while a call runs, then green, or red when one failed; it is the calls' icon when they are all of one kind, and `●` for a mix. Each call hangs from it on a branch in the rail, `├` and `└` for the last. The branch takes the rail, and the call's icon moves just past it as a badge that keeps everything a lone call's marker says: its kind, blinking while the call runs, then green, or red on failure. So in a step of three delegations, `↳ spawn 3`, the reader sees which subagent is still running and which already answered. A call that finished cleanly under the head's own icon would only repeat the head, so its badge cell stays blank and the tool's name stands alone; a running or failed call, or one of another kind, keeps its badge. The tree itself is structure, so every branch, the `│` that carries a call's lines down to the next call, and each `⎿` are dim and hold still; a blinking branch would open a gap in the tree, so the badge blinks instead. A call that failed also says so in its own head, its name and argument red, since its output may be folded away or empty; the head's `1 failed` counts it. The call's text starts two cells later, past the badge, and its output keeps its column. No blank row separates the calls; the stem is what joins them. A step with one call draws it on its own.

A running step taller than the live region fits itself to the window rather than being cut from the top (`fittedGroup` in `packages/ui/src/present.ts`). Cut like prose, the block lost its head first, which is the line that says what the step is doing and the marker that says it is still running. Instead the block gives up detail oldest first: each finished call folds to its head line, the oldest first, so the output just read stays in view longest; once every finished call is one line, the oldest calls fold into a single dim `+N earlier calls` branch until the rest fit. The head, the newest calls, and every running call stay. On a window too short even for that, the block keeps what says the most per row, in order: the head, the newest call's head line, and the count of the rest, giving up its opening blank first. The window is measured in wrapped rows at the current width, so the same rules hold for a narrow split pane and a wide monitor, and they are re-applied on every resize. Nothing is lost: the step prints to history whole once it ends.

A code-mode script's calls hang from it one level in, on a tree of their own that starts where a result's `⎿` does, each call's marker past its branch as a badge, as in a step:

```
● Script(Find TODOs)  12 calls · 1 failed
  ⎿      for (const path of paths) {
           const text = await tools.read({ path });
         }
  ├ ● Read(src/m0.ts)  2 lines
  ├ ● Read(src/m1.ts)  2 lines
  ├ +3 more calls
  ├ ● Read(src/m5.ts)
  │ ⎿    Permission denied
  ├ +4 more calls
  ├ ● Read(src/m10.ts)  2 lines
  └ ● Read(src/m11.ts)  2 lines
         Script output
         ["src/m3.ts"]
```

The script's head counts its calls, and how many failed once it has ended, as a step's head does. A call that succeeded folds to its head and size, since the script's own result is what it worked toward; a failed call keeps its error under it, its head red, and a running call blinks and shows its newest output. The tree's `│` carries a call's lines down without moving their text off the output column. A script can loop over a whole workspace, so its printed block keeps its first two calls and its last two, and up to three failed or unfinished calls between them; the rest fold into dim `+N more calls` branches, and a count that would stand for one call is drawn as that call. Each call is in the session log. While the script runs, the live region fits it as it fits a step: the oldest calls fold into `+N earlier calls`, then the source gives way, and on the shortest window the head drops its count before the newest call goes. A failed script's result is labelled `Script error`, a finished one's `Script output`.

Tool arguments use normal brightness, and a result's preview draws its plain text in the `output` grey, with no background ([§6](#6-color-motion-accessibility)). The marker and tool name are bold; a call's description and a `+N more lines` count are dim. Failures retain red emphasis throughout, and diffs retain red/green emphasis, under their syntax colour. Among reasoning, actions, and answers, only reasoning is dimmed, and its text is italic as well, in the transcript and the live region alike. Dim alone put the working-out in the same weight as a call's description, one column away from it.

A call's own lines sit under its head: the rest of a multi-line command, then the card's description and the input a card chose to show. They are bounded as output is, first and last lines around a count, and at least one line of each always shows, so a script the model wrote prints a few rows and a description survives `resultLines: 0`. An input the title already names, such as a search's pattern, is left out. A structured input is drawn a field or an item to a line, and an item of plain values is shown as its values. A card's fenced block is drawn as its code, since two lines of backticks around a failed command's error are the one piece of markdown this surface would otherwise print.

### Indentation separates columns; a blank row separates sections

Within a turn the levels are: rail glyph at column 0, prose and reasoning at 2, tool output at 9. Indentation carries every separation it can, and it carries most of them — an answer at the rail is never confused with output under a verb.

Two things it cannot carry. Two actions in a row share the verb column, so the second, drawn directly under the previous call's output, looks like more of that output. And reasoning shares the rail with the answer that follows it: the answer stops being dim and italic, which under `NO_COLOR` is the only difference left — the working-out and the conclusion would look like one paragraph. The answer carries no marker of its own; its full-brightness text at the rail, and the blank row that opens it, are what say the reply has begun. A final answer ends with no row of its own: its generation speed closes the ended turn's summary on the header ([§2.2a](#22a-the-header--one-steady-row-for-the-whole-turn)), where one row carries the turn's numbers.

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

✓ Completed  42s · ran 2 · 42 tok/s                        Ctrl+O ● Goal 3/256
──────────────────────────────────────────────────────────────────────────────
> ▌Ask anything · / commands · @ files
──────────────────────────────────────────────────────────────────────────────
  chat  ctx ~12% (15k/128k)  in 42k  out 3.1k  cache hit 81%  ~/bake
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

The slash takes the rail's first column, so the row breaks the left edge the way a command breaks the conversation and reads without colour; the name is bold, as an action's verb is, and the arguments are the user's own words at full weight. `command/done` projects its text as a notice placed `command`, which draws the branch in place of a `note` verb, the branch dim and the text red when the command failed; later lines of a long outcome, such as `/help`, sit under the first at the text column, and the outcome wraps at the full width, as tool output does, since it is as often a table as a sentence. A notice no command produced keeps its verb.

`Ctrl-C` arms quitting and displays one row of feedback directly above the composer, for `doubleInterruptMs` (two seconds by default) or until any other key dismisses it. The runner owns the timer and armed state independently of command notices, so neither replaces the other.

### The right slot is contextual, never decorative

The composer's right edge can hold `↵ send`, `esc interrupt` while a turn runs, or `↑↓ select` while an overlay is open. It must stay contextual. A permanent hint teaches nothing after the first day and becomes noise on a surface the user looks at hundreds of times a day — the same reason nothing here animates on keystroke. The one other thing the slot holds is a count of draft rows the window hides, on the edge row that is not the caret's ([§2.6](#26-composer--a-cursor-following-window)); it says something only while it is true. The newline key is not a hint: the welcome card names it once, into scrollback.

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

A picker does offer scrolling, so its edges say `↑ N more` and `↓ N more`, each spending one of the picker's rows; at two rows there is no room, and the `i/n` position on the key line carries it. The window moves only when the selection would leave it, never recentres, so walking inside it moves the pointer and nothing else. Columns — label, status, description — are sized over every choice rather than the visible ones for the same reason. An action that must stay reachable, such as a new session under a long history, is `pinned` below the scrolled list instead of at its end. Choices in groups, such as `/model`'s providers, are headed once per run, and a heading spends a row like a choice does; scrolled past it, the top edge names the group before its count, so no row loses what it belongs to. Facts in aligned columns after the label give way from the right on a narrow terminal before a label is cut too short to tell apart. A levels row, such as `/model`'s efforts, is charged to the picker's rows, and only the current level is named, between arrows, where the whole row does not fit. Every picker, and every approval, question, and sign-in panel, reads in one order: a bold title, the input after `>`, the choices, and dim keys last. A picker whose choices carry help, such as a `/settings` page, keeps one or two dim rows above the keys for the selected choice's, charged to its rows and sized over the whole list, so the keys do not move as the pointer does. `packages/ui/src/choices.ts` holds the filter and scroll rules; `choices.test.ts` checks that the selection is always shown and the rows never exceed the limit.

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

Reasoning streams whole in the live region while it happens, under the step it follows, as a dim italic paragraph at the rail with no verb. While it is the newest row, the live region takes every row the panels leave, so the thought is drawn at its length rather than a few rows of it; its own paragraph breaks do not open sections, so when it outgrows the screen its oldest rows clip from the top the way a terminal scrolls, and an older section above it, such as the running step, is dropped whole. `streamingThought` in `packages/ui/src/present.ts` parses only as many of its newest paragraphs as fill the window, so a long thought costs each frame the rows it draws, not its length. Once a later block starts, the thought folds. When it prints, or when the step commits, the transcript keeps a preview, the way it keeps a result's: the first `resultLines` wrapped rows, a dim italic paragraph at the rail, then a dim `+N more lines`.

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
- **Diff text is display text.** Each side's lines pass through `toolText` before their words are compared, so a carriage return, a tab, or an ANSI code in the file on disk is escaped, expanded, or stripped before it reaches the terminal, and the reversed words still land on the characters drawn. Lines are paired by their raw text, so a change only a control made still shows. A CRLF ending is dropped.

### A command shows what it changed

A model often edits files through the shell instead of `edit`, as in `sed -i … && node test.cjs`. When the shell tool reports the files its command changed, the terminal card draws them as a section of the call's own block, under the output:

```
● Bash(sed -i 's/= 3/= 5/' a.js && cp t b.js && node t.cjs)  +2 −1  exit 1
  ⎿      not ok 1 - retries
         +6 more lines
         # fail 1
  edited a.js
       4 - const retries = 3
       4 + const retries = 5
  edited b.js  new
       1 + module.exports = {}
```

- **It stays in the block.** `Actions` prints the rows that are not calls before the held call they follow, so the changes travel on the result as the card's `changes`, not as a row of their own that would print above its command.
- **Each file has a path line.** `edited`, the past tense of the `edit` verb, takes the verb column, in bold. The path follows in reference blue, then, dim, how the file changed when it was not a plain edit: `new`, `deleted`, `renamed from <path>`, `binary`, `too large`, `mode`, or `symlink`. The file's hunks are drawn as an edit's are. A file with no lines drawn, because the tool sent no hunks or the bound was spent, shows its own `+N −M` on the path line.
- **The head says how much changed.** The total `+N −M`, over every file the tool measured, sits before the exit status. At `resultLines: 0` the block is its head alone, and the head adds the number of files when there are several, since no path line is left to say so.
- **Output and changes are bounded apart.** The output keeps its first and last lines around `+N more lines`. The changes count their changed lines against `resultLines` on their own, as an edit's do, and a file with none to draw counts its path line. Files are drawn while some of the bound is left, each with its path line; the rest, and any the tool left out, are counted as `+N more files`. A count standing for one file is replaced by that file's path line, which costs the same row.
- **Tones are per group.** A non-zero exit draws the output red, as any failure is. The changes are not the failure, so they keep green, red, reversed words, syntax colour, and the path's blue, in a single call's block or a step's.
- **Caveats are dim lines under the section.** The tool marks a list that may include other activity in the workspace, because other work ran during the command, and one that may be incomplete because the comparison ran out of time.

A result without `changes` draws exactly what it drew before.

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

The composer rests on the terminal's bottom rows from the first frame. In inline mode, `frameOutput` in `packages/app/src/output.ts` moves the cursor to the bottom row before Ink draws, so the first frame grows upward from there and whatever the shell printed scrolls up above the session line. Committed history prints through `Static` above the frame, the live region follows it, and the chrome follows the live region with one blank row between. A fresh session keeps its session line directly above the input on the bottom rows. When optional `AppProps.version` is supplied and the session surface mounts with an empty committed transcript outside child inspection, a `BAKE v<root version>` welcome card prints once through `Static` in place of that heading, carrying the same session line inside it, so the block costs no row beyond the one the heading takes; resumed history has no banner and prints the heading alone. Under `/help` and `/changelog` it names `Ctrl+J`, the key that breaks a line in the prompt in any terminal, with `/terminal-setup` beside it for Shift+Enter; that is the one place the key is taught, and `/help` names Alt+Enter with it. The description column is 48 cells wide beside `/changelog`, which bounds that row's text in every locale. The card is at most 64 columns wide, draws the line style resolved from the terminal, and drops its border below `FRAME_MIN_COLUMNS` (40). It belongs to terminal scrollback, not the dynamic region, and each line that prints scrolls history up by its rows rather than moving the input down.

An input that followed the newest line down the screen, as Claude Code places its prompt, moved on every printed line until history filled the screen, and moved again whenever the frame shrank. A tall terminal spent most of a session in that phase.

> **Layout invariant L2.** The composer rests on the terminal's bottom rows, with the subagents row while there are children, the status line, and (in inline mode) Ink's cursor row beneath it, at every size and in every state. Above it are the rule, the header, and input-owned panels (completion, notices, queued input, quit feedback), a blank row, and the newest transcript or live line. The blank is one row unless the frame is holding rows something above the input gave up; those rows are blank too, until printed history takes them.

#### The frame never rises

The held-height and replay rules in this section apply to inline output. Fullscreen uses a fixed-height root and gives any space released by panels back to its transcript viewport.

A frame drawn on the bottom row stays there while it and the history printed above it in the same render fill at least the rows the previous frame did. Most shrinking is made up that way: an answer's line leaves the live region as it prints, and a step's calls leave it as they commit when the step ends. A thought that streamed taller than its preview is made up only in part: the preview's rows print, and the rest are held blank over the controls until the answer's lines or the step's commit take them, sooner when an answer follows than when calls do. What is not made up — a completion menu or picker closing, a sheet closing, a notice clearing, a task finishing, a queued message leaving the panel — would lift the composer by the rows it gave up and drop it back as the next lines printed. A sheet is the largest case, and the one exception. Opening it scrolls the history above the frame into the terminal's scrollback, which cannot be drawn back down; held, its rows would stand as a blank block over the composer, most of a screen, until a turn printed enough to fill it, and an idle session prints nothing. Closing a sheet therefore repaints ([Narrowing and growing taller repaint](#narrowing-and-growing-taller-repaint)): the history is replayed back down against the controls and the input stays on the bottom rows. Opening one does not, since growing never leaves a gap.

So `useHeldHeight` in `packages/ui/src/app.tsx` gives the frame a minimum height: its previous height less the rows this render prints. The printed rows are measured the way `RowView` draws them, before the render that prints them, because a floor corrected afterwards would already have moved the composer once. The rows the content does not fill are a blank spacer under the live output and over the controls. What streams stays against the history it continues, the panels stay against the input, and the next rows to print take the spacer's rows instead of scrolling the screen. The spacer never reaches scrollback, because printed history replaces it in place. The held height is capped by the frame's own budget, so L1 holds.

A new session's blank rows are above its heading rather than in the frame. They reach scrollback once, between the shell's output and the session, which is the one cost of starting at the bottom.

The live region has no reserved height. It grows a row per streamed row up to `budget.live`, then keeps its newest rows. `tailLines` chooses them in wrapped rows, measured the way Ink wraps `Text`, so the window the lines are chosen for is the one they are drawn in.

#### A streaming answer prints as it completes

The answer does not wait for its commit to reach the transcript. `Printed` in `packages/app/src/printed.ts` prints settled Markdown blocks of the streaming answer to `Static`. A paragraph settles after a blank line; a top-level code fence settles after its closing line ends. Lists and tables wait for a successor block or the message commit. A table uses the available prose width for aligned columns, with wrapped cells and quiet ASCII separators; narrow, nested, or irregular tables use labeled cells. A partially admitted source row keeps its first presentation through resize so no cells are skipped or printed twice. A text or reasoning block prints whole once a later block starts. Nothing past the first tool call prints, because the call commits through its own event. The live region keeps the unfinished block within its row budget. When the message commits, `reconcile` drops from it what already printed, so each line appears once. A block the commit changed prints again whole, since duplication stays readable.

This removes two failures of drawing the answer in the live region. An answer longer than the window was shown clipped until it committed. And every row the window gained or lost moved the composer on a full screen. Printed a line at a time, the text scrolls into the terminal's history the way the terminal would scroll it, while the frame holds one line and does not change height. `placement.spec.tsx` streams twelve wrapped paragraphs this way and asserts the input row never moves.

Printing is display, not record. The session log commits the message as before, and a resume draws it whole. Scrollback cannot be unwritten, so lines printed from an attempt that never commits stay; the transcript follows them with a notice that they were discarded.

#### What remains in the window

Only the newest section is cut; an older one is shown whole or not at all. A cut section keeps its opening blank, drawn outside the clip, and gets back the connector or reply marker the cut removed. Answer prose fills the window exactly, with its oldest line clipped from the top the way a terminal scrolls. Stopping at whole lines would leave the window a row or two short whenever the next line wraps. Tool output keeps whole lines. The window claims its rows from the shared budget only while it has rows to draw: a turn that has not produced output yet leaves those rows to the panels.

Growth scrolls history up by the rows it adds, and nothing the frame gives up moves the input. Existing history is neither traversed nor reprinted during typing or streaming, and the frame stays under `rows - 1` (L1) because every region claims from one budget.

#### Narrowing and growing taller repaint

Ink erases the previous frame by counting its lines. A terminal that reflows on resize has already re-wrapped each full-width row of that frame — the composer's rule — onto two rows, so the count falls short and the rows it misses stay on screen above the new frame. A terminal that grows taller adds its rows under the frame, unless it pulls history down from scrollback, and leaves the composer off the bottom row. Ink clears the terminal and replays history only when a frame overflows the viewport, so after the terminal narrows or grows taller, a child is opened or left, or a sheet closes, `App` draws a spacer of viewport height above the controls. That frame is cleared and replayed; the next one, overflowing no longer, is cleared and replayed again at the normal size. Ink throttles its writes and drops a commit replaced within one throttle window, so `useRepaint` holds the spacer until `waitUntilRenderFlush` confirms Ink wrote it; dropped in the same flush, the two frames would coalesce and nothing would be cleared. A repaint therefore costs two replays of the transcript, which is what a resize cost when the frame filled the screen.

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

The same renderer draws a changed row and then erases to the end of the line. A row that fills the terminal leaves the cursor on its last cell until the next character wraps it, and a terminal that does not defer that wrap, Warp among them, erases the cell from there: the right edge of every full-width row lost its last character, which the composer's key hint and the goal at the header's end always reach. `clearFirst` in `frameOutput` moves each such erase before the row's text, in the inline and fullscreen screens alike, so the row keeps its last cell on every terminal; the region overwrite already clears each row as it starts. `packages/app/tests/output.spec.tsx` runs the app through `frameOutput` and asserts no write erases a row after drawing on it.

Fullscreen shares the batched output queue, `NO_COLOR` filtering, and `clearFirst`, but skips the inline overwrite, bottom-anchor, and newline transformations. After Ink enters the alternate buffer, the wrapper clears and homes that buffer without moving the saved shell cursor, and turns on mouse reporting and turns off autowrap, restoring both just before Ink leaves the buffer. With autowrap off, a row the terminal draws wider than Ink measured, as a character whose width they disagree on can make it, is clipped at the right edge instead of wrapping onto the row below and scrolling the fixed screen. It removes `CSI 3J` so resize and cleanup do not erase primary scrollback. Ink's cleanup leaves the alternate screen through the same runner release path as normal quit and fatal failure.

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
| L1 holds in inline mode | render each state at 80×24 and 40×10, assert dynamic height `<= rows - 1` |
| Fullscreen renders a bounded viewport and restores the shell | `packages/app/tests/fullscreen.spec.tsx` checks output, paging, append/commit anchors, resize, large history, tiny terminals, sheets, inspection, and shell restoration; the PTY `fullscreen` scenario exercises fresh and resumed built profiles, and `fatal-exception` checks alternate-screen release |
| append cost is O(1) | `packages/ui/tests/scale.spec.tsx` (raw stdout capture, not `frames`) |
| no full-clear in a normal turn | assert `ansiEscapes.clearTerminal` never appears in captured stdout for a scripted turn |
| status never wraps | render at 40, 80, 200 columns, assert one line |
| the status line cuts no bounded field | `packages/ui/tests/status-line.test.ts` checks every field's readings and ranks, the order `fitStatus` gives way in, the model's floor, and the filler; `line.spec.tsx` narrows full rows at 120, 80, and 60 columns and below, filling, full, and with a compaction mark (`expected/status.*.txt`), and asserts each field goes whole or to a complete shorter reading while the path keeps its tail |
| billed tokens follow the provider | `packages/app/tests/context.spec.ts` reports no totals before a request, then input and output without a cache field, then a cache hit once the provider reports cache reads; `status.spec.tsx` draws them; the PTY `fresh` scenario asserts the recorded turn's `ctx ~N% (…/128k)` and `in 5.9k  out 115  cache hit 48%` |
| motion shares one beat and draws only what changes | `packages/ui/tests/live.spec.tsx` runs the header and two running actions on one timer, draws at most once a beat and fewer than 70% of the frames their separate timers drew, draws a header without motion once a second, and keeps no timer for a header with nothing running; `live.spec.tsx` also blinks a running action's marker between the orange `●` and a space; `styles.spec.tsx` asserts the rule under the running header stays one dim run, a healthy status line with no colour at all, the cache-hit warnings, and the context ramp's steps; `palette.test.ts` pins the ramp's thresholds with and without a compaction mark |
| L2: the input rests on the bottom row | `packages/ui/tests/placement.spec.tsx` checks actual terminal rows across streaming, commits, idle transitions, menus, feedback, a full screen, and resizing; `live.spec.tsx` checks the frame grows a row per streamed row and no further than the live budget; `packages/app/tests/output.spec.tsx` runs a turn with its panels through `frameOutput` and asserts the input is on the bottom row from the first frame and after each resize replay; `output.test.ts` checks `anchor` byte for byte; the PTY `rendering` scenario asserts the built profile opens on and returns to the bottom rows |
| the frame never rises | `packages/ui/tests/live.spec.tsx` clears live output without printing and asserts the frame keeps its height, blank above the controls, until committed rows take it; `placement.spec.tsx` closes menus, notices, queued input, and quit feedback without the input row moving, then prints history into the held rows, and closes a sheet by each of its keys and asserts the history is replayed back against the controls, the screen matching the one before it opened, every line once in scrollback; `output.spec.tsx` clears a notice and shrinks the queued input mid-turn |
| a running turn shows what it is doing | `packages/app/tests/live.spec.ts` streams reasoning, a call, and an answer through `LiveBlocks` and asserts all three appear, in order, with no arguments rendered |
| a windowed section stays named | `packages/ui/tests/present.test.ts` cuts a long block below its head and asserts `tailLines` restores the connector onto what is left, shows an older section whole or not at all, and counts wrapped rows; `live.spec.tsx` asserts a wrapped answer keeps its blank and holds its height once full |
| a printed line never erases the controls | `packages/app/tests/output.test.ts` checks the rewrite byte for byte and passes through writes it does not recognize; `output.spec.tsx` streams an answer on a full screen, replays every render a row at a time in a terminal emulator, and asserts that written directly at least three rows go blank part-way, that through `frameOutput` at most one does, and that both end with identical scrollback |
| a streaming answer prints once | `packages/app/tests/printed.spec.ts` prints settled blocks, keeps paragraph breaks across delta boundaries, compares streamed Markdown with replay, and reconciles the commit; `session.spec.ts` streams through the controller, resumes to one message, and marks an abandoned attempt's lines discarded; `placement.spec.tsx` holds the input row through twelve printed paragraphs on a full screen |
| the composer cannot outgrow its budget | `packages/ui/tests/line.spec.tsx` renders a 4,000-character paste at several widths and asserts the row count against `COMPOSER_BUDGET`, with the caret on the last row |
| the window says what it hides | `packages/ui/tests/line.spec.tsx` marks hidden rows with `^` and `v`, counts them right-aligned in the hint's slot only on a row without the hint, leaves out a count wider than the hint and every count without a slot, and holds the window still while the caret moves inside it; `placement.spec.tsx` pins the `v` in `composer-wrapped-home.txt` |
| a newline key works in every terminal | `packages/ui/tests/shell.spec.tsx` breaks lines on a lone line feed, Escape and a carriage return, CSI-u Shift-Enter, Ctrl-J, and Alt-Enter without submitting, in the composer and the sign-in and question inputs, and still submits text and a line feed read together; the PTY `fresh` scenario types a line, presses Ctrl-J, types another, and asserts two draft rows in a terminal emulator and a log without the draft; `packages/app/tests/terminal-bindings.test.ts` adds Shift+Enter as Escape and a carriage return to each editable terminal's file, keeping comments, finding it again, and reporting another binding, and `terminal-setup.spec.ts` and the PTY `terminal-setup` scenario write it in a private home only after confirmation, with a backup |
| Up and Down move by rows before history | `packages/ui/tests/editor.test.ts` moves between logical and wrapped rows, keeps a goal column in cells, never splits a wide character, and reports the first and last rows; `history.test.ts` places a recalled entry's caret; `shell.spec.tsx` walks a draft's rows, recalls from the first row, restores the draft, opens an older multi-row entry at its start, and still reaches the goal past the oldest entry; the PTY `resume` scenario recalls and restores a draft |
| a draft reflows only with its width | `packages/ui/tests/editor.test.ts` wraps drafts at every width from 4 to 29 and asserts no row exceeds it or opens with a hanging space, that no caret position moves a word, and that tabs expand; `line.spec.tsx` moves the caret over a wrapped draft beside a hint and asserts identical rows; `placement.spec.tsx` pastes a wrapped draft with a long path and a tab, resizes across 40 columns, and asserts every row stays between the rule and the base rule at the prompt column and no tab reaches the terminal |
| the caret covers a cell and never shifts text | `packages/ui/tests/caret.test.ts` covers a grapheme whole, a tab's first column, or a blank cell; `editor.test.ts` places Home/End at a drawn row's edge, then the line's, and maps a clicked cell to the offset before its grapheme; `styles.spec.tsx` asserts the caret's reverse video in a coloured render; `output.spec.tsx` keeps it under `NO_COLOR`; `fullscreen.spec.tsx` clicks the caret into place; the PTY `rendering` scenario presses Home and End twice in a wrapped draft |
| a fullscreen selection copies what it shows | `packages/ui/tests/selection.test.ts` orders points, widens a cut through a wide character, joins a path or hyphenated word, and marks only the covered part of each run; `packages/app/tests/fullscreen.spec.tsx` drags across rows and asserts the reversed cells and the copied text, takes a word and a row on double and triple clicks, scrolls a drag held at the top edge, and shows `Copy failed`; `clipboard.test.ts` picks each platform's tool and falls back to OSC 52; the PTY `fullscreen` scenario double-clicks a word and reads its OSC 52 copy |
| the chrome fits every supported width | `packages/ui/tests/line.spec.tsx` renders `Chrome` from 1 to 200 columns bare, running, and ended, and asserts no row exceeds the width, both rules are bare and exactly the width, the header puts the turn at the draft column and the goal at the right edge, narrowing in the header's order (the goal's note, the turn's counts and rate, `Ctrl+O`, the turn's phase and elapsed time, `● 3/256`, `●`) before the turn's word, the height never changes with width alone, the rows yield gap, base rule, rule, status, then header on short terminals, and the hint drops at its threshold; `fitStanding` and `headerLayout` are checked cell for cell; `styles.spec.tsx` asserts in truecolour that the rules are dim and the draft keeps the terminal's foreground; `output.test.ts` checks `scrolling` byte for byte |
| the rule and the frame match the terminal | `packages/app/tests/frame.test.ts` resolves every environment that cannot draw box characters; `line.spec.tsx` draws the rule in ASCII; `welcome.spec.tsx` draws both card styles |
| the header's label holds for the turn | `packages/ui/tests/live.spec.tsx` keeps one word through thinking, writing, a commit, and a running tool; streams reasoning whole under a running step past the live window without outgrowing the terminal, then folds it to its preview once a later row follows; checks with a fake clock that the spinner and elapsed time advance and the interval is disposed when the turn ends; checks that with motion off only the seconds change; and runs compaction's laminating spinner through its loop, over `/compact` and over a turn compacting itself. `activity.test.ts` pins both spinners' frames as dot pictures, and `styles.spec.tsx` asserts compaction's blue in truecolour |
| streaming reasoning folds once a later row follows | `packages/ui/tests/streaming-reasoning.test.ts` draws the newest reasoning whole in the fullscreen viewport, then its `resultLines` preview once the answer starts; scrolls a long thought by rows; and asserts the paragraph-bounded tail draws what the whole thought would, mid-fence included. `placement.spec.tsx` streams a thought taller than the live window and folds it without moving the input |
| reasoning is previewed in scrollback | `packages/ui/tests/present.test.ts` keeps the first `resultLines` wrapped rows and counts the rest, ends the preview on text rather than a blank row, draws a block whole when the count would hide one row, and reduces to a size at `0` |
| a wrapped row never opens with a space | `packages/ui/tests/present.test.ts` asserts `softBreaks` breaks at the space that would open a row while keeping the text length, and leaves fitting or tab-expanded text unchanged |
| the header holds the finished turn | `packages/ui/tests/live.spec.tsx` ends a clocked turn and asserts the summary takes the header's label at the same frame height, survives a notice, reports failures, clears when the next turn starts, and summarizes a replayed session's last turn untimed; `activity.test.ts` covers the counts, the outcomes, and the rate, reported only for a sample of at least a second and 64 tokens |
| a call and its result are one block | `packages/ui/tests/fold.test.ts` merges results into their calls, releases out-of-order results in call order, and settles held calls; `styles.spec.tsx` and `actions.spec.tsx` assert the running, done, and failed markers, `Tool(argument)` heads with a `⎿` connector, and no call ids; `styles.spec.tsx` and `present.test.ts` assert a step's tree is dim and uncoloured, branch, stem, corner, and connector alike, with a failed call's head red |
| an edit shows what changed | `packages/ui/tests/cards.test.ts` draws only changed lines, numbers each side past the other's insertions, separates joined changes and hunks with `⋯`, heads files only when there are several, draws no numbers without `oldStart`, and marks the words of an edited line but not a replaced one; `present.test.ts` puts `+N −M` on the head at `resultLines: 0`, numbers lines in the gutter, counts only changed lines against the preview and never ends it on a gap, reverses changed words, highlights one side's run at a time, and keeps a failure red; `styles.spec.tsx` pins the size, the gutter, and the reversed words in truecolour; `packages/app/tests/syntax.spec.ts` highlights a preloaded language before the first frame, leaves another plain until its grammar loads, and draws nothing once closed; `packages/fs/tool-fs/tests/diff.spec.ts` records and validates `oldStart` and `newStart` |
| diff text is display text | `packages/ui/tests/cards.test.ts` expands a tab, strips ANSI codes, drops a CRLF ending, and escapes a stray carriage return and a bell, and checks the changed words' offsets against the drawn text; `tool-output.spec.tsx` renders an edit and a command's change whose file holds them, with no raw control in the frame |
| a command shows what it changed | `packages/ui/tests/cards.test.ts` keeps a terminal result without changes equal to its earlier card, words each kind of change, escapes controls in paths, and bounds the section with `+N more files`; `present.test.ts` puts `+N −M` before `exit 1`, previews output and changes each against its own bound, keeps the diff's tones and the path's blue under a failure and in a step's head, collapses to the size and the file count at `resultLines: 0`, keeps the path line of a file without hunks with its size, counts files left out and draws the caveats dim; `tool-output.spec.tsx` pins the section at 40 and 80 columns and in truecolour |
| every indicator takes its colour from the palette | `packages/ui/tests/styles.spec.tsx` forces truecolour and pins a running, a finished, and a failed marker to their `PALETTE` tones; `present.test.ts` asserts `styleOf` returns the palette's done, failed, and asking tones, so no component can reintroduce a named colour |
| quit feedback sits above the input | `packages/ui/tests/live.spec.tsx` checks one row of feedback above the input alongside command notices |
| no foreign terminal writes | scripted turn asserts nothing reaches stderr while mounted |

The third is the direct regression test for §1 and the one most worth adding first. The last two are the regression tests for §8.1 and §8.3, which are what the user actually perceives as quality.
