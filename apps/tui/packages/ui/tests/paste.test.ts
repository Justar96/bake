/** Paste placeholders collapse, expand, and edit as one character. */
import { expect, it } from 'bun:test'
import { draftAt, eraseAtCursor, moveCursor } from '../src/editor.ts'
import { collapses, expandPastes, imagePath, imageToken, pastedTextToken, type PasteAtom } from '../src/paste.ts'

it('collapses pastes over 800 characters or of three lines and more', () => {
  expect(collapses('a\nb')).toBe(false)
  expect(collapses('a\nb\nc')).toBe(true)
  expect(collapses('x'.repeat(800))).toBe(false)
  expect(collapses('x'.repeat(801))).toBe(true)
  expect(pastedTextToken(1, 'a\nb\nc')).toBe('[Pasted text #1 +2 lines]')
  expect(pastedTextToken(2, 'x'.repeat(900))).toBe('[Pasted text #2 900 chars]')
  expect(imageToken(3)).toBe('[Image #3]')
})

it('expands registered text placeholders and leaves image placeholders and look-alikes', () => {
  const atoms = new Map<string, PasteAtom>([
    ['[Pasted text #1 +2 lines]', { kind: 'text', text: 'a\nb\nc' }],
    ['[Image #2]', { kind: 'image', key: '7' }],
  ])
  expect(expandPastes('see [Pasted text #1 +2 lines] and [Image #2] [Pasted text #9 +1 lines]', atoms))
    .toBe('see a\nb\nc and [Image #2] [Pasted text #9 +1 lines]')
})

it('reads one dropped image path however the terminal quotes it', () => {
  expect(imagePath('/tmp/shot.png')).toBe('/tmp/shot.png')
  expect(imagePath("'/tmp/my shot.PNG' ")).toBe('/tmp/my shot.PNG')
  expect(imagePath('/tmp/my\\ shot.jpeg')).toBe('/tmp/my shot.jpeg')
  expect(imagePath('file:///tmp/my%20shot.webp')).toBe('/tmp/my shot.webp')
  expect(imagePath('C:\\Users\\me\\shot.gif')).toBe('C:\\Users\\me\\shot.gif')
  expect(imagePath('/tmp/notes.txt')).toBeUndefined()
  expect(imagePath('/tmp/a.png\n/tmp/b.png')).toBeUndefined()
  expect(imagePath('')).toBeUndefined()
})

it('steps over and erases a placeholder whole', () => {
  const token = '[Image #1]'
  const text = `ab${token}cd`
  const after = draftAt(text, 2 + token.length)
  expect(eraseAtCursor(after, 'backward', [token])).toEqual({ text: 'abcd', cursor: 2 })
  expect(eraseAtCursor(draftAt(text, 2), 'forward', [token])).toEqual({ text: 'abcd', cursor: 2 })
  expect(eraseAtCursor(draftAt(text, 5), 'backward', [token])).toEqual({ text: 'abcd', cursor: 2 })
  expect(moveCursor(after, 'left', [token]).cursor).toBe(2)
  expect(moveCursor(draftAt(text, 2), 'right', [token]).cursor).toBe(2 + token.length)
  expect(eraseAtCursor(after, 'backward').text).toBe(`ab[Image #1cd`)
})
