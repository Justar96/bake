/** Text transformations are independent of terminal and harness state. */
import { expect, it } from 'bun:test'
import { composerText, eraseLast, draftAt, insertText, moveCursor, eraseAtCursor } from '../src/editor.ts'

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
