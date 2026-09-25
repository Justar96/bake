/**
 * Terminal geometry. Region budgets and the render vocabulary.
 *
 * Every rendered region asks this module how much room it may take. The
 * arithmetic is centralized because the constraint is not local. Ink clears
 * the screen and replays the whole transcript once the dynamic region reaches
 * viewport height, so one region overrunning its share degrades the entire
 * surface. See `apps/tui/DESIGN-LAYOUT.md` for the measurements behind each rule.
 *
 * @module @dsh-tui/ui/layout
 */

/**
 * Column budgets shared by every row kind.
 *
 * The values are columns, not characters. A caller measuring text must use a
 * display-width function, because one code point can occupy two cells.
 */
export const COLUMN = {
  /** Marker column, then a space. `> ` for a user row, `  ` for an answer. */
  rail: 2,
  /** Verb field, including its trailing gap, so arguments start at `rail + verb`. */
  verb: 7,
  /** Reasoning and tool output indent, aligned under a verb's argument. */
  output: 9,
} as const

/**
 * Rows the dynamic region always reserves for chrome, whatever else it draws.
 *
 * `Chrome` spends them, top to bottom, on a blank row, the header, the rule
 * over the draft, the composer's first line, the base rule, and the status
 * line. Extra draft lines are taken from the regions above, so the composer
 * is counted at its one-row floor, not at {@link COMPOSER_BUDGET}.
 *
 * Do not undercount this. During a running turn the live region may grow to
 * `budget.live` rows, and a short chrome height is then an L1 violation for
 * as long as the turn's output fills its window.
 */
export const CHROME_ROWS = 6

/**
 * Narrowest terminal the welcome card draws its border in.
 *
 * The border costs four columns. Two are the border and two are the padding.
 * At the supported minimum of 40 columns, that is a tenth of the line. Below
 * this width the border is dropped, not shrunk.
 */
export const FRAME_MIN_COLUMNS = 40

/**
 * Narrowest terminal the composer's right slot draws a hint in.
 *
 * The slot is contextual help, so it yields first. A hint that has to be
 * truncated has stopped being help, and the keys it names still work unnamed.
 * Above this width the hint keeps its full text and the draft wraps around
 * it, so the hint never shrinks.
 */
export const HINT_MIN_COLUMNS = 60

/**
 * Live-region height on an ordinary terminal. The rows it may take whenever
 * the terminal has them, however short it is otherwise.
 */
export const LIVE_BUDGET = 10

/**
 * Share of the dynamic region the live region may grow to on a tall terminal.
 *
 * A fixed ten rows folded a running step's output on a 60-row screen whose
 * upper half stood empty. Scaling it with height lets a tall terminal show
 * more of what is running, while the rest stays for the history it
 * continues, which is what the reader scrolls back through when it ends.
 */
export const LIVE_SHARE = 0.4

/**
 * Largest composer height, before the terminal's own height is considered.
 *
 * The composer's first row is charged to {@link CHROME_ROWS}. The rest are
 * taken from the regions above it as the draft grows, so this is the most a
 * draft can cost them. Without the cap, a pasted paragraph would take the
 * whole dynamic region and put Ink on its screen-clearing path with the
 * cursor still in the box.
 */
export const COMPOSER_BUDGET = 5

/**
 * Largest notice height, before the terminal's own height is considered.
 *
 * The notice region is for short feedback, such as "model set for the next
 * turn", not for catalogs. A command whose full output matters returns that
 * output, so it commits to the transcript where the terminal can scroll it.
 * This bound stops anything else from pushing the status line and composer
 * off the screen.
 */
export const NOTICE_BUDGET = 6

/**
 * Line glyphs the welcome card's border and the composer's rule draw.
 *
 * `round` is box-drawing characters; `classic` is ASCII. Which one a terminal
 * can render is a property of that terminal, resolved once at the application
 * boundary and passed in, so this layer needs no environment to lay out.
 */
export type FrameStyle = 'round' | 'classic'

/**
 * Line glyph shared by both composer rules.
 *
 * Box-drawing characters are East Asian Ambiguous. A full-width run is the
 * only place that width error accumulates across a row, so `classic` uses ASCII.
 */
export const RULE: Readonly<Record<FrameStyle, { readonly line: string }>> = {
  round: { line: '\u2500' },
  classic: { line: '-' },
}

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
   * One less than the viewport. At viewport height Ink clears the terminal
   * and replays every committed row on each frame.
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
  /** Current terminal width, including the rail and output columns. */
  readonly columns: number
  /** Columns prose may wrap at. */
  readonly measure: number
  /** Columns tool output may use. */
  readonly output: number
}

/**
 * Composer structure that fits before any extra draft rows are reserved.
 *
 * Top to bottom. A blank, the header, the rule over the draft, the draft's
 * first row, the base rule, and the status line. The base rule keeps the
 * status line off the draft, so no blank padding row sits between them.
 */
export interface ChromeLayout {
  /** Blank row that opens the stack under the conversation. */
  readonly gap: boolean
  /** Row above the upper rule. Carries the turn's state and the goal. */
  readonly header: boolean
  /** Rule directly above the draft. */
  readonly rule: boolean
  /** Rule directly under the draft. */
  readonly base: boolean
  /** Status line under the base rule. */
  readonly status: boolean
  /** Height of the visible structure, including a one-row draft. */
  readonly rows: number
}

/**
 * Drop chrome rows before hiding the input on a short terminal.
 *
 * Yield order, cheapest first. The gap, the base rule, the upper rule, the
 * status line, then the header. The header is last because it is the row
 * that says whether a turn is running.
 *
 * Every piece is one row at any width, so the chrome's height depends on the
 * terminal's rows alone and a width change never moves the input vertically.
 *
 * @param _columns - terminal width, which no row depends on.
 * @param available - rows available to the footer, excluding Ink's cursor row.
 * @returns visible structure and its exact height with a one-row draft.
 */
export function chromeFor(_columns: number, available = CHROME_ROWS): ChromeLayout {
  const header = available >= 2
  const status = available >= 3
  const rule = available >= 4
  const base = available >= 5
  const gap = available >= 6
  return { gap, header, rule, base, status, rows: 1 + Number(header) + Number(rule) + Number(base) + Number(status) + Number(gap) }
}

/**
 * Derive every region budget from the terminal size.
 *
 * These are not constants tuned for an 80×24 window. A limit that ignores
 * the viewport overflows a split pane, and overflow is the failure that
 * clears the user's screen.
 *
 * @param size - current terminal size.
 * @param options.header - whether an overlay draws a title line.
 * @returns budgets for every region, each at least one row or column.
 */
export function budgetFor(size: WindowSize, options: { readonly header?: boolean } = {}): Budget {
  const dynamic = Math.max(1, size.rows - 1)
  const chrome = chromeFor(size.columns, dynamic)
  const live = Math.max(1, Math.min(Math.max(LIVE_BUDGET, Math.floor(dynamic * LIVE_SHARE)), dynamic - chrome.rows))
  const composer = Math.max(1, Math.min(COMPOSER_BUDGET, dynamic - chrome.rows))
  const items = Math.max(1, dynamic - chrome.rows - (options.header === true ? 1 : 0))
  const notice = Math.max(1, Math.min(NOTICE_BUDGET, dynamic - chrome.rows))
  const columns = Math.max(1, size.columns)
  const measure = Math.max(1, columns - COLUMN.rail)
  return { dynamic, chrome, live, composer, items, notice, columns, measure, output: Math.max(1, columns - COLUMN.output) }
}

/**
 * Split a list into the part an overlay shows and the count it hides.
 *
 * The footer occupies a row, so a list one item too long shows one item fewer
 * than the limit. Callers that skip this arithmetic overflow by exactly one
 * row, which is the whole budget on a short window.
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
 * Clipped lines are not lost. The turn commits them to the transcript, where
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
 * Text is ASCII, because a terminal and `string-width` can disagree above
 * `0x7f`, and Ink measures with `string-width`. Markers are the exception.
 * Each sits alone in a fixed-width rail, so a terminal that draws one wider
 * than measured shifts that row and nothing else, instead of accumulating
 * error across a line. `isRenderable` still guards everything that is not a
 * marker.
 */
export const MARKER = {
  /** Opens a turn in the transcript. The user's words that started it. */
  turn: '\u25cf',
  /** The composer prompt. Only the live input carries it, never history. */
  prompt: '>',
  /** A selected list row. A pointer, so the selection stays visible as it moves. */
  selected: '\u25b8',
  /** A value already in force, as distinct from the one under the cursor. */
  current: '*',
  /** Something held that has not started. A pending task, or a child at rest. */
  waiting: '\u25cb',
  /**
   * Opens an action. It pulses while the action runs, turns green when it
   * succeeds, and turns red when it fails. The shape is the same in every
   * state, so `NO_COLOR` still shows each action as one block.
   */
  action: '\u25cf',
  /** An unmarked row. Assistant prose, and unselected list rows. */
  none: ' ',
} as const

/**
 * Tree glyphs that hang a block's items from its head, drawn in the rail.
 *
 * A branch opens each item. A stem carries an item's lines down to the next.
 * The last item's corner closes the block. A step's calls hang from the head
 * that counts them this way, and so do the task list and the subagent entries,
 * so every block with a head and items uses the same layout. Markers, for the
 * reason {@link MARKER} gives.
 */
export const TREE = { branch: '\u251c', corner: '\u2514', stem: '\u2502' } as const

/**
 * Verbs naming what the agent did.
 *
 * Named, not drawn as icons. A verb is readable at a glance, survives
 * every font and locale, and stays legible when pasted into a bug report.
 * Approvals use the same words instead of their own marks. Reasoning takes
 * none. It is the model's prose, not an action, and it is a dim italic paragraph.
 */
export const VERB = {
  run: 'run',
  read: 'read',
  edit: 'edit',
  find: 'find',
  fetch: 'fetch',
  plan: 'plan',
  ask: 'ask',
  note: 'note',
  error: 'error',
  done: 'done',
} as const

/** A verb this vocabulary names. */
export type Verb = typeof VERB[keyof typeof VERB]

/**
 * Past tense of a finished action's verb.
 *
 * The block keeps its place and its words. Only the verb changes tense. A
 * running `run` means work in progress. A finished `ran` means done, without
 * a second row saying so. Every form fits the verb column, including its gap.
 */
export const PAST: Readonly<Record<Verb, string>> = {
  run: 'ran',
  read: 'read',
  edit: 'edited',
  find: 'found',
  fetch: 'got',
  plan: 'plan',
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
