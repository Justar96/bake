/** Responsive table anchors retain the source row through width and live-content changes. */
import { expect, test } from 'bun:test'
import { budgetFor } from '../src/layout.ts'
import { present } from '../src/present.ts'
import type { Row } from '../src/rows.ts'
import { emptyTranscript } from '../src/transcript.ts'
import { Viewport } from '../src/viewport.ts'

const result = { lines: 3, unit: 'lines', more: 'more lines' }
const wide = budgetFor({ columns: 80, rows: 24 })
const narrow = budgetFor({ columns: 24, rows: 24 })
const text = '| Item | Count | Description |\n| --- | ---: | --- |\n'
  + Array.from({ length: 30 }, (_, index) => `| row_${index} | ${index} | entry_${index} |`).join('\n')

test('retains the viewed row across grid, stacked, and subsequent unchanged-width renders', () => {
  const viewport = new Viewport('session')
  const row: Row = { kind: 'assistant', text }
  viewport.update(emptyTranscript, [row])
  viewport.configure(wide, result)
  const offset = present(row, result, undefined, wide.measure).findIndex(line => line.text.includes('row_15'))
  const anchor = viewport.anchor({ row: 1, offset }, wide, result)
  viewport.configure(narrow, result)
  const top = viewport.locate(anchor, narrow, result)
  expect(viewport.window(top, 1, narrow, result)[0]?.line.text).toBe('Item: row_15')
  const held = { ...anchor, ...top, columns: narrow.columns }
  expect(viewport.window(viewport.locate(held, narrow, result), 1, narrow, result)[0]?.line.text).toBe('Item: row_15')
  viewport.configure(wide, result)
  expect(viewport.window(viewport.locate(held, wide, result), 1, wide, result)[0]?.line.text).toContain('row_15')
})

test('retains the viewed live row when appended cells redistribute columns at the same width', () => {
  const viewport = new Viewport('session')
  const budget = budgetFor({ columns: 42, rows: 24 })
  const row: Row = { kind: 'assistant', text: text.replaceAll('entry_', 'a description entry_') }
  viewport.update(emptyTranscript, [row])
  viewport.configure(budget, result)
  const lines = present(row, result, undefined, budget.measure)
  const offset = lines.findIndex(line => line.text.includes('row_15'))
  const anchor = viewport.anchor({ row: 1, offset }, budget, result)
  viewport.update(emptyTranscript, [{ kind: 'assistant', text: row.text + '\n| a_long_item_identifier | 100 | final |' }])
  const top = viewport.locate(anchor, budget, result)
  expect(viewport.window(top, 1, budget, result)[0]?.line.text).toContain('row_15')
})
