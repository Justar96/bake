/**
 * Terminal geometry: region budgets and the render vocabulary.
 *
 * Every rendered region asks this module how much room it may take. The
 * arithmetic is centralized because the constraint it enforces is not local:
 * Ink clears the screen and replays the whole transcript once the dynamic
 * region reaches viewport height, so a single region overrunning its share
 * degrades the entire surface. See `tui/DESIGN-LAYOUT.md` for the measurements
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
 * `Chrome` spends them on the status line and the composer's first line. A
 * draft taller than one line takes further rows from the regions above it,
 * which is why the composer is counted at its floor rather than its maximum.
 *
 * Understating this understates nothing else: the live region reserves
 * `budget.live` rows for the whole of a running turn, so a chrome height
 * counted short is an L1 violation held for the length of every turn.
 */
export const CHROME_ROWS = 2

/** Largest live-region height, before the terminal's own height is considered. */
export const LIVE_BUDGET = 10

/**
 * Largest notice height, before the terminal's own height is considered.
 *
 * The notice region is for short feedback — "model set for the next turn" — not
 * for catalogs. A command whose full output matters returns it, so it commits
 * to the transcript where the terminal can scroll it; this bound is what stops
 * anything else from pushing the status line and composer off the screen.
 */
export const NOTICE_BUDGET = 6

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
  /** Rows the live region may draw, and the height it is padded to while a turn runs. */
  readonly live: number
  /** Items an overlay may list before it must show a `+N more` footer. */
  readonly items: number
  /** Lines a notice may draw before it must show a `+N more` footer. */
  readonly notice: number
  /** Columns prose may wrap at. */
  readonly measure: number
  /** Columns tool output may use. */
  readonly output: number
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
  const live = Math.max(1, Math.min(LIVE_BUDGET, dynamic - CHROME_ROWS))
  const items = Math.max(1, dynamic - CHROME_ROWS - (options.header === true ? 1 : 0))
  const notice = Math.max(1, Math.min(NOTICE_BUDGET, dynamic - CHROME_ROWS))
  const measure = Math.max(1, Math.min(PROSE_MEASURE, size.columns - COLUMN.rail))
  return { dynamic, live, items, notice, measure, output: Math.max(1, size.columns - COLUMN.output) }
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
} as const

/** A verb this vocabulary names. */
export type Verb = typeof VERB[keyof typeof VERB]

/** True when every character is ASCII and can therefore be measured reliably. */
export const isRenderable = (text: string): boolean => {
  for (const character of text) {
    if (character.codePointAt(0)! > 0x7f) return false
  }
  return true
}
