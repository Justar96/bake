/** The caret covers a cell in reverse video instead of taking a column of its own. */
import { expect, test } from 'bun:test'
import { CARET_OFF, CARET_ON, caretBefore, caretCell } from '../src/caret.ts'

const on = (cell: string) => `${CARET_ON}${cell}${CARET_OFF}`

test('covers a grapheme whole, a tab\'s first column, or a blank cell after the text', () => {
  expect(caretCell('a')).toBe(on('a'))
  expect(caretCell('👩🏽‍💻')).toBe(on('👩🏽‍💻'))
  expect(caretCell('    ')).toBe(`${on(' ')}   `)
  expect(caretCell('')).toBe(on(' '))
})

test('covers the first grapheme of the text after the caret', () => {
  expect(caretBefore('e\u0301tude')).toBe(`${on('e\u0301')}tude`)
  expect(caretBefore('')).toBe(on(' '))
  expect(caretBefore('\nnext')).toBe(`${on(' ')}\nnext`)
})
