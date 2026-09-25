/** Text transformations are independent of terminal and harness state. */
import { describe, expect, it } from 'bun:test'
import stringWidth from 'string-width'
import { composerText, eraseLast, draftAt, insertText, moveCursor, eraseAtCursor, TAB_COLUMNS, wrapDraft } from '../src/editor.ts'

it('keeps pasted lines and tabs while removing terminal controls', () => {
  expect(composerText('a\r\nb\rc\t\u0003\u0000')).toBe('a\nb\nc\t')
})
it('backspaces one visible grapheme', () => {
  expect(eraseLast('a👩🏽‍💻')).toBe('a')
  expect(eraseLast('ae\u0301')).toBe('a')
  expect(eraseLast('')).toBe('')
})

it('moves and deletes complete graphemes on either side of the cursor', () => {
  const text = 'a👩🏽‍💻e\u0301z'
  let draft = draftAt(text, 1)
  draft = moveCursor(draft, 'right')
  expect(draft.text.slice(0, draft.cursor)).toBe('a👩🏽‍💻')
  expect(eraseAtCursor(draft, 'forward').text).toBe('a👩🏽‍💻z')
  expect(eraseAtCursor(draft, 'backward')).toEqual({ text: 'ae\u0301z', cursor: 1 })
  expect(moveCursor(draft, 'left')).toEqual({ text, cursor: 1 })
  expect(moveCursor(draftAt(text, 0), 'left').cursor).toBe(0)
  expect(moveCursor(draftAt(text), 'right').cursor).toBe(text.length)
})

it('inserts literal multiline text in place and moves to logical line boundaries', () => {
  const draft = insertText(draftAt('first\nlast', 6), '中\r\nline\t')
  expect(draft).toEqual({ text: 'first\n中\nline\tlast', cursor: 13 })
  expect(moveCursor(draft, 'home').cursor).toBe(8)
  expect(moveCursor(draft, 'end').cursor).toBe(draft.text.length)
  expect(eraseAtCursor(draftAt('a\nb', 2), 'backward')).toEqual({ text: 'ab', cursor: 1 })
})

it('keeps the cursor outside graphemes formed by insertion', () => {
  expect(insertText(draftAt('👩💻', 2), '\u200d')).toEqual({ text: '👩‍💻', cursor: '👩‍💻'.length })
  expect(insertText(draftAt('ae\u0301b', 1), '中')).toEqual({ text: 'a中e\u0301b', cursor: 2 })
})

describe('wrapDraft', () => {
  const wrap = (text: string, cursor: number, width: number) => wrapDraft(text, cursor, width, '|')

  it('breaks after whitespace and hangs it, so no wrapped row opens with a space', () => {
    // Laid out one column narrower than the row, which keeps the caret's column.
    expect(wrap('alpha beta gamma', 0, 11).rows).toEqual(['|alpha beta', 'gamma'])
    expect(wrap('alpha  beta   gamma', 0, 7).rows).toEqual(['|alpha ', 'beta  ', 'gamma'])
    for (let width = 4; width < 30; width++) {
      const { rows } = wrap(`${'word 你好 '.repeat(8)}end`, 0, width)
      for (const row of rows.slice(1)) expect(row.startsWith(' '), `${width}: ${JSON.stringify(rows)}`).toBe(false)
      for (const row of rows) expect(stringWidth(row)).toBeLessThanOrEqual(width)
    }
  })

  it('never reflows as the caret moves through the draft', () => {
    const text = 'alpha beta gamma delta epsilon 你好世界 zeta'
    const bare = (rows: readonly string[]) => rows.map(row => row.replace('|', ''))
    const expected = bare(wrap(text, 0, 12).rows)
    for (let cursor = 0; cursor <= text.length; cursor++) {
      const { rows, caret } = wrap(text, cursor, 12)
      expect(bare(rows)).toEqual(expected)
      expect(rows[caret]).toContain('|')
      expect(rows.join('').split('|')).toHaveLength(2)
    }
  })

  it('draws the caret where the next character goes, including after a full row', () => {
    expect(wrap('alpha beta', 10, 11)).toEqual({ rows: ['alpha beta|'], caret: 0 })
    expect(wrap('alpha beta gamma', 11, 11)).toEqual({ rows: ['alpha beta', '|gamma'], caret: 1 })
    // Inside the hanging space. The end of the row it hangs from.
    expect(wrap('alpha beta gamma', 10, 11)).toEqual({ rows: ['alpha beta|', 'gamma'], caret: 0 })
    expect(wrap('one\n\nthree', 4, 20)).toEqual({ rows: ['one', '|', 'three'], caret: 1 })
    expect(wrap('', 0, 20)).toEqual({ rows: ['|'], caret: 0 })
  })

  it('splits a word longer than the row where the row ends', () => {
    expect(wrap('ab abcdefghij', 13, 6).rows).toEqual(['ab ', 'abcde', 'fghij|'])
  })

  it('breaks between wide characters, which have no spaces to break at', () => {
    expect(wrap('你好世界你好', 0, 6).rows).toEqual(['|你好', '世界', '你好'])
    expect(wrap('ab你好', 0, 6).rows).toEqual(['|ab你', '好'])
  })

  it('expands tabs to spaces, which is what the layout measured', () => {
    expect(wrap('\tx', 3, 20).rows).toEqual([`${' '.repeat(TAB_COLUMNS)}x|`])
    expect(wrap('ab\tx', 4, 20).rows).toEqual([`ab${' '.repeat(TAB_COLUMNS - 2)}x|`])
    expect(wrap('abc\tx', 0, 5).rows).toEqual(['|abc ', 'x'])
  })
})
