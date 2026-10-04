/** Grapheme-safe text editing; Ink owns key and bracketed-paste decoding. */
import stringWidth from 'string-width'

const segmenter = new Intl.Segmenter(undefined, { granularity: 'grapheme' })
const words = new Intl.Segmenter('en', { granularity: 'word' })
const dictionaryScript = /[\p{Script=Thai}\p{Script=Lao}\p{Script=Khmer}\p{Script=Myanmar}]/u

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
 * ASCII punctuation that ends a word inside one Unicode word, such as the dots
 * of a path, the hyphen of kebab-case, or the slash of a URL, so a word step
 * stops at each part. Adapted from pi's word navigation (MIT, Mario Zechner).
 */
const PUNCTUATION = /[(){}[\]<>.,;:'"!?+\-=*/\\|&%^$#@~`]/u
const PUNCTUATION_ALL = new RegExp(PUNCTUATION.source, 'gu')
const WHITESPACE = /\s/u

/**
 * The span of the placeholder a cursor step or erase would enter.
 * @param text - complete draft.
 * @param cursor - UTF-16 cursor offset.
 * @param atoms - placeholders that edit as one character.
 * @param direction - backward enters a span ending at or around the cursor; forward one starting there.
 * @returns the span's offsets, or undefined when the step leaves every placeholder alone.
 */
function atomAt(text: string, cursor: number, atoms: readonly string[], direction: 'backward' | 'forward'):
  { readonly start: number, readonly end: number } | undefined {
  for (const atom of atoms) {
    if (atom === '') continue
    for (let start = text.indexOf(atom); start !== -1; start = text.indexOf(atom, start + atom.length)) {
      const end = start + atom.length
      if (direction === 'backward' ? start < cursor && cursor <= end : start <= cursor && cursor < end) return { start, end }
    }
  }
  return undefined
}

/**
 * Move by one grapheme or to the current logical line's boundary.
 * @param draft - current text and cursor.
 * @param direction - desired cursor movement.
 * @param atoms - placeholders the cursor steps over whole.
 * @returns the draft with its new cursor; text remains unchanged.
 */
export function moveCursor(draft: Draft, direction: 'left' | 'right' | 'home' | 'end', atoms: readonly string[] = []): Draft {
  const { text, cursor } = draft
  if (direction === 'left' || direction === 'right') {
    const atom = atomAt(text, cursor, atoms, direction === 'left' ? 'backward' : 'forward')
    if (atom !== undefined) return { text, cursor: direction === 'left' ? atom.start : atom.end }
  }
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
 * Move a cursor that falls strictly inside a placeholder to the placeholder's start.
 * @param text - complete draft.
 * @param cursor - UTF-16 offset at a grapheme boundary.
 * @param atoms - placeholders that edit as one character.
 * @returns the cursor, outside every placeholder.
 */
export function outsideAtoms(text: string, cursor: number, atoms: readonly string[]): number {
  const atom = atomAt(text, cursor, atoms, 'forward')
  return atom !== undefined && atom.start < cursor ? atom.start : cursor
}

/**
 * Widen a range so it removes any placeholder it touches whole.
 * @param text - complete draft.
 * @param start - first UTF-16 offset of the range.
 * @param end - offset after the range.
 * @param atoms - placeholders that edit as one character.
 * @returns the widened range.
 */
export function atomRange(text: string, start: number, end: number, atoms: readonly string[]): { readonly start: number, readonly end: number } {
  const first = atomAt(text, start, atoms, 'forward')
  const last = atomAt(text, end, atoms, 'backward')
  return {
    start: first !== undefined && first.start < start ? first.start : start,
    end: last !== undefined && last.end > end ? last.end : end,
  }
}

/**
 * Where a word step from the cursor lands.
 *
 * A step skips whitespace, then one word, one run of punctuation, or one
 * placeholder. Inside a word, ASCII punctuation such as the dots of a path
 * is a stop of its own. Unicode word boundaries split CJK, Thai, and other
 * scripts without spaces. The step stays inside its logical line: at a line's
 * edge it crosses the line break alone.
 *
 * @param text - complete draft.
 * @param cursor - UTF-16 offset at a grapheme boundary.
 * @param direction - toward the start or the end of the draft.
 * @param atoms - placeholders a step crosses whole.
 * @param opaque - treat each line as one word, so a masked secret's word boundaries are not revealed.
 * @returns the new offset, at a grapheme boundary.
 */
export function wordStop(text: string, cursor: number, direction: 'left' | 'right', atoms: readonly string[] = [], opaque = false): number {
  const lineStart = text.lastIndexOf('\n', cursor - 1) + 1
  const next = text.indexOf('\n', cursor)
  const lineEnd = next < 0 ? text.length : next
  if (direction === 'left') {
    if (cursor === lineStart) return Math.max(0, cursor - 1)
    if (opaque) return lineStart
    let at = cursor
    while (at > lineStart && WHITESPACE.test(text[at - 1]!)) at--
    if (at === lineStart) return at
    const atom = atomAt(text, at, atoms, 'backward')
    if (atom !== undefined) return atom.start
    const segments = [...words.segment(text.slice(lineStart, at))]
    const last = segments.at(-1)!
    if (last.isWordLike === true) {
      // After the last punctuation that leaves part of the word to cross.
      const inner = [...last.segment.matchAll(PUNCTUATION_ALL)]
        .map(match => match.index + match[0].length).filter(end => end < last.segment.length)
      return lineStart + last.index + (inner.at(-1) ?? 0)
    }
    let index = segments.length - 1
    while (index >= 0 && segments[index]!.isWordLike !== true && !WHITESPACE.test(segments[index]!.segment)
      && atomAt(text, lineStart + segments[index]!.index + segments[index]!.segment.length, atoms, 'backward') === undefined) index--
    return lineStart + (index < 0 ? 0 : segments[index]!.index + segments[index]!.segment.length)
  }
  if (cursor === lineEnd) return Math.min(text.length, cursor + 1)
  if (opaque) return lineEnd
  let at = cursor
  while (at < lineEnd && WHITESPACE.test(text[at]!)) at++
  if (at === lineEnd) return at
  const atom = atomAt(text, at, atoms, 'forward')
  if (atom !== undefined) return atom.end
  let offset = at
  for (const segment of words.segment(text.slice(at, lineEnd))) {
    if (offset === at && segment.isWordLike === true) {
      const match = PUNCTUATION.exec(segment.segment)
      return at + (match === null || match.index === 0 ? segment.segment.length : match.index)
    }
    if (segment.isWordLike === true || WHITESPACE.test(segment.segment) || atomAt(text, offset, atoms, 'forward') !== undefined) break
    offset += segment.segment.length
  }
  return offset
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
 * Delete one adjacent visible grapheme, including combining marks, or one whole placeholder.
 * @param draft - current text and cursor.
 * @param direction - Backspace removes left; Delete removes right.
 * @param atoms - placeholders one erase removes whole.
 * @returns the edited draft, preserving the remaining graphemes.
 */
export function eraseAtCursor(draft: Draft, direction: 'backward' | 'forward', atoms: readonly string[] = []): Draft {
  const atom = atomAt(draft.text, draft.cursor, atoms, direction)
  if (atom !== undefined) return draftAt(draft.text.slice(0, atom.start) + draft.text.slice(atom.end), atom.start)
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
 * Draw a caret over one drawn cell.
 * @param cell - the cell's text: a grapheme, the spaces a tab expands to, or
 *   empty after a row's text and for whitespace hanging past its end.
 * @returns the cell as drawn with the caret.
 */
export type DrawCaret = (cell: string) => string

/**
 * Wrap a draft the way an editor wraps, not the way a paragraph wraps.
 *
 * Rows break after whitespace. Whitespace at a break hangs past the row
 * instead of opening the next one, so a wrapped row never starts with a space
 * the user did not type. Wide characters are their own break opportunities,
 * because CJK text has no spaces to break on. Thai, Lao, Khmer, and Myanmar
 * use ICU word boundaries. A word longer than a row starts a row of its own
 * and is split only between graphemes.
 *
 * The layout is computed without the caret and one column narrower than
 * `width`. The caret is drawn over the cell it precedes, or into the column
 * that remains after a row's text. Moving the caret through a draft never
 * reflows it, and a caret at the end of a full row still fits on that row.
 *
 * @param text - complete draft, as `composerText` leaves it.
 * @param cursor - UTF-16 offset of the caret, at a grapheme boundary.
 * @param width - columns available to each row, caret included.
 * @param caret - draws the caret over the cell at the cursor.
 * @returns the rows, each at most `width` columns, and the caret's row.
 */
export function wrapDraft(text: string, cursor: number, width: number, caret: DrawCaret): WrappedDraft {
  const { rows, ends } = layoutDraft(text, width)
  let caretRow = ends.find(end => end.offset === cursor)?.row ?? 0
  let caretCell = Number.POSITIVE_INFINITY
  rows.forEach((row, index) => {
    const at = row.findIndex(cell => cell.offset === cursor)
    if (at !== -1) { caretRow = index; caretCell = at }
  })
  return {
    rows: rows.map((row, index) => {
      const cells = row.map(cell => cell.text)
      if (index === caretRow) {
        if (caretCell < cells.length) cells[caretCell] = caret(cells[caretCell]!)
        else cells.push(caret(''))
      }
      return cells.join('')
    }),
    caret: caretRow,
  }
}

/**
 * Screen rows a draft occupies at a width, as {@link wrapDraft} draws it.
 * @param text - complete draft.
 * @param width - columns available to each row, caret included.
 * @returns at least one.
 */
export function draftRows(text: string, width: number): number {
  return layoutDraft(text, width).rows.length
}

/** A one-line draft window that keeps the caret visible in a bounded field. */
export interface CursorWindow {
  /** Display text before the caret, including an ellipsis when text is hidden. */
  readonly before: string
  /** The grapheme the caret covers, or empty at the end of the visible text. */
  readonly under: string
  /** Display text after the caret's grapheme, including an ellipsis when text is hidden. */
  readonly after: string
}

/**
 * Keep a one-line input's caret visible while preserving as much surrounding
 * text as the field width allows. The inputs are already display text, so a
 * masked secret and a normal URL use the same cell accounting.
 * @param before - display text before the caret.
 * @param after - display text after the caret.
 * @param width - cells available for text and the caret.
 * @returns the visible before and after portions.
 */
export function cursorWindow(before: string, after: string, width: number): CursorWindow {
  const segments = Array.from(segmenter.segment(before + after), ({ segment }) => segment)
  const cursor = Array.from(segmenter.segment(before)).length
  const capacity = Math.max(0, width - 1)
  const build = (limit: number): { readonly start: number; readonly end: number } => {
    let start = cursor
    let end = cursor
    let used = 0
    while (true) {
      const left = start === 0 ? undefined : stringWidth(segments[start - 1]!)
      const right = end === segments.length ? undefined : stringWidth(segments[end]!)
      const leftFits = left !== undefined && used + left <= limit
      const rightFits = right !== undefined && used + right <= limit
      if (!leftFits && !rightFits) break
      if (leftFits && rightFits) {
        if (end - cursor < cursor - start) { end += 1; used += right! }
        else { start -= 1; used += left! }
      } else if (rightFits) { end += 1; used += right! }
      else { start -= 1; used += left! }
    }
    return { start, end }
  }
  let limit = capacity
  let window = build(limit)
  for (let pass = 0; pass < 3; pass++) {
    const markers = Number(window.start > 0) + Number(window.end < segments.length)
    const next = Math.max(0, capacity - markers)
    if (next === limit) break
    limit = next
    window = build(limit)
  }
  const hiddenBefore = window.start > 0
  const hiddenAfter = window.end < segments.length
  const under = window.end > cursor ? segments[cursor]! : ''
  return {
    before: `${hiddenBefore ? '\u2026' : ''}${segments.slice(window.start, cursor).join('')}`,
    under,
    after: `${segments.slice(cursor + (under === '' ? 0 : 1), window.end).join('')}${hiddenAfter ? '\u2026' : ''}`,
  }
}

/**
 * Move the caret to the screen row above or below, keeping its column.
 *
 * Rows are the ones {@link wrapDraft} draws at the same width, so the caret
 * moves between what the user sees, not between logical lines. The column is
 * counted in terminal cells, and the caret lands on the last place in the row
 * that is not right of it, so a wide character is never split. A wrapped
 * row's end is the start of the next row, so its last place is before its
 * last grapheme; only a logical line's last row has a place after its text.
 *
 * @param draft - text and a cursor at a grapheme boundary.
 * @param width - columns available to each row, caret included.
 * @param direction - the row to move to.
 * @param goal - the column a run of vertical moves keeps to, so crossing a
 *   short row does not pull the caret left for good; the caret's own column
 *   when absent.
 * @returns the moved draft and the column it kept, or undefined when the caret
 *   is already on the first row (up) or the last row (down).
 */
export function moveVertically(draft: Draft, width: number, direction: 'up' | 'down', goal?: number):
  { readonly draft: Draft, readonly goal: number } | undefined {
  const places = rowStops(draft.text, width)
  const from = places.findIndex(stops => stops.some(stop => stop.offset === draft.cursor))
  const to = from + (direction === 'up' ? -1 : 1)
  if (from < 0 || to < 0 || to >= places.length) return undefined
  const column = goal ?? places[from]!.find(stop => stop.offset === draft.cursor)!.column
  return { draft: { text: draft.text, cursor: landing(places[to]!, column) }, goal: column }
}

/**
 * Move the caret to the start or end of the screen row it is on, then, from
 * there, to the start or end of its logical line.
 *
 * Rows are the ones {@link wrapDraft} draws at the same width. A wrapped
 * row's end is the start of the next row, so its last place is before its
 * last grapheme, which on a row broken at a space is the space hanging past
 * the row.
 *
 * @param draft - text and a cursor at a grapheme boundary.
 * @param width - columns available to each row, caret included.
 * @param edge - the row's start or end.
 * @returns the moved draft.
 */
export function moveToRowEdge(draft: Draft, width: number, edge: 'start' | 'end'): Draft {
  const stops = rowStops(draft.text, width).find(row => row.some(stop => stop.offset === draft.cursor))
  const target = edge === 'start' ? stops?.[0]?.offset : stops?.at(-1)?.offset
  if (target === undefined || target === draft.cursor) return moveCursor(draft, edge === 'start' ? 'home' : 'end')
  return { text: draft.text, cursor: target }
}

/**
 * The draft offset under a screen cell of the drawn rows, as a click places the caret.
 * @param text - complete draft.
 * @param width - columns available to each row, caret included.
 * @param row - zero-based index into the rows {@link wrapDraft} draws.
 * @param column - zero-based cell within the row; past its text reaches the row's last place.
 * @returns the offset before the grapheme drawn at that cell, or undefined past the last row.
 */
export function offsetAt(text: string, width: number, row: number, column: number): number | undefined {
  const stops = rowStops(text, width)[row]
  return stops === undefined ? undefined : landing(stops, column)
}

/** A place the caret can rest on a drawn row, and its cell. */
interface Stop {
  readonly offset: number
  readonly column: number
}

/**
 * The caret's places on each drawn row: before each cell, and after the text
 * of a logical line's last row.
 */
function rowStops(text: string, width: number): readonly (readonly Stop[])[] {
  const { rows, ends } = layoutDraft(text, width)
  return rows.map((row, index) => {
    let column = 0
    const stops = row.map(cell => {
      const stop = { offset: cell.offset, column }
      column += cell.width
      return stop
    })
    const end = ends.find(line => line.row === index)
    return end === undefined ? stops : [...stops, { offset: end.offset, column }]
  })
}

/** The last place in a row that is not right of a column, so a wide character is never split. */
function landing(stops: readonly Stop[], column: number): number {
  let offset = stops[0]!.offset
  for (const stop of stops) if (stop.column <= column) offset = stop.offset
  return offset
}

/**
 * Lay a draft out in rows of graphemes, without a caret.
 * @param text - complete draft.
 * @param width - columns available to each row, caret included.
 * @returns each row's cells, and the row each logical line ends on.
 */
function layoutDraft(text: string, width: number): {
  readonly rows: readonly (readonly Cell[])[]
  readonly ends: readonly { readonly offset: number, readonly row: number }[]
} {
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
    const breaks = new Set<number>()
    if (dictionaryScript.test(line)) {
      for (const { segment, index } of words.segment(line)) {
        if (dictionaryScript.test(segment)) {
          breaks.add(offset + index).add(offset + index + segment.length)
        }
      }
    }
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
        if (word.length > 0 && (breaks.has(at)
          || (cell.width > 1 && !dictionaryScript.test(cell.text))
          || (word.at(-1)!.width > 1 && !dictionaryScript.test(word.at(-1)!.text)))) break
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
  return { rows, ends }
}
