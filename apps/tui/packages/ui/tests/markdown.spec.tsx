/** Terminal rows and emphasis for formatted responses and reasoning. */
import React from 'react'
import { renderToString } from 'ink'
import { execFileSync } from 'node:child_process'
import stringWidth from 'string-width'
import { expect, it } from 'vitest'
import { RowView } from '../src/app.tsx'
import { budgetFor } from '../src/layout.ts'
import { present } from '../src/present.ts'
import { wrappedRows } from '../src/line.tsx'

const source = '# Result\n\n**Updated** the `snake_case` helper with *中文 support*.\n\n'
  + '1. Keep input intact.\n2. Check [docs](https://example.com/docs).\n\n'
  + '> A quote with **emphasis**.\n\n```ts\nconst snake_case = "**literal**"\n  return snake_case\n```\n\n'
  + '| Check | Result |\n| --- | --- |\n| Width | wraps without losing text |\n| State | **ready** |'
const result = { lines: 3, unit: 'lines', more: 'more lines' }

it.each([40, 80])('renders Markdown at %i columns with a bounded reasoning preview', async columns => {
  const budget = budgetFor({ columns, rows: 24 })
  const frame = renderToString(<>
    <RowView row={{ kind: 'reasoning', text: '**Check** the formatter across widths and streamed chunks.\n\nThen verify the output.\nAnd replay it.' }} budget={budget} result={result} />
    <RowView row={{ kind: 'assistant', text: source }} budget={budget} result={result} />
  </>, { columns })
  expect(frame.split('\n').every(line => stringWidth(line) <= columns)).toBe(true)
  expect(frame).not.toContain('```')
  expect(frame).toContain('**literal**')
  expect(frame).toContain('Check: Width')
  await expect(frame + '\n').toMatchFileSnapshot(`./expected/markdown.${columns}.txt`)
})

it.each([20, 40, 80, 120])('keeps formatted headings, links, and table cells within %i columns', columns => {
  const budget = budgetFor({ columns, rows: 24 })
  const frame = renderToString(<RowView row={{ kind: 'assistant', text: source }} budget={budget} result={result} />, { columns })
  expect(frame.split('\n').every(line => stringWidth(line) <= columns)).toBe(true)
  expect(frame).toContain('Result')
  expect(frame).toContain('Check: Width')
  expect(frame.replace(/\n\s*/g, '')).toContain('https://example.com/docs')
})

it('slices reasoning styles at the same offsets as physical wrapping', () => {
  const budget = budgetFor({ columns: 40, rows: 24 })
  const word = 'x'.repeat(budget.measure)
  const lines = present({ kind: 'reasoning', text: `**${word} bold continuation** and plain tail that fills more rows\nlast` },
    { ...result, lines: 2 }, line => wrappedRows(line, budget))
  const body = lines.slice(1, -1)
  expect(body.map(line => line.text)).toEqual([word, 'bold continuation and plain tail that'])
  const second = body[1]!
  expect(second.spans?.[0]).toEqual({ tone: 'thought', bold: true, length: 'bold continuation'.length })
  expect(second.spans?.reduce((sum, span) => sum + span.length, 0)).toBe(second.text.length)
})

it('keeps reasoning dim through inline styles', async () => {
  const env: NodeJS.ProcessEnv = { ...process.env, FORCE_COLOR: '3', COLORTERM: 'truecolor' }
  delete env.NO_COLOR
  const frame = execFileSync(process.execPath, ['--import', 'tsx/esm', '--input-type=module', '--eval', `
    import React from 'react';
    import { renderToString } from 'ink';
    import { RowView } from ${JSON.stringify(new URL('../src/app.tsx', import.meta.url).href)};
    import { budgetFor } from ${JSON.stringify(new URL('../src/layout.ts', import.meta.url).href)};
    const budget = budgetFor({ columns: 80, rows: 24 });
    const result = { lines: 9, unit: 'lines', more: 'more lines' };
    const text = '**Bold** and *italic* with ~~old~~ and [docs](https://example.com).';
    process.stdout.write(renderToString(React.createElement(React.Fragment, null,
      ...['reasoning', 'assistant'].map(kind => React.createElement(RowView, { row: { kind, text }, budget, result }))), { columns: 80 }));
  `], { cwd: new URL('../../../../../', import.meta.url), env, encoding: 'utf8', timeout: 20_000 })
  expect(frame).toContain('\x1b[1mBold\x1b[22m')
  expect(frame).toContain('\x1b[4mdocs\x1b[24m')
  await expect(frame.replaceAll('\x1b', '<ESC>') + '\n').toMatchFileSnapshot('./expected/markdown.styles.txt')
})
