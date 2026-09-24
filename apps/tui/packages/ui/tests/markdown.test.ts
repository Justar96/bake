/** Markdown formatting, incomplete input, and terminal-safe text. */
import { describe, expect, test } from 'bun:test'
import { finishedMarkdown, markdownLines, sliceSpans } from '../src/markdown.ts'
import { present } from '../src/present.ts'
import { PALETTE } from '../src/palette.ts'

const textOf = (source: string) => markdownLines(source).map(line => line.text)

describe('Markdown', () => {
  test('formats nested emphasis, escapes, code, links, and Unicode without changing source', () => {
    const source = '# Ready\n\n**bold *中文*** and ~~old~~ with `a_b` and \\*literal\\*.\n[docs](https://example.com)'
    expect(textOf(source)).toEqual(['Ready', '', 'bold 中文 and old with a_b and *literal*.', 'docs (https://example.com)'])
    const line = markdownLines(source)[2]!
    expect(line.spans).toContainEqual({ tone: 'plain', bold: true, italic: true, length: 2 })
    expect(line.spans).toContainEqual({ tone: 'plain', strikethrough: true, length: 3 })
    expect(markdownLines(source)[3]!.spans?.[0]?.underline).toBe(true)
  })

  test('keeps nested lists, task states, quotes, and code whitespace readable', () => {
    expect(textOf('3. one\n   - nested\n4. two\n\n- [x] done\n- [ ] next\n\n> quote')).toEqual([
      '3. one', '   - nested', '4. two', '', '- [x] done', '- [ ] next', '', '> quote',
    ])
    const code = markdownLines('```ts\n  const snake_case = "**literal**"\n\n\treturn snake_case\n```')
    expect(code.map(line => line.text)).toEqual(['ts', '    const snake_case = "**literal**"', '  ', '      return snake_case'])
    expect(code.slice(1).every(line => line.literal)).toBe(true)
  })

  test('uses the existing highlighter and keeps every reasoning span in its tone', () => {
    const seen: unknown[] = []
    const lines = markdownLines('**Plan**\n\n```ts\nconst x = 1\n```', 'thought', (lines, path) => {
      seen.push([lines, path])
      return [[{ length: 5, color: '#abcdef' }]]
    })
    expect(seen).toEqual([[['const x = 1'], 'ts']])
    expect(lines.flatMap(line => line.spans ?? []).every(span => span.tone === 'thought')).toBe(true)
    expect(lines.at(-1)?.spans).toContainEqual({ length: 5, tone: 'thought', color: '#abcdef' })
  })

  test('colours answer headings and references, but leaves reasoning in its own tone', () => {
    const source = '# Context\n\nSee [docs](https://example.com) and ![diagram](img.png).'
    const answer = markdownLines(source)
    expect(answer[0]?.spans).toContainEqual({ tone: 'plain', bold: true, color: PALETTE.reference, length: 7 })
    expect(answer[2]?.spans).toContainEqual({ tone: 'plain', underline: true, color: PALETTE.reference, length: 4 })
    expect(answer[2]?.spans?.some(span => span.color === PALETTE.reference && span.length === ' (https://example.com)'.length)).toBe(true)
    expect(answer[2]?.spans?.some(span => span.color === PALETTE.reference && span.length === 'diagram (img.png)'.length)).toBe(true)
    expect(markdownLines(source, 'thought').flatMap(line => line.spans ?? []).every(span => span.color === undefined)).toBe(true)
  })

  test('renders tables as labeled cells without a terminal-width assumption', () => {
    expect(textOf('| Name | Value |\n| --- | --- |\n| **中文** | a long value |\n| next | `yes` |')).toEqual([
      'Name: 中文', 'Value: a long value', '', 'Name: next', 'Value: yes',
    ])
    expect(textOf('| Name | Value |\n| --- | --- |')).toEqual(['Name | Value'])
  })

  test('keeps unfinished constructs and literal references readable', () => {
    expect(textOf('**not closed')).toEqual(['**not closed'])
    expect(textOf('[label](unfinished')).toEqual(['[label](unfinished'])
    expect(textOf('```js\nconst x = `unfinished')).toEqual(['js', '  const x = `unfinished'])
    expect(textOf('[docs][ref]\n\n[ref]: https://example.com')).toEqual(['[docs][ref]', '', '[ref]: https://example.com'])
    expect(textOf('<b>literal</b>')).toEqual(['<b>literal</b>'])
  })

  test('escapes raw and entity-encoded terminal controls', () => {
    const lines = markdownLines('hello\x1b[2J\r\n&#27;[31m\x07\u009b2J\t世界')
    expect(lines.map(line => line.text)).toEqual(['hello\\x1b[2J', '�[31m\\x07\\x9b2J    世界'])
    expect(lines.some(line => /[\x00-\x1f\x7f-\x9f]/.test(line.text))).toBe(false)
  })

  test('does not apply Markdown to user text or tool output', () => {
    const bound = { lines: 3, unit: 'lines', more: 'more lines' }
    expect(present({ kind: 'user', text: '**literal**' }, bound).at(-1)?.text).toBe('**literal**')
    expect(present({ kind: 'tool-result', callId: '1', ok: true, text: '**literal**' }, bound).at(-1)?.text).toBe('**literal**')
  })

  test('slices emphasis at UTF-16 boundaries for wrapped reasoning', () => {
    expect(sliceSpans([{ length: 4, tone: 'thought' }, { length: 6, tone: 'thought', bold: true }], 2, 7)).toEqual([
      { length: 2, tone: 'thought' }, { length: 3, tone: 'thought', bold: true },
    ])
  })
})

describe('stable Markdown prefix', () => {
  test('waits for block context and does not cut an open fence, list, or table', () => {
    for (const source of ['hello\n', 'title\n---', '```ts\nconst x = 1\n\n', '- one\n\n', '| a |\n|---|\n| b |\n\n']) {
      expect(finishedMarkdown(source)).toBe(0)
    }
    expect(finishedMarkdown('hello\n\n')).toBe(6)
    expect(finishedMarkdown('# title\n')).toBe(8)
    expect(finishedMarkdown('title\n---\n')).toBe(10)
    const source = '```ts\nconst x = 1\n```\n\nnext'
    expect(source.slice(0, finishedMarkdown(source))).toBe('```ts\nconst x = 1\n```\n')
    expect(finishedMarkdown('hello\r\n\r\n')).toBe(7)
  })
})
