/** Streaming reasoning is drawn whole while it is newest and folds once a later row follows. */
import { expect, test } from 'bun:test'
import { budgetFor } from '../src/layout.ts'
import { lineHeight } from '../src/line.tsx'
import { present, streamingThought, tailLines } from '../src/present.ts'
import type { Row } from '../src/rows.ts'
import { emptyTranscript } from '../src/transcript.ts'
import { Viewport } from '../src/viewport.ts'

const result = { lines: 3, unit: 'lines', more: 'more lines' }

test('fullscreen draws the newest reasoning whole, then its preview once the answer starts', () => {
  const budget = budgetFor({ columns: 80, rows: 40 }, { fullscreen: true })
  const thought: Row = { kind: 'reasoning', text: Array.from({ length: 12 }, (_, index) => `line ${index + 1}`).join('\n') }
  const drawn = (viewport: Viewport): string[] =>
    viewport.window({ row: 1, offset: 0 }, 40, budget, result).map(visible => visible.line.text)
  const viewport = new Viewport('session')
  viewport.configure(budget, result)
  viewport.update(emptyTranscript, [thought])
  expect(drawn(viewport).filter(text => text.startsWith('line '))).toHaveLength(12)
  viewport.update(emptyTranscript, [thought, { kind: 'assistant', text: 'Done.' }])
  const folded = drawn(viewport)
  expect(folded.filter(text => text.startsWith('line '))).toEqual(['line 1', 'line 2', 'line 3'])
  expect(folded).toContain('+9 more lines')
})

test('scrolls a long thought by rows rather than dropping whole paragraphs', () => {
  const budget = budgetFor({ columns: 40, rows: 24 })
  const height = (line: Parameters<typeof lineHeight>[0]): number => lineHeight(line, budget)
  const text = Array.from({ length: 6 }, (_, index) => `Paragraph ${index} runs long enough to wrap onto a second row here.`).join('\n\n')
  const shown = tailLines(present({ kind: 'reasoning', text }, result, undefined, budget.measure), 7, height)
  expect(shown.reduce((sum, line) => sum + height(line), 0)).toBe(7)
})

test('draws the window the whole thought would, from its newest paragraphs alone', () => {
  const budget = budgetFor({ columns: 26, rows: 24 })
  const height = (line: Parameters<typeof lineHeight>[0]): number => lineHeight(line, budget)
  const parts = [
    'Plain paragraph that runs long enough to wrap onto a second row.',
    '```ts\nconst a = 1\n\nconst b = 2\n```',
    '- first item\n- second item',
    '## Heading',
    '---',
    '~~~\nfenced\n\n\nwith gaps\n~~~',
    'Short.',
    '> quoted line',
    '1. one\n2. two',
  ]
  for (let size = 1; size <= 40; size++) {
    const whole = Array.from({ length: size }, (_, index) => parts[(index * 7) % parts.length]).join('\n\n')
    // Mid-stream too, including cuts inside an unclosed fence.
    for (const source of [whole, whole.slice(0, Math.floor(whole.length * 0.6))]) {
      const row = { kind: 'reasoning', text: source } as const
      for (const limit of [1, 3, 8, 20]) {
        const expected = tailLines(present(row, result, undefined, budget.measure), limit, height).map(line => line.text)
        const actual = tailLines(streamingThought(row, result, limit, height, budget.measure), limit, height).map(line => line.text)
        expect(actual, `${size} ${limit}`).toEqual(expected)
      }
    }
  }
})
