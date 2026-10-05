/** Tool calls headed by their raw arguments or a presenter's card, drawn without colour at two widths. */
import React from 'react'
import { renderToString } from 'ink'
import stringWidth from 'string-width'
import { expect, it } from 'vitest'
import type { SessionEvent } from 'bake-session'
import type { ToolPresenters } from '../src/cards.ts'
import { RowView } from '../src/app.tsx'
import { dictionaries } from '../src/copy.ts'
import { budgetFor } from '../src/layout.ts'
import type { ResultBound } from '../src/present.ts'
import { project, projector } from '../src/project.ts'
import { Actions } from '../src/actions.ts'

/** Partial envelopes keep these presentation fixtures independent of persistence metadata. */
const event = (value: unknown): SessionEvent => value as SessionEvent

const call = (callId: string, name: string, args: string): SessionEvent =>
  event({ type: 'tool/call', data: { callId, name, arguments: args } })
const result = (callId: string, text: string, isError = false): SessionEvent =>
  event({ type: 'tool/result', data: { message: { content: [{ toolCallId: callId, isError, content: [{ type: 'text', text }] }] } } })

const questions = [
  { id: 'scope', header: 'Choose scope',
    question: 'Which parts of the migration should I include in this pass, given the constraints we discussed?',
    multi_select: true, options: [
      { label: 'Tooling swaps', description: 'Replace the remaining pnpm invocations in scripts and CI with bun equivalents.' },
      { label: 'Shared versions', description: 'Pin shared dependency versions in the root catalog.' },
    ] },
  { id: 'fixes', header: 'Runtime fixes', question: 'Should the runtime fixes land in the same commit?',
    options: [{ label: 'Same commit (Recommended)' }, { label: 'Separate commit' }] },
]
const answers = JSON.stringify({ answers: [
  { id: 'scope', selected: ['Tooling swaps', 'Shared versions'], custom: 'skip CI for now' },
  { id: 'fixes', selected: ['Separate commit'] },
] })
const records = JSON.stringify(Array.from({ length: 60 }, (_, index) => ({ id: index, name: `record-${index}`, tags: ['a', 'b'] })))

const events: readonly SessionEvent[] = [
  // A tool with no card, whose arguments hold what `ask_user_question` sends.
  call('c1', 'mcp_ask', JSON.stringify({ questions })),
  result('c1', answers),
  // The same call through presenters shaped as the tool's own.
  call('c2', 'ask_user_question', JSON.stringify({ questions })),
  result('c2', answers),
  // A primary field, cut on its first line.
  call('c3', 'mcp_lookup', JSON.stringify({ query: `callers of the session reader ${'and its helpers '.repeat(10)}`, limit: 5 })),
  result('c3', records),
  // Wide text, measured in cells.
  call('c4', 'mcp_note', JSON.stringify({ title: '\u4e2d\u6587\u6807\u9898'.repeat(20), body: 'e\u0301'.repeat(80) })),
  result('c4', 'ok'),
  // Malformed arguments stay as sent, up to the bound.
  call('c5', 'mcp_broken', `{"items": ${'"entry", '.repeat(40)}`),
  result('c5', 'invalid arguments: unexpected end of JSON input', true),
]

/** Presenters shaped as `ask_user_question` declares them; the tool's own spec pins their words. */
const ask: ToolPresenters = {
  presentCall: () => ({ card: 'generic', title: 'Ask 2 questions: Choose scope, Runtime fixes', kind: 'other' }),
  presentResult: (_args, outcome) => outcome.isError ? undefined : ({ card: 'generic', content: [{ type: 'text',
    text: 'scope \u2192 Tooling swaps, Shared versions, "skip CI for now"\nfixes \u2192 Separate commit' }] }),
}

function draw(columns: number): string {
  const seam = projector(dictionaries.en, name => name === 'ask_user_question' ? ask : undefined)
  const actions = new Actions()
  // Each result ends its own step, so every call draws as its own block.
  const rows = events.flatMap(item => [...actions.fold(project(item, seam)), ...item.type === 'tool/result' ? actions.fold([], true) : []])
  const budget = budgetFor({ columns, rows: 24 })
  // The application's default preview.
  const { cardLines: unit, cardLine: single, moreLines: more } = dictionaries.en
  const bound: ResultBound = { lines: 4, unit, single, more }
  return renderToString(<>{rows.map((row, index) => <RowView key={index} row={row} budget={budget} frame="classic" result={bound} />)}</>, { columns })
}

it.each([40, 80])('heads every call in a line or two and never prints its arguments as JSON at %i columns', async columns => {
  const frame = draw(columns)
  const heads = frame.split('\n').filter(line => line.startsWith('\u25cf '))
  expect(heads).toHaveLength(5)
  expect(frame).not.toContain('"options"')
  expect(frame).not.toContain('"question"')
  expect(frame).toContain('questions: [scope, +1]')
  expect(frame).toContain('scope \u2192 Tooling swaps')
  expect(frame).toContain('invalid arguments')
  expect(frame.split('\n').every(line => stringWidth(line) <= columns)).toBe(true)
  await expect(frame + '\n').toMatchFileSnapshot(`./expected/tool-headline.${columns}.txt`)
})
