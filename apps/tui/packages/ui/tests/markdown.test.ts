/** Markdown formatting, incomplete input, and terminal-safe text. */
import stringWidth from 'string-width'
import { describe, expect, test } from 'bun:test'
import { finishedMarkdown, inlineCodeColor, markdownLines, sliceSpans } from '../src/markdown.ts'
import { present } from '../src/present.ts'
import { MARKDOWN, PALETTE } from '../src/palette.ts'

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
      '3. one', '   \u25e6 nested', '4. two', '', '\u2022 [x] done', '\u2022 [ ] next', '', '\u2502 quote',
    ])
    // Numbers align on their dot once a list reaches ten.
    expect(textOf(Array.from({ length: 10 }, (_, index) => `${index + 1}. item`).join('\n')).slice(8)).toEqual([' 9. item', '10. item'])
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
    expect(answer[0]?.spans).toContainEqual({ tone: 'plain', bold: true, color: PALETTE.reference, underline: true, length: 7 })
    // Headings step down by level: underlined blue, blue, then bold at the full foreground.
    const levels = markdownLines('# One\n## Two\n### Three', 'body').map(line => line.spans?.[0])
    expect(levels).toEqual([
      { tone: 'body', bold: true, color: PALETTE.reference, underline: true, length: 3 },
      { tone: 'body', bold: true, color: PALETTE.reference, length: 3 },
      { tone: 'plain', bold: true, color: MARKDOWN.heading, length: 5 },
    ])
    expect(answer[2]?.spans).toContainEqual({ tone: 'plain', underline: true, color: PALETTE.reference, length: 4 })
    expect(answer[2]?.spans?.some(span => span.tone === 'quiet' && span.color === undefined && span.length === ' (https://example.com)'.length)).toBe(true)
    expect(answer[2]?.spans?.some(span => span.color === PALETTE.reference && span.length === 'diagram'.length)).toBe(true)
    expect(markdownLines(source, 'thought').flatMap(line => line.spans ?? []).every(span => span.color === undefined)).toBe(true)
  })

  test('draws body prose in the body tone, and bold words at the full foreground', () => {
    const [line] = markdownLines('Plain and **bold** text.', 'body')
    expect(line?.spans).toEqual([
      { tone: 'body', length: 10 }, { tone: 'plain', bold: true, length: 4 }, { tone: 'body', length: 6 },
    ])
    expect(markdownLines('a\n\n---', 'body', undefined, 20).at(-1)).toEqual({ text: '\u2500'.repeat(20), spans: [{ tone: 'quiet', length: 20 }] })
  })

  test('uses semantic accents without colouring ordinary prose or reasoning', () => {
    const source = '`value` and **strong**\n\n- [x] done\n- [ ] next\n\n> quote\n\n| Name |\n| --- |\n| plain |\n\n```ts\nvalue\n```'
    const lines = markdownLines(source)
    expect(lines[0]?.spans).toContainEqual({ tone: 'plain', bold: true, color: PALETTE.code, length: 5 })
    const task = lines.find(line => line.text === '\u2022 [x] done')!
    expect(task.spans).toEqual([
      { tone: 'plain', color: MARKDOWN.bullet, length: 2 }, { tone: 'plain', color: PALETTE.done, length: 4 }, { tone: 'plain', length: 4 },
    ])
    expect(lines.find(line => line.text === '\u2502 quote')?.spans).toEqual([{ tone: 'plain', color: MARKDOWN.quote, length: 2 }, { tone: 'plain', length: 5 }])
    expect(lines.find(line => line.text === 'Name: plain')?.spans?.[0]).toEqual({ tone: 'plain', bold: true, color: PALETTE.reference, length: 4 })
    expect(lines.find(line => line.text === 'ts')?.spans).toEqual([{ tone: 'plain', bold: true, color: MARKDOWN.literal, length: 2 }])
    expect(markdownLines(source, 'thought').flatMap(line => line.spans ?? []).every(span => span.tone === 'thought' && span.color === undefined)).toBe(true)
  })

  test('colours inline code by what it names: paths blue, literals amber, the rest lavender', () => {
    for (const path of ['src/app.ts', './bin', '~/.bake/', 'README.md', '.gitignore']) expect(inlineCodeColor(path)).toBe(PALETTE.reference)
    for (const literal of ['42', '3.5s', '100%', '"hi"', 'true', 'null']) expect(inlineCodeColor(literal)).toBe(MARKDOWN.literal)
    for (const other of ['snake_case', 'ctx.get', 'Array.from', 'bun run build', 'foo.bar()']) expect(inlineCodeColor(other)).toBe(PALETTE.code)
    // A nested quote repeats its bar in the quote's tone; reasoning keeps its own.
    const quoted = markdownLines('> one\n> two\n>\n> three')
    expect(quoted.map(line => line.spans?.[0])).toEqual(Array.from({ length: 4 }, () => ({ tone: 'plain', color: MARKDOWN.quote, length: 2 })))
    expect(markdownLines('- a\n\n> b', 'thought').flatMap(line => line.spans ?? []).every(span => span.color === undefined)).toBe(true)
  })

  test('deduplicates visible nested link labels and preserves distinct image destinations', () => {
    expect(textOf('[**https://example.com**](https://example.com) [`src/app.ts`](src/app.ts)')).toEqual(['https://example.com src/app.ts'])
    expect(textOf('[![logo](img.png)](https://example.com) ![img.png](img.png)')).toEqual(['logo (img.png) (https://example.com) img.png'])
    const codeLink = markdownLines('[`src/app.ts`](src/app.ts)')[0]!
    expect(codeLink.spans).toEqual([{ tone: 'plain', color: PALETTE.reference, underline: true, bold: true, length: 10 }])
    expect(textOf('[**a\x07**](a%07)')).toEqual(['a\\x07 (a%07)'])
  })

  test('renders tables as labeled cells without a terminal-width assumption', () => {
    expect(textOf('| Name | Value |\n| --- | --- |\n| **中文** | a long value |\n| next | `yes` |')).toEqual([
      'Name: 中文', 'Value: a long value', '', 'Name: next', 'Value: yes',
    ])
    expect(textOf('| Name | Value |\n| --- | --- |')).toEqual(['Name \u2502 Value'])
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

test('settles only a complete top-level closing fence line', () => {
  for (const source of ['```ts\nx\n```\n', '~~~~ js\nx\n~~~~~  \n', '  ```\nx\n   ```\r\n', '```\n```\n']) {
    expect(finishedMarkdown(source)).toBe(source.length)
  }
  for (const source of ['```ts\nx\n```', '```ts\nx\n```\r', '````\nx\n```\n', '```\nx\n~~~\n',
    '```\nx\n``` more\n', '    ```\n    x\n    ```\n', '> ```\n> x\n> ```\n', '- ```\n  x\n  ```\n']) {
    expect(finishedMarkdown(source)).toBe(0)
  }
})

describe('responsive tables', () => {
  const table = '| Item | Count | Description |\n| :--- | ---: | :---: |\n| **中文** | 7 | `ready` |\n| next | 120 | a longer description with 👩‍💻 |'

  test('aligns compact columns and preserves header, code, and body styles', () => {
    const lines = markdownLines('| Name | N | State |\n| :--- | --: | :---: |\n| 中文 | 7 | `yes` |\n| x | 120 | no |', 'plain', undefined, 80)
    expect(lines.map(line => line.text)).toEqual([
      'Name \u2502   N \u2502 State', '\u2500\u2500\u2500\u2500\u2500\u253c\u2500\u2500\u2500\u2500\u2500\u253c\u2500\u2500\u2500\u2500\u2500\u2500',
      '中文 \u2502   7 \u2502  yes ', 'x    \u2502 120 \u2502  no  ',
    ])
    expect(lines.every(line => line.literal)).toBe(true)
    expect(lines[0]?.spans?.[0]).toMatchObject({ color: PALETTE.reference, bold: true })
    expect(lines[2]?.spans).toContainEqual({ tone: 'plain', color: PALETTE.code, bold: true, length: 3 })
    expect(lines.flatMap(line => line.spans ?? []).some(span => span.tone === 'quiet')).toBe(true)
  })

  test('wraps Unicode cells without losing characters or styled offsets', () => {
    const lines = markdownLines(table, 'plain', undefined, 36)
    expect(lines.every(line => stringWidth(line.text) <= 36)).toBe(true)
    expect(lines.every(line => line.spans?.reduce((sum, span) => sum + span.length, 0) === line.text.length)).toBe(true)
    const lastRow = lines.filter(line => line.tableRow === table.lastIndexOf('| next'))
    const description = lastRow.map(line => line.text.split(' \u2502 ')[2]!.trim()).join(' ')
    expect(description).toBe('a longer description with 👩‍💻')
    expect(lines.some(line => line.text.includes('👩‍💻'))).toBe(true)
    expect(markdownLines(table, 'thought', undefined, 36).flatMap(line => line.spans ?? []).every(span => span.tone === 'thought' && span.color === undefined)).toBe(true)
  })

  test('carries inline code styles through hard wraps without splitting Unicode text', () => {
    const code = '界👩‍💻'.repeat(12)
    const lines = markdownLines(`| Code | Note |\n| --- | --- |\n| \`${code}\` | plain |`, 'plain', undefined, 28)
    let styled = ''
    for (const line of lines) {
      expect(stringWidth(line.text)).toBeLessThanOrEqual(28)
      let offset = 0
      for (const span of line.spans ?? []) {
        if (span.color === PALETTE.code) styled += line.text.slice(offset, offset + span.length)
        offset += span.length
      }
    }
    expect(styled).toBe(code)
  })

  test('retains source-row anchors across grid and stacked layouts', () => {
    const wide = markdownLines(table, 'plain', undefined, 80)
    const narrow = markdownLines(table, 'plain', undefined, 20)
    const offset = table.lastIndexOf('| next')
    expect(wide.filter(line => line.tableRow === offset).map(line => line.text).join('')).toContain('next')
    expect(narrow.filter(line => line.tableRow === offset).map(line => line.text)).toEqual([
      'Item: next', 'Count: 120', 'Description: a longer description with 👩‍💻',
    ])
  })

  test('keeps nested and malformed tables readable without dropping extra cells', () => {
    const malformed = '| A | B |\n| - | - |\n| one |\n| x | y | extra |'
    expect(markdownLines(malformed, 'plain', undefined, 80).map(line => line.text)).toEqual(['A: one', 'B: ', '', 'A: x', 'B: y', ': extra'])
    expect(markdownLines('> | A | B |\n> | - | - |\n> | x | y |', 'plain', undefined, 80).map(line => line.text)).toEqual(['\u2502 A: x', '\u2502 B: y'])
    expect(markdownLines('| | B |\n| - | - |\n| x | y |', 'plain', undefined, 80).map(line => line.text)).toEqual([': x', 'B: y'])
    expect(finishedMarkdown(table + '\n\n')).toBe(0)
  })
})
