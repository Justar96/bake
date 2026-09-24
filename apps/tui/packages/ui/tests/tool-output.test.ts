/** Tool source metadata, diagnostic colour, and bounded result presentation. */
import { describe, expect, test } from 'bun:test'
import { outputLines, outputSpans, toolText } from '../src/tool-output.ts'
import { ToolCards, type ToolPresenters } from '../src/cards.ts'
import { dictionaries } from '../src/copy.ts'
import { PALETTE } from '../src/palette.ts'
import { present, type Highlight, type ResultBound } from '../src/present.ts'
import type { CardLine, Row } from '../src/rows.ts'

const bound: ResultBound = { lines: 4, unit: 'lines', more: 'more lines' }
const row = (detail: readonly CardLine[], ok = true): Row => ({ kind: 'tool-call', callId: '1', tool: 'read', input: 'file.ts', result: { ok, text: '', detail } })
const result = (presentResult: NonNullable<ToolPresenters['presentResult']>) => {
  const cards = new ToolCards(() => ({ presentResult }), dictionaries.en)
  cards.call('1', 'tool', '{}')
  return cards.result('1', { content: [], isError: false })!
}

describe('tool syntax', () => {
  test('retains read hints, source prefixes, and separate fenced blocks', () => {
    const read = result(() => ({ card: 'read', path: 'no-extension', lang: 'ts', offset: 9, totalLines: 10,
      lines: [{ number: 9, text: 'const x = 1' }, { number: 10, text: 'return x' }] }))
    expect(read.detail.slice(0, 2)).toEqual([
      { text: ' 9  const x = 1', source: 'ts', codeOffset: 4, number: 9, codeStart: true },
      { text: '10  return x', source: 'ts', codeOffset: 4, number: 10 },
    ])
    const generic = result(() => ({ card: 'generic', content: [
      { type: 'text', text: '```ts\n/* one\n```' }, { type: 'text', text: '```ts\nconst two = 2\n```' },
    ] }))
    const seen: unknown[] = []
    present(row(generic.detail), { ...bound, code: (lines, language) => { seen.push([lines, language]); return undefined } })
    expect(seen).toEqual([[['/* one'], 'ts'], [['const two = 2'], 'ts']])
  })

  test('highlights consecutive source lines together and restarts at search gaps', () => {
    const detail = result(() => ({ card: 'search', shape: 'matches', total: 3, truncated: false,
      files: [{ path: 'a.ts', matches: [
        { lineNumber: 1, line: '/* start' }, { lineNumber: 2, line: 'end */' }, { lineNumber: 8, line: 'const x = 1' },
      ] }] })).detail
    const seen: unknown[] = []
    const code: Highlight = (lines, path) => {
      seen.push([lines, path])
      return lines.map(line => [{ length: line.length, color: '#123456', dim: true, italic: true }])
    }
    const rendered = present(row(detail), { ...bound, lines: 20, code })
    expect(seen).toEqual([[['/* start', 'end */'], 'a.ts'], [['const x = 1'], 'a.ts']])
    expect(rendered.filter(line => line.literal).every(line => line.spans?.every(span => span.tone === 'plain'))).toBe(true)
    expect(rendered.find(line => line.text.includes('const x'))?.spans).toEqual([
      { length: 5, tone: 'plain', color: PALETTE.reference },
      { length: 11, tone: 'plain', color: '#123456', italic: true },
    ])
  })

  test('infers complete JSON and JSONL without treating stdout as shell code', () => {
    expect(outputLines('{"ok":true,"count":3}')[0]?.source).toBe('json')
    expect(outputLines('{"n":1}\n{"n":2}').map(line => line.source)).toEqual(['json', 'json'])
    for (const text of ['{partial', 'true', 'npm test\nAll done', 'ERROR: failed']) {
      expect(outputLines(text).every(line => line.source === undefined)).toBe(true)
    }
    expect(outputLines('{"x":"' + 'a'.repeat(262_144) + '"}')[0]?.source).toBeUndefined()
  })

  test('keeps failures red and leaves unsupported grammars readable', () => {
    const detail = outputLines('{"error":"bad"}')
    let calls = 0
    const code: Highlight = () => { calls++; return undefined }
    const failure = present(row(detail, false), { ...bound, code }).slice(2)
    expect(calls).toBe(0)
    expect(failure.every(line => line.tone === 'failed' && line.spans === undefined)).toBe(true)
    const plain = present(row(outputLines('value', 'unknown-language')), { ...bound, code }).at(-1)!
    expect(plain.text).toBe('value')
    expect(plain.spans).toBeUndefined()
  })

  test('keeps a zero-line preview collapsed and reports omitted rows', () => {
    expect(present(row(outputLines('one\ntwo\nthree')), { ...bound, lines: 0 }).map(line => line.text)).toEqual(['', 'file.ts  3 lines'])
    const lines = present(row(outputLines('one\ntwo\nthree\nfour\nfive')), { ...bound, lines: 2 })
    expect(lines.map(line => line.text)).toEqual(['', 'file.ts', 'one', '+3 more lines', 'five'])
  })
})

test('colours diagnostic labels, links, and file paths without colouring ordinary prose', () => {
  const text = '2026-09-23T10:20:30Z WARN src/app.ts:12 https://example.com'
  let offset = 0
  const colored = outputSpans(text)?.flatMap(span => {
    const value = text.slice(offset, offset + span.length)
    offset += span.length
    return span.color === undefined ? [] : [[value, span.color]]
  })
  expect(colored).toEqual([['WARN', PALETTE.waiting], ['src/app.ts:12', PALETTE.reference], ['https://example.com', PALETTE.reference]])
  expect(outputSpans('This error is mentioned in ordinary prose.')).toBeUndefined()
  expect(outputSpans('PASS all checks')?.[0]?.color).toBe(PALETTE.done)
  expect(outputSpans('ERROR: failed')?.[0]?.color).toBe(PALETTE.failed)
})

test('strips tool ANSI and OSC before assigning styles and measuring text', () => {
  const raw = '\x1b[31mERROR\x1b[0m \x1b]8;;https://example.com\x07link\x1b]8;;\x07\tfile.ts\r\n\x07'
  expect(toolText(raw)).toBe('ERROR link    file.ts\n\\x07')
  expect(outputLines(raw).map(line => line.text)).toEqual(['ERROR link    file.ts', '\\x07'])
})
