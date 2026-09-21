/** Text transformations are independent of terminal and harness state. */
import { expect, it } from 'bun:test'
import { composerText, eraseLast } from '../src/editor.ts'

it('keeps pasted lines and tabs while removing terminal controls', () => {
  expect(composerText('a\r\nb\rc\t\u0003\u0000')).toBe('a\nb\nc\t')
})
it('backspaces one visible grapheme', () => {
  expect(eraseLast('a👩🏽‍💻')).toBe('a')
  expect(eraseLast('ae\u0301')).toBe('a')
  expect(eraseLast('')).toBe('')
})
