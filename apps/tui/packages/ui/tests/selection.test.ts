/** Fullscreen selection geometry: ordering, the cells of each row, words, and reversed runs. */
import { expect, it } from 'bun:test'
import { columnOffsets, invertSpans, ordered, rowColumns, sliceCells, wordAt } from '../src/selection.ts'

const at = (row: number, column: number, boundary?: true) => ({ row, offset: 0, column, ...boundary ? { boundary } : {} })

it('orders a selection either way and treats one cell as empty', () => {
  expect(ordered(at(3, 4), at(1, 2))).toEqual({ start: at(1, 2), end: at(3, 4) })
  expect(ordered(at(1, 2), at(1, 2))).toBeUndefined()
  expect(ordered(undefined, at(1, 2))).toBeUndefined()
  // Wrapped rows of one line order by their offset.
  expect(ordered({ row: 1, offset: 2, column: 0 }, { row: 1, offset: 1, column: 5 })?.start).toEqual({ row: 1, offset: 1, column: 5 })
})

it('covers the first row from its start cell, middle rows whole, and the last row through its end cell', () => {
  const range = { start: at(1, 3), end: at(3, 2) }
  expect(rowColumns(range, at(0, 0), 'before')).toBeUndefined()
  expect(rowColumns(range, at(1, 0), 'abcdef')).toEqual({ from: 3, to: 6 })
  expect(rowColumns(range, at(2, 0), 'middle')).toEqual({ from: 0, to: 6 })
  expect(rowColumns(range, at(3, 0), 'last row')).toEqual({ from: 0, to: 3 })
  // A boundary end stops before its column; a row past its text has nothing.
  expect(rowColumns({ start: at(1, 0), end: at(1, 4, true) }, at(1, 0), 'abcdef')).toEqual({ from: 0, to: 4 })
  expect(rowColumns({ start: at(1, 9), end: at(2, 0) }, at(1, 0), 'abc')).toBeUndefined()
})

it('widens a cut through a wide character to the whole character', () => {
  // `中` spans columns 2 and 3.
  expect(rowColumns({ start: at(0, 3), end: at(0, 3) }, at(0, 0), 'ab中cd')).toEqual({ from: 2, to: 4 })
  expect(rowColumns({ start: at(0, 3), end: at(0, 4) }, at(0, 0), 'ab中cd')).toEqual({ from: 2, to: 5 })
  expect(sliceCells('ab中cd', 2, 5)).toBe('中c')
  expect(columnOffsets('ab中cd', 2, 5)).toEqual({ start: 2, end: 4 })
})

it('takes a word whole across path and hyphen joiners, and nothing between words', () => {
  const text = 'open apps/tui/src then kebab-case.'
  expect(wordAt(text, 0)).toEqual({ from: 0, to: 4 })
  expect(wordAt(text, 9)).toEqual({ from: 5, to: 17 })
  expect(wordAt(text, 25)).toEqual({ from: 23, to: 33 })
  expect(wordAt(text, 4)).toBeUndefined()
  expect(wordAt(text, 33)).toBeUndefined()
})

it('marks only the selected part of each run, and of text past the runs', () => {
  expect(invertSpans([{ length: 4, tone: 'body' }, { length: 4, tone: 'error' }], 10, 'body', 2, 9)).toEqual([
    { length: 2, tone: 'body' }, { length: 2, tone: 'body', selected: true },
    { length: 4, tone: 'error', selected: true },
    { length: 1, tone: 'body', selected: true }, { length: 1, tone: 'body' },
  ])
  expect(invertSpans(undefined, 5, 'quiet', 0, 5)).toEqual([{ length: 5, tone: 'quiet', selected: true }])
})
