/**
 * Terminal geometry: region budgets and the render vocabulary.
 *
 * Every rendered region asks this module how much room it may take. The
 * arithmetic is centralized because the constraint it enforces is not local:
 * Ink clears the screen and replays the whole transcript once the dynamic
 * region reaches viewport height, so a single region overrunning its share
 * degrades the entire surface. See `apps/tui/DESIGN-LAYOUT.md` for the measurements
 * behind each rule.
 *
 * @module @dsh-tui/ui/layout
 */

/**
 * Column budgets shared by every row kind.
 *
 * The values are columns, not characters: a caller measuring text must use a
 * display-width function, since one code point can occupy two cells.
 */
export const COLUMN = {
  /** Marker column, then a space: `> ` for a user row, `  ` for an answer. */
  rail: 2,
  /** Verb field, including its trailing gap, so arguments start at `rail + verb`. */
  verb: 7,
  /** Reasoning and tool output indent, aligned under a verb's argument. */
  output: 9,
} as const

/**
 * Widest comfortable prose column.
 *
 * Long monospace lines are hard to scan back to the start of, and terminals get
 * arbitrarily wide. Tool output is exempt: wrapping a log or a diff to a narrow
 * measure destroys the alignment that makes it readable.
 */
export const PROSE_MEASURE = 88

/**
 * Rows the dynamic region always owes, whatever else it draws.
 *
 * `Chrome` spends them on a blank row, the composer's frame above and below,
 * the composer's first line, and the status line under the frame. A draft
 * taller than one line takes further rows from the regions above it, which is
 * why the composer is counted at its floor rather than its maximum.
 *
 * Understating this understates nothing else: the live region may grow to
 * `budget.live` rows during a running turn, so a chrome height counted short
 * is an L1 violation for as long as a turn's output fills its window.
 */
export const CHROME_ROWS = 5

/**
 * Narrowest terminal the composer draws its frame in.
 *
 * The frame costs four columns — two for the border and two for the padding —
 * and at §4's supported minimum of 40 that is a tenth of the line the user is
 * typing on. Below it the frame is dropped rather than shrunk: the rail and
 * the status line below already say where input lands, and a draft with room
 * to read is worth more than a box around it.
 */
export const FRAME_MIN_COLUMNS = 40

/**
 * Narrowest terminal the composer's right slot draws a hint in.
 *
 * The slot is contextual help, so it is the first thing to go: a hint that has
 * to be truncated to fit has stopped being help, and the keys it names still
 * work unnamed. Above this width the hint keeps its full text and the draft
 * wraps around it, which is why it never has to shrink.
 */
export const HINT_MIN_COLUMNS = 60

/** Largest live-region height, before the terminal's own height is considered. */
export const LIVE_BUDGET = 10

/**
 * Largest composer height, before the terminal's own height is considered.
 *
 * The composer's first row is charged to {@link CHROME_ROWS}; the rest are
 * taken from the regions above it as the draft grows, so this is the most a
 * draft can cost them. Unbounded, a pasted paragraph would take the whole
 * dynamic region and put Ink on its screen-clearing path with the cursor still
 * in the box.
 */
export const COMPOSER_BUDGET = 5

/**
 * Largest notice height, before the terminal's own height is considered.
 *
 * The notice region is for short feedback — "model set for the next turn" — not
 * for catalogs. A command whose full output matters returns it, so it commits
 * to the transcript where the terminal can scroll it; this bound is what stops
 * anything else from pushing the status line and composer off the screen.
 */
export const NOTICE_BUDGET = 6

/**
 * Frame the composer draws around the draft.
 *
 * `round` is box-drawing characters; `classic` is ASCII. Which one a terminal
 * can render is a property of that terminal, resolved once at the application
 * boundary and passed in, so this layer needs no environment to lay out.
 */
export type FrameStyle = 'round' | 'classic'

/** Terminal size, as reported by `useWindowSize()`. */
export interface WindowSize {
  /** Horizontal character cells. */
  readonly columns: number
  /** Vertical character cells. */
  readonly rows: number
}

/** How much room each region may take at one terminal size. */
export interface Budget {
  /**
   * Rows the dynamic region may occupy in total.
   *
   * One less than the viewport: at viewport height Ink switches to clearing the
   * terminal and replaying every committed row on each frame.
   */
  readonly dynamic: number
  /** Visible composer structure, including its first input row. */
  readonly chrome: ChromeLayout
  /** Rows the live region may draw; it is sized to its content below that. */
  readonly live: number
  /** Rows a draft may occupy, its first — charged to the chrome — included. */
  readonly composer: number
  /** Items an overlay may list before it must show a `+N more` footer. */
  readonly items: number
  /** Lines a notice may draw before it must show a `+N more` footer. */
  readonly notice: number
  /** Columns prose may wrap at. */
  readonly measure: number
  /** Columns tool output may use. */
  readonly output: number
}

/** Composer structure that fits before any additional draft rows are reserved. */
export interface ChromeLayout {
  readonly frame: boolean
  readonly status: boolean
  readonly gap: boolean
  readonly rows: number
}

/**
 * Yield decorative rows before hiding input on a short or narrow terminal.
 * @param columns - terminal width.
 * @param available - rows available to the footer, excluding Ink's cursor row.
 * @returns visible structure and its exact height with a one-row draft.
 */
export function chromeFor(columns: number, available = CHROME_ROWS): ChromeLayout {
  const frame = columns >= FRAME_MIN_COLUMNS && available >= 3
  const input = frame ? 3 : 1
  const status = available > input
  const gap = available > input + 1
  return { frame, status, gap, rows: input + Number(status) + Number(gap) }
}

/**
 * Derive every region budget from the terminal size.
 *
 * Nothing here is a tuned constant that happens to fit an 80x24 window: a
 * limit that ignores the viewport overflows a split pane, and overflow is the
 * one failure that clears the user's screen.
 *
 * @param size - current terminal size.
 * @param options.header - whether an overlay draws a title line.
 * @returns budgets for every region, each at least one row or column.
 */
export function budgetFor(size: WindowSize, options: { readonly header?: boolean } = {}): Budget {
  const dynamic = Math.max(1, size.rows - 1)
  const chrome = chromeFor(size.columns, dynamic)
  const live = Math.max(1, Math.min(LIVE_BUDGET, dynamic - chrome.rows))
  const composer = Math.max(1, Math.min(COMPOSER_BUDGET, dynamic - chrome.rows))
  const items = Math.max(1, dynamic - chrome.rows - (options.header === true ? 1 : 0))
  const notice = Math.max(1, Math.min(NOTICE_BUDGET, dynamic - chrome.rows))
  const measure = Math.max(1, Math.min(PROSE_MEASURE, size.columns - COLUMN.rail))
  return { dynamic, chrome, live, composer, items, notice, measure, output: Math.max(1, size.columns - COLUMN.output) }
}

/**
 * Split a list into the part an overlay shows and the count it hides.
 *
 * The footer occupies a row, so a list one item too long shows one item fewer
 * than the limit. Callers that skip this arithmetic overflow by exactly one row,
 * which is the whole budget on a short window.
 *
 * @param items - every candidate, in display order.
 * @param limit - rows available to the list, footer included.
 * @returns the visible slice and how many items it omits.
 */
export function windowOf<T>(items: readonly T[], limit: number): {
  readonly shown: readonly T[]
  readonly hidden: number
} {
  if (items.length <= limit) return { shown: items, hidden: 0 }
  const shown = items.slice(0, Math.max(0, limit - 1))
  return { shown, hidden: items.length - shown.length }
}

/**
 * Window a selectable list without hiding its selection; reserve an omission row
 * when at least two rows fit. A one-row window prioritizes the selected item.
 * @param items - candidates in display order.
 * @param selected - selected index in the complete list.
 * @param limit - available rows, including the omission row.
 * @param maximum - configured maximum number of candidates.
 * @returns visible candidates, their relative selection, and omitted count.
 */
export function selectionWindow<T>(items: readonly T[], selected: number, limit: number, maximum = limit): {
  readonly shown: readonly T[]
  readonly selected: number
  readonly hidden: number
} {
  const overflow = items.length > Math.min(limit, maximum)
  const capacity = Math.max(1, Math.min(maximum, limit - (overflow && limit > 1 ? 1 : 0)))
  const start = Math.max(0, selected - capacity + 1)
  const shown = items.slice(start, start + capacity)
  return { shown, selected: selected - start, hidden: items.length - shown.length }
}

/**
 * Keep the last `budget` lines of an unbounded stream.
 *
 * Clipped lines are not lost: the turn commits them to the transcript, where
 * the terminal's own scrollback holds them.
 *
 * @param lines - every line produced so far.
 * @param budget - rows the live region may draw.
 * @returns the trailing lines that fit.
 */
export const tailOf = <T>(lines: readonly T[], budget: number): readonly T[] =>
  budget <= 0 ? [] : lines.slice(-budget)

/**
 * Marker characters.
 *
 * Text is ASCII, because a terminal and `string-width` can disagree above 0x7f
 * and Ink measures with `string-width`. Markers are the deliberate exception:
 * each sits alone in a fixed-width rail, so a terminal that draws one wider
 * than measured shifts that row and nothing else, rather than accumulating
 * error across a line. `isRenderable` still guards everything that is not a
 * marker.
 */
export const MARKER = {
  /** Opens a turn in the transcript: the user's words that started it. */
  turn: '\u25cf',
  /** First line of an assistant reply. */
  reply: '<',
  /**
   * A slash command the user ran.
   *
   * The rail carries the slash, so the row breaks the left column the way a
   * command breaks the conversation, and the name reads without it. Colour
   * alone would not do this: dim prose at the text column is indistinguishable
   * from an answer under NO_COLOR and to a screen reader.
   */
  command: '/',
  /** The composer prompt. Only the live input carries it, never history. */
  prompt: '>',
  /** A selected list row: a pointer, because the eye follows it as it moves. */
  selected: '\u25b8',
  /** A value already in force, as opposed to the one under the cursor. */
  current: '*',
  /**
   * Opens an action: pulsing while it runs, green once it succeeded, red once
   * it failed. The shape is the same in every state, so `NO_COLOR` still reads
   * each action as one block.
   */
  action: '\u25cf',
  /** An unmarked row: assistant prose, and unselected list rows. */
  none: ' ',
} as const

/**
 * Verbs naming what the agent did.
 *
 * Named rather than pictured: a verb reads at a glance, survives every font and
 * locale, and stays legible pasted into a bug report. Reasoning and approvals
 * use the same grammar rather than inventing their own marks.
 */
export const VERB = {
  think: 'think',
  run: 'run',
  read: 'read',
  edit: 'edit',
  find: 'find',
  fetch: 'fetch',
  ask: 'ask',
  note: 'note',
  error: 'error',
  done: 'done',
} as const

/** A verb this vocabulary names. */
export type Verb = typeof VERB[keyof typeof VERB]

/**
 * What a finished action's verb becomes.
 *
 * The block keeps its place and its words, and the verb changes tense: a
 * running `run` reads as work in progress, a finished `ran` as done, without a
 * second row saying so. Every form fits the verb column with its gap.
 */
export const PAST: Readonly<Record<Verb, string>> = {
  think: 'think',
  run: 'ran',
  read: 'read',
  edit: 'edited',
  find: 'found',
  fetch: 'got',
  ask: 'asked',
  note: 'note',
  error: 'error',
  done: 'done',
}

/** True when every character is ASCII and can therefore be measured reliably. */
export const isRenderable = (text: string): boolean => {
  for (const character of text) {
    if (character.codePointAt(0)! > 0x7f) return false
  }
  return true
}
