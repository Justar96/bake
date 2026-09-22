/** Tool identity and turn endings in narrow, color-independent transcripts. */
import React from 'react'
import { renderToString } from 'ink'
import { describe, expect, it } from 'vitest'
import type { SessionEvent } from '@deepseek-ai/dsh-session'
import { RowView } from '../src/app.tsx'
import { dictionaries } from '../src/copy.ts'
import { budgetFor } from '../src/layout.ts'
import type { ResultBound } from '../src/present.ts'
import { project, projector } from '../src/project.ts'
import { Actions } from '../src/actions.ts'

/** Partial envelopes keep these presentation fixtures independent of persistence metadata. */
const event = (value: unknown): SessionEvent => value as SessionEvent

const events = [
  event({ type: 'user/message', data: { source: { kind: 'user' }, content: [{ type: 'text', text: 'Read both files, then summarize.' }] } }),
  event({ type: 'tool/call', data: { callId: 'c1', name: 'read_file', arguments: '{"path":"notes.md"}' } }),
  event({ type: 'tool/call', data: { callId: 'c2', name: 'read_file', arguments: '{"path":"private.md"}' } }),
  event({ type: 'tool/result', data: { message: { content: [{ toolCallId: 'c2', isError: true, content: 'Permission denied' }] } } }),
  event({ type: 'tool/result', data: { message: { content: [{ toolCallId: 'c1', content: 'Notes loaded.' }] } } }),
  event({ type: 'assistant/message', data: { message: { content: [{ type: 'text', text: 'Read notes.md; private.md was unavailable.' }] } } }),
  event({ type: 'turn/end', data: { turn: 1, reason: { kind: 'completed' } } }),
  event({ type: 'user/message', data: { source: { kind: 'user' }, content: [{ type: 'text', text: 'Try again.' }] } }),
  event({ type: 'turn/end', data: { turn: 2, reason: { kind: 'aborted', reason: { kind: 'user' } } } }),
]

describe('action transcript', () => {
  it.each(['en', 'zh'] as const)('pairs out-of-order results with their calls and separates turn outcomes (%s)', async locale => {
    const seam = projector(dictionaries[locale], () => undefined)
    const actions = new Actions()
    const rows = events.flatMap(item => actions.fold(project(item, seam), item.type === 'assistant/message' || item.type === 'turn/end'))
    const budget = budgetFor({ columns: 40, rows: 24 })
    // The application's default preview.
    const committed: ResultBound = { lines: 4, unit: dictionaries[locale].cardLines, more: dictionaries[locale].moreLines }
    const frame = renderToString(<>{rows.map((row, index) => <RowView key={index} row={row} budget={budget} result={committed} />)}</>, { columns: 40 })
    // Each result lands under its own call, and the calls keep the order the
    // model made them, though c2 finished first.
    expect(frame.indexOf('notes.md')).toBeLessThan(frame.indexOf('private.md'))
    expect(frame.indexOf('private.md')).toBeLessThan(frame.indexOf('Permission denied'))
    expect(frame).not.toContain('[c1]')
    // A completed turn says so on the summary row above the input instead.
    expect(frame).not.toContain(`- ${dictionaries[locale].turnCompleted}`)
    expect(frame).toContain(`- ${dictionaries[locale].cancelled}`)
    expect(frame).not.toContain(`${dictionaries[locale].turn} 1:`)
    expect(frame).not.toContain(`${dictionaries[locale].turn} 2:`)
    await expect(frame + '\n').toMatchFileSnapshot(`./expected/actions.${locale}.txt`)
  })
})
