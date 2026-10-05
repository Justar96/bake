/**
 * Fullscreen text selection: points in transcript coordinates, word and line
 * ranges, and the cells a selection covers on each row.
 *
 * Fullscreen reports the mouse, so the terminal's own selection is gone and
 * Bake draws and copies one instead. A point names a terminal row by the
 * transcript row it shows, not by its place on screen, so a selection stays on
 * its text while the view scrolls or output arrives below it.
 *
 * Adapted from the fullscreen selection of pi's `TuiAltScreen` (MIT, Mario
 * Zechner, https://github.com/earendil-works/pi).
 *
 * @module bake-tui-ui/selection
 */
import stringWidth from 'string-width'
import type { Span } from './present.ts'
import type { Position } from './viewport.ts'

/** A cell of the transcript: the row that draws it, and its column on screen. */
export interface SelectionPoint extends Position {
  readonly column: number
  /**
   * The column is the cell after the selection rather than its last cell. A
   * word or line range ends on a boundary; a dragged point covers its cell.
   */
  readonly boundary?: true
}

/** Two points, the first not after the second. */
export interface SelectionRange {
  readonly start: SelectionPoint
  readonly end: SelectionPoint
}

/** How a press extends: a cell at a time, a word, or a whole row, for one, two, or three clicks. */
export type SelectionGranularity = 'character' | 'word' | 'line'

/** Presses closer together than this, on the same word, count toward a double or triple click. */
export const CLICK_MS = 500

/**
 * Characters that join words into one, as terminals select a path or a
 * kebab-case name whole on a double click.
 */
const JOINERS = new Set(['/', '-'])

const words = new Intl.Segmenter(undefined, { granularity: 'word' })
const graphemes = new Intl.Segmenter(undefined, { granularity: 'grapheme' })

/**
 * Order two positions by the row that draws them.
 * @returns negative when `a` is above `b`, zero on the same row.
 */
export const compareRows = (a: Position, b: Position): number => a.row - b.row || a.offset - b.offset

/** Order two points, by row and then by column. */
export const comparePoints = (a: SelectionPoint, b: SelectionPoint): number => compareRows(a, b) || a.column - b.column

/**
 * The selection between an anchor and a focus, or nothing while they are one cell.
 * @param anchor - where the press began.
 * @param focus - where the pointer is now.
 * @returns the ordered range, or undefined for an empty selection.
 */
export function ordered(anchor: SelectionPoint | undefined, focus: SelectionPoint | undefined): SelectionRange | undefined {
  if (anchor === undefined || focus === undefined) return undefined
  if (compareRows(anchor, focus) === 0 && anchor.column === focus.column && anchor.boundary === focus.boundary) return undefined
  return comparePoints(anchor, focus) <= 0 ? { start: anchor, end: focus } : { start: focus, end: anchor }
}

/** Each grapheme of a row with the cells it spans. */
function cellsOf(text: string): { readonly segment: string, readonly index: number, readonly from: number, readonly to: number }[] {
  const cells = []
  let column = 0
  for (const { segment, index } of graphemes.segment(text)) {
    const width = stringWidth(segment)
    cells.push({ segment, index, from: column, to: column + width })
    column += width
  }
  return cells
}

/**
 * The cells of a row a selection covers.
 * @param range - the selection.
 * @param row - the row, as a position.
 * @param text - the row as drawn, used to widen a cut through a wide character.
 * @returns the half-open columns, or undefined when the selection misses the row.
 */
export function rowColumns(range: SelectionRange, row: Position, text: string): { readonly from: number, readonly to: number } | undefined {
  if (compareRows(row, range.start) < 0 || compareRows(row, range.end) > 0) return undefined
  const width = stringWidth(text)
  const cells = cellsOf(text)
  const cellAt = (column: number) => cells.find(cell => column >= cell.from && column < cell.to)
  const from = compareRows(row, range.start) === 0 ? cellAt(range.start.column)?.from ?? Math.min(range.start.column, width) : 0
  const to = compareRows(row, range.end) !== 0 ? width
    : range.end.boundary === true ? Math.min(range.end.column, width)
      : cellAt(range.end.column)?.to ?? Math.min(range.end.column + 1, width)
  return to > from ? { from, to } : undefined
}

/**
 * The text of a row between two columns.
 * @param text - the row as drawn, without styling.
 * @param from - first cell.
 * @param to - cell after the last.
 * @returns the graphemes that start inside the columns.
 */
export function sliceCells(text: string, from: number, to: number): string {
  return cellsOf(text).filter(cell => cell.from >= from && cell.from < to).map(cell => cell.segment).join('')
}

/**
 * The UTF-16 offsets of a row's columns, for laying styles over them.
 * @param text - the row's text.
 * @param from - first cell.
 * @param to - cell after the last.
 * @returns the code-unit range of the graphemes those cells hold.
 */
export function columnOffsets(text: string, from: number, to: number): { readonly start: number, readonly end: number } {
  const inside = cellsOf(text).filter(cell => cell.from >= from && cell.from < to)
  const first = inside[0]
  const last = inside.at(-1)
  return first === undefined || last === undefined ? { start: 0, end: 0 } : { start: first.index, end: last.index + last.segment.length }
}

/**
 * The word under a column, joined across `/` and `-` so a path or a
 * hyphenated name is one word, as a terminal's double click takes it.
 * @param text - the row as drawn.
 * @param column - the clicked cell.
 * @returns the half-open columns of the word, or undefined between words.
 */
export function wordAt(text: string, column: number): { readonly from: number, readonly to: number } | undefined {
  const segments: { from: number, to: number, selectable: boolean, joiner: boolean }[] = []
  let at = 0
  for (const segment of words.segment(text)) {
    const to = at + stringWidth(segment.segment)
    const joiner = JOINERS.has(segment.segment)
    segments.push({ from: at, to, selectable: segment.isWordLike === true || joiner, joiner })
    at = to
  }
  const clicked = segments.findIndex(segment => column >= segment.from && column < segment.to)
  if (clicked < 0 || !segments[clicked]!.selectable) return undefined
  const joins = (left: typeof segments[number], right: typeof segments[number]): boolean =>
    left.selectable && right.selectable && (left.joiner || right.joiner)
  let first = clicked
  let last = clicked
  while (first > 0 && joins(segments[first - 1]!, segments[first]!)) first--
  while (last < segments.length - 1 && joins(segments[last]!, segments[last + 1]!)) last++
  return { from: segments[first]!.from, to: segments[last]!.to }
}

/**
 * Mark the runs of a row that a selection covers, keeping their own style.
 * @param spans - the row's runs, absent for one run in `tone`.
 * @param length - the row's UTF-16 length.
 * @param tone - the tone a row without runs is drawn in.
 * @param start - first selected code unit.
 * @param end - code unit after the last selected one.
 * @returns runs covering the row, the covered ones `selected`.
 */
export function invertSpans(spans: readonly Span[] | undefined, length: number, tone: Span['tone'], start: number, end: number): readonly Span[] {
  const runs = spans ?? [{ length, tone }]
  const result: Span[] = []
  let offset = 0
  for (const span of runs) {
    const cuts = [offset, Math.max(offset, Math.min(start, offset + span.length)), Math.max(offset, Math.min(end, offset + span.length)), offset + span.length]
    for (let index = 0; index < 3; index++) {
      const width = cuts[index + 1]! - cuts[index]!
      if (width > 0) result.push(index === 1 ? { ...span, length: width, selected: true } : { ...span, length: width })
    }
    offset += span.length
  }
  // Text past the runs is drawn in the row's tone.
  if (offset < length) {
    const from = Math.max(offset, start)
    if (from > offset) result.push({ length: Math.min(from, length) - offset, tone })
    if (end > from && from < length) result.push({ length: Math.min(end, length) - from, tone, selected: true })
    if (Math.max(end, from) < length) result.push({ length: length - Math.max(end, from), tone })
  }
  return result
}
