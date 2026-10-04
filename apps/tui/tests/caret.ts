/**
 * Test frames mark the input caret with `▌`.
 *
 * A terminal shows the caret as one reverse-video cell: a blank one after the
 * text, or the character the caret precedes. A test frame writes the blank as
 * `▌` and puts `▌` before a covered character, so `draft▌` is a caret at the
 * end and `dr▌aft` one before the `a`. Only an isolated reverse-video cell in
 * the default colours is a caret; a longer run, such as a selected picker row,
 * and a coloured cell, such as one changed character of a diff, stay as they are.
 */
import type { IBufferLine } from '@xterm/headless'

/** The caret's mark in a test frame. */
export const CARET = '\u258c'

const graphemes = new Intl.Segmenter(undefined, { granularity: 'grapheme' })
/** A reverse-video span with no other code inside it, or any other SGR sequence. */
const TOKEN = /\u001b\[7m([^\u001b]*)\u001b\[27m|\u001b\[([\d;:]*)m/gu

/**
 * @param frame - rendered text with its SGR codes, as Ink writes it: each
 *   row's styles open and close within the row.
 * @returns the frame with each one-grapheme reverse-video span in the default
 *   colours marked as the caret.
 */
export function markCaret(frame: string): string {
  // Whether a foreground or background colour is set where the scan stands.
  let foreground = false
  let background = false
  return frame.replace(TOKEN, (match, cell: string | undefined, codes: string | undefined) => {
    if (cell !== undefined) {
      if (foreground || background || Array.from(graphemes.segment(cell)).length !== 1) return match
      return cell === ' ' ? CARET : `${CARET}${cell}`
    }
    const parts = (codes ?? '').split(';')
    for (let index = 0; index < parts.length; index++) {
      const code = parts[index]!
      const value = Number(code.split(':')[0])
      if (code === '' || value === 0) { foreground = false; background = false }
      else if (value === 39) foreground = false
      else if (value === 49) background = false
      else if ((value >= 30 && value <= 38) || (value >= 90 && value <= 97)) foreground = true
      else if ((value >= 40 && value <= 48) || (value >= 100 && value <= 107)) background = true
      // An extended colour's own parameters follow it.
      if ((value === 38 || value === 48) && !code.includes(':')) index += parts[index + 1] === '5' ? 2 : 4
    }
    return match
  })
}

/** One drawn cell, and whether it could be the caret. */
interface Cell {
  readonly text: string
  readonly reversed: boolean
}

function cellsOf(line: IBufferLine | undefined, columns: number): Cell[] {
  const cells: Cell[] = []
  for (let column = 0; line !== undefined && column < columns; column++) {
    const cell = line.getCell(column)
    if (cell === undefined) break
    // The second column of a wide character.
    if (cell.getWidth() === 0) continue
    cells.push({ text: cell.getChars() === '' ? ' ' : cell.getChars(),
      reversed: cell.isInverse() !== 0 && cell.isFgDefault() && cell.isBgDefault() })
  }
  return cells
}

/**
 * The start of each run of reversed cells that draws one grapheme. A grapheme
 * with a spacing mark, such as Thai น้ำ, takes two cells.
 */
function caretStarts(cells: readonly Cell[]): number[] {
  const starts: number[] = []
  for (let index = 0; index < cells.length; index++) {
    if (!cells[index]!.reversed) continue
    let end = index
    while (cells[end + 1]?.reversed === true) end++
    const text = cells.slice(index, end + 1).map(cell => cell.text).join('')
    if (Array.from(graphemes.segment(text)).length === 1) starts.push(index)
    index = end
  }
  return starts
}

function draw(cells: readonly Cell[], caret: (index: number) => boolean): string {
  return cells.map((cell, index) => !caret(index) ? cell.text : cell.text === ' ' ? CARET : `${CARET}${cell.text}`).join('').trimEnd()
}

/**
 * @param line - one row of an xterm buffer.
 * @param columns - the terminal's width.
 * @returns the row's text without trailing blanks, each possible caret marked as {@link markCaret} marks it.
 */
export function caretRow(line: IBufferLine | undefined, columns: number): string {
  const cells = cellsOf(line, columns)
  const starts = caretStarts(cells)
  return draw(cells, index => starts.includes(index))
}

/**
 * Rows of a whole screen, with only its lowest possible caret marked. The
 * input is drawn below everything else, so this tells the caret from one
 * reversed character of an uncoloured diff above it.
 * @param lines - the screen's rows, top first.
 * @param columns - the terminal's width.
 * @returns each row's text without trailing blanks.
 */
export function caretRows(lines: readonly (IBufferLine | undefined)[], columns: number): string[] {
  const rows = lines.map(line => cellsOf(line, columns))
  let at: { readonly row: number, readonly index: number } | undefined
  rows.forEach((cells, row) => { for (const index of caretStarts(cells)) at = { row, index } })
  return rows.map((cells, row) => draw(cells, index => at?.row === row && at.index === index))
}
