/**
 * The input caret: one cell in reverse video over the character it precedes,
 * or over a blank cell after the text.
 *
 * Reverse video leaves every character where it is, so moving the caret
 * never shifts the text after it, and a wide character is covered whole. The
 * codes are written into the text instead of set through Ink's `inverse`,
 * which Chalk drops on a terminal it reads as colourless, so the caret shows
 * wherever the frame does. Reverse video carries no colour, and the runner's
 * `NO_COLOR` filter keeps these two codes.
 *
 * @module bake-tui-ui/caret
 */

/** Reverse video on (SGR 7). */
export const CARET_ON = '\u001b[7m'
/** Reverse video off (SGR 27), leaving every other attribute as it was. */
export const CARET_OFF = '\u001b[27m'

const graphemes = new Intl.Segmenter(undefined, { granularity: 'grapheme' })

/**
 * Draw the caret over one drawn cell.
 * @param cell - a grapheme, the spaces a tab expands to, or empty after the
 *   text, where the caret takes a blank cell of its own.
 * @returns the cell with its first column in reverse video.
 */
export function caretCell(cell: string): string {
  if (cell === '') return `${CARET_ON} ${CARET_OFF}`
  // A tab expanded to spaces: the caret covers its first column.
  if (/^ +$/u.test(cell)) return `${CARET_ON} ${CARET_OFF}${cell.slice(1)}`
  return `${CARET_ON}${cell}${CARET_OFF}`
}

/**
 * Draw the caret over the start of the text that follows it.
 * @param after - the text after the caret, as it is displayed.
 * @returns that text with the caret over its first grapheme, or over a blank
 *   cell before it when it is empty or starts a new line or a tab.
 */
export function caretBefore(after: string): string {
  const first = graphemes.segment(after)[Symbol.iterator]().next().value?.segment
  if (first === undefined || first === '\n' || first === '\t') return `${caretCell('')}${after}`
  return `${caretCell(first)}${after.slice(first.length)}`
}
