/** Grapheme-safe text editing; Ink owns key and bracketed-paste decoding. */
import stringWidth from 'string-width'

const segmenter = new Intl.Segmenter(undefined, { granularity: 'grapheme' })

/** Text and a UTF-16 cursor offset at a grapheme boundary. */
export interface Draft {
  readonly text: string
  readonly cursor: number
}

/**
 * Remove terminal controls while retaining pasted line breaks and tabs.
 * @param text - text delivered by Ink.
 * @returns text safe to display in the composer.
 */
export function composerText(text: string): string {
  return text.replace(/\r\n?/g, '\n').replace(/[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f-\u009f]/g, '')
}

/**
 * Place a cursor without splitting a visible character.
 * @param text - complete draft.
 * @param cursor - requested UTF-16 offset, defaulting to the end.
 * @returns a draft with the cursor at the next grapheme boundary.
 */
export function draftAt(text: string, cursor = text.length): Draft {
  for (const segment of segmenter.segment(text)) {
    if (segment.index >= cursor) return { text, cursor: segment.index }
  }
  return { text, cursor: text.length }
}

/**
 * Move by one grapheme or to the current logical line's boundary.
 * @param draft - current text and cursor.
 * @param direction - desired cursor movement.
 * @returns the draft with its new cursor; text remains unchanged.
 */
export function moveCursor(draft: Draft, direction: 'left' | 'right' | 'home' | 'end'): Draft {
  const { text, cursor } = draft
  if (direction === 'home') return { text, cursor: text.slice(0, cursor).lastIndexOf('\n') + 1 }
  if (direction === 'end') {
    const next = text.indexOf('\n', cursor)
    return { text, cursor: next < 0 ? text.length : next }
  }
  let previous = 0
  for (const segment of segmenter.segment(text)) {
    if (direction === 'right' && segment.index > cursor) return { text, cursor: segment.index }
    if (direction === 'left' && segment.index >= cursor) return { text, cursor: previous }
    previous = segment.index
  }
  return { text, cursor: direction === 'left' ? previous : text.length }
}

/**
 * Insert literal text at the cursor; pasted newlines never submit.
 * @param draft - current text and cursor.
 * @param value - text delivered by Ink or the clipboard.
 * @returns the edited draft, with its cursor following the inserted graphemes.
 */
export function insertText(draft: Draft, value: string): Draft {
  const inserted = composerText(value)
  return draftAt(draft.text.slice(0, draft.cursor) + inserted + draft.text.slice(draft.cursor), draft.cursor + inserted.length)
}

/**
 * Delete one adjacent visible grapheme, including combining marks.
 * @param draft - current text and cursor.
 * @param direction - Backspace removes left; Delete removes right.
 * @returns the edited draft, preserving the remaining graphemes.
 */
export function eraseAtCursor(draft: Draft, direction: 'backward' | 'forward'): Draft {
  const neighbor = moveCursor(draft, direction === 'backward' ? 'left' : 'right').cursor
  const start = Math.min(neighbor, draft.cursor)
  const end = Math.max(neighbor, draft.cursor)
  return draftAt(draft.text.slice(0, start) + draft.text.slice(end), start)
}

/**
 * Remove the last visible grapheme, including its combining marks.
 * @param text - the current value.
 * @returns the text before its final grapheme.
 */
export function eraseLast(text: string): string {
  return eraseAtCursor(draftAt(text), 'backward').text
}

/**
 * Columns between tab stops in the composer, counted from the start of a row.
 *
 * A raw tab cannot be laid out. `string-width` measures it as zero columns,
 * and the terminal advances to its own tab stop. Every cell after that tab
 * lands somewhere the layout did not reserve, and the cursor moves over what
 * was already drawn there instead of erasing it. Expand tabs to spaces.
 */
export const TAB_COLUMNS = 4

/** A draft laid out in screen rows, with the caret drawn into its row. */
export interface WrappedDraft {
  /** Each row as displayed. Tabs are expanded, and the caret is inserted. */
  readonly rows: readonly string[]
  /** Index of the row holding the caret. */
  readonly caret: number
}

interface Cell {
  readonly offset: number
  readonly text: string
  readonly width: number
}

/**
 * Wrap a draft the way an editor wraps, not the way a paragraph wraps.
 *
 * Rows break after whitespace. Whitespace at a break hangs past the row
 * instead of opening the next one, so a wrapped row never starts with a space
 * the user did not type. Wide characters are their own break opportunities,
 * because CJK text has no spaces to break on. A word longer than a row starts
 * a row of its own and is split at each row boundary.
 *
 * The layout is computed without the caret and one column narrower than
 * `width`. The caret is then drawn into the column that remains. Moving the
 * caret through a draft never reflows it, and a caret at the end of a full
 * row still fits on that row.
 *
 * @param text - complete draft, as `composerText` leaves it.
 * @param cursor - UTF-16 offset of the caret, at a grapheme boundary.
 * @param width - columns available to each row, caret included.
 * @param caret - one-column glyph drawn at the cursor.
 * @returns the rows, each at most `width` columns, and the caret's row.
 */
export function wrapDraft(text: string, cursor: number, width: number, caret: string): WrappedDraft {
  const limit = Math.max(1, width - 1)
  const rows: Cell[][] = []
  // Where each logical line's rows end, for a caret after its last grapheme.
  const ends: { readonly offset: number, readonly row: number }[] = []
  let offset = 0
  for (const line of text.split('\n')) {
    let row: Cell[] = []
    let column = 0
    rows.push(row)
    const open = (): void => { row = []; column = 0; rows.push(row) }
    const place = (cell: Cell): void => { row.push(cell); column += cell.width }
    const graphemes = Array.from(segmenter.segment(line), ({ segment, index }) => ({ segment, index: offset + index }))
    for (let index = 0; index < graphemes.length;) {
      const first = graphemes[index]!
      if (first.segment === ' ' || first.segment === '\t') {
        const stop = first.segment === '\t' ? TAB_COLUMNS - column % TAB_COLUMNS : 1
        // Past the row's end the whitespace hangs. It stays on this row, drawn
        // as nothing, and the next word opens the following row.
        const shown = Math.max(0, Math.min(stop, limit - column))
        place({ offset: first.index, text: ' '.repeat(shown), width: shown })
        index++
        continue
      }
      const word: Cell[] = []
      for (; index < graphemes.length; index++) {
        const { segment, index: at } = graphemes[index]!
        if (segment === ' ' || segment === '\t') break
        const cell = { offset: at, text: segment, width: stringWidth(segment) }
        if (word.length > 0 && (cell.width > 1 || word.at(-1)!.width > 1)) break
        word.push(cell)
      }
      const size = word.reduce((sum, cell) => sum + cell.width, 0)
      // A word that does not fit opens a row even when it must then be split.
      // A pasted path or URL starts at the left edge, not after a space.
      if (column > 0 && column + size > limit) open()
      for (const cell of word) {
        if (column > 0 && column + cell.width > limit) open()
        place(cell)
      }
    }
    offset += line.length + 1
    ends.push({ offset: offset - 1, row: rows.length - 1 })
  }
  let caretRow = ends.find(end => end.offset === cursor)?.row ?? 0
  let caretCell = Number.POSITIVE_INFINITY
  rows.forEach((row, index) => {
    const at = row.findIndex(cell => cell.offset === cursor)
    if (at !== -1) { caretRow = index; caretCell = at }
  })
  return {
    rows: rows.map((row, index) => {
      const cells = row.map(cell => cell.text)
      if (index === caretRow) cells.splice(Math.min(caretCell, cells.length), 0, caret)
      return cells.join('')
    }),
    caret: caretRow,
  }
}
