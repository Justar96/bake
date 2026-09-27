/** Bake's Ink patch serializes frame rows itself; the terminal text must stay what ansi-tokenize writes. */
import { createRequire, findPackageJSON } from 'node:module'
import { pathToFileURL } from 'node:url'
import { expect, it } from 'vitest'

interface Cell { type: 'char'; value: string; fullWidth: boolean; styles: unknown[] }
interface Tokenizer {
  tokenize(text: string): unknown[]
  styledCharsFromTokens(tokens: unknown[]): Cell[]
  styledCharsToString(cells: Cell[]): string
}
interface StyledLine {
  blankCell: Cell
  styledCharsToString(cells: Cell[]): string
}

// Compare against the tokenizer Ink itself resolves; the UI package does not
// declare it. Unpatched Ink has no styled-line module, so this import fails.
const ink = pathToFileURL(createRequire(import.meta.url).resolve('ink'))
const tokenizer = await import(new URL('build/index.js', pathToFileURL(findPackageJSON('@alcalzone/ansi-tokenize', ink)!)).href) as Tokenizer
const patched = await import(new URL('styled-line.js', ink).href) as StyledLine

it('serializes styled rows to exactly the text ansi-tokenize produces', () => {
  const codes = ['\x1b[1m', '\x1b[2m', '\x1b[22m', '\x1b[3m', '\x1b[23m', '\x1b[4m', '\x1b[24m', '\x1b[7m', '\x1b[27m',
    '\x1b[31m', '\x1b[39m', '\x1b[38;2;180;184;191m', '\x1b[38;5;208m', '\x1b[41m', '\x1b[49m', '\x1b[0m']
  const glyphs = ['a', ' ', '界', '─', 'é', '😀']
  let seed = 7
  const pick = (count: number) => (seed = (seed * 1103515245 + 12345) % 2147483648) % count
  for (let row = 0; row < 5000; row++) {
    let text = ''
    for (let index = pick(24); index > 0; index--) text += pick(3) === 0 ? codes[pick(codes.length)] : glyphs[pick(glyphs.length)]
    // Output pads rows with the shared blank cell and writes styled text over them.
    const cells = [
      ...pick(2) === 0 ? [patched.blankCell] : [],
      ...tokenizer.styledCharsFromTokens(tokenizer.tokenize(text)),
      ...pick(2) === 0 ? [patched.blankCell, patched.blankCell] : [],
    ]
    expect(patched.styledCharsToString(cells), JSON.stringify(text)).toBe(tokenizer.styledCharsToString(cells))
  }
})
