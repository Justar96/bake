/** Source whitespace, nested outcomes, and live-window bounds through the product's Ink renderer. */
import React from 'react'
import { renderToString } from 'ink'
import { describe, expect, it } from 'vitest'
import { Actions, foldEvent } from '../src/actions.ts'
import { budgetFor } from '../src/layout.ts'
import { Line, LiveRegion, wrappedRows } from '../src/line.tsx'
import { fittedAction, present } from '../src/present.ts'
import { project, projector } from '../src/project.ts'
import type { ToolCallRow } from '../src/rows.ts'
import { bound, codeModeTools, copy, event } from './fixtures/code-mode.ts'

function script(): ToolCallRow {
  const seam = projector(copy, codeModeTools)
  const actions = new Actions()
  const events = [
    event('tool/call', { callId: 'root', name: 'run_code', arguments: JSON.stringify({ description: 'Inspect project files', code: [
      'const paths = ["first.ts", "private.ts"];',
      'for (const path of paths) {',
      '  const file = await tools.read({ path });',
      '  console.log(file);',
      '}',
      'return "Inspection complete";',
    ].join('\n') }) }),
    ...['first', 'private'].map(id => event('tool/ptc-dispatch-start', {
      rootCallId: 'root', parentCallId: 'root', subCallId: id, name: 'read', arguments: { path: `${id}.ts` },
    })),
    ...['private', 'first'].map(id => event('tool/ptc-dispatch', {
      rootCallId: 'root', parentCallId: 'root', subCallId: id, name: 'read', arguments: { path: `${id}.ts` },
      isError: id === 'private', content: [{ type: 'text', text: id === 'private' ? 'Permission denied' : 'First file loaded' }],
    })),
    event('tool/result', { message: { content: [{ toolCallId: 'root', content: [{ type: 'text', text: 'Inspection complete' }] }] } }),
    event('step/end', {}),
  ]
  return events.flatMap(item => foldEvent(item, project(item, seam), actions))[0] as ToolCallRow
}

describe('code-mode rendering', () => {
  it.each([40, 80])('renders source and nested outcomes without a JSON input dump at %i columns', async columns => {
    const budget = budgetFor({ columns, rows: 24 })
    const lines = present(script(), bound, undefined, budget.measure)
    const frame = renderToString(<>{lines.map((line, index) => <Line key={index} line={line} budget={budget} frame="classic" />)}</>, { columns })
    expect(frame).toContain('Script(Inspect project files)')
    expect(frame).toContain('Read(first.ts)')
    expect(frame).toContain('Read(private.ts)')
    expect(frame).toContain('Permission denied')
    expect(frame).toContain('Inspection complete')
    expect(frame).not.toContain('"description":')
    expect(frame).not.toContain('\\n')
    expect(frame.split('\n').every(line => line.length <= columns)).toBe(true)
    await expect(frame + '\n').toMatchFileSnapshot(`./expected/code-mode-${columns}.txt`)
  })

  it.each([40, 80])('prints a long script as its first and last calls, its failure, and counts at %i columns', async columns => {
    const seam = projector(copy, codeModeTools)
    const actions = new Actions()
    const reads = Array.from({ length: 12 }, (_, index) => `src/m${index}.ts`)
    const events = [
      event('tool/call', { callId: 'root', name: 'run_code', arguments: JSON.stringify({ description: 'Find TODOs', code: [
        'for (const path of paths) {',
        '  const text = await tools.read({ path });',
        '}',
      ].join('\n') }) }),
      ...reads.flatMap((path, index) => [
        event('tool/ptc-dispatch-start', { rootCallId: 'root', parentCallId: 'root', subCallId: path, name: 'read', arguments: { path } }),
        event('tool/ptc-dispatch', { rootCallId: 'root', parentCallId: 'root', subCallId: path, name: 'read', arguments: { path },
          isError: index === 5, content: [{ type: 'text', text: index === 5 ? 'Permission denied' : 'export const x = 1\n// TODO' }] }),
      ]),
      event('tool/result', { message: { content: [{ toolCallId: 'root', content: [{ type: 'text', text: '["src/m3.ts"]' }] }] } }),
      event('step/end', {}),
    ]
    const row = events.flatMap(item => foldEvent(item, project(item, seam), actions))[0] as ToolCallRow
    const budget = budgetFor({ columns, rows: 24 })
    const lines = present(row, bound, undefined, budget.measure)
    const frame = renderToString(<>{lines.map((line, index) => <Line key={index} line={line} budget={budget} frame="classic" />)}</>, { columns })
    expect(lines[1]?.text).toBe('Script(Find TODOs)  12 calls \u00b7 1 failed')
    expect(frame).toContain(`\u251c +3 ${copy.moreCalls}`)
    expect(frame).toContain('\u2502 \u23bf    Permission denied')
    expect(frame).toContain('\u2514 \u25cf Read(src/m11.ts)')
    expect(frame.split('\n').every(line => line.length <= columns)).toBe(true)
    await expect(frame + '\n').toMatchFileSnapshot(`./expected/code-mode-long-${columns}.txt`)
  })

  it.each([40, 80])('keeps the script head and latest nested activity in a short live window at %i columns', columns => {
    const { result: _result, ...running } = script()
    const row: ToolCallRow = { ...running, dispatches: Array.from({ length: 20 }, (_, index) => ({
      kind: 'tool-call', callId: `nested-${index}`, tool: 'read', input: `file-${index}.ts`,
      ...index === 19 ? {} : { result: { ok: true, text: 'Read a long file\n'.repeat(10) } },
    })) }
    const budget = budgetFor({ columns, rows: 12 })
    const height = (line: Parameters<typeof wrappedRows>[0]) => wrappedRows(line, budget).length
    for (const limit of [1, 2, 3, 6]) {
      const lines = fittedAction(row, bound, limit, height, budget.output)
      expect(lines.reduce((sum, line) => sum + height(line), 0)).toBeLessThanOrEqual(limit)
      expect(lines.some(line => line.text.startsWith('Script(Inspect project files)'))).toBe(true)
    }
    const frame = renderToString(<LiveRegion rows={[row]} budget={budget} frame="classic" limit={6} result={bound} />, { columns })
    expect(frame).toContain('Script(Inspect project files)')
    expect(frame).toContain('Read(file-19.ts)')
    expect(frame).not.toContain('const paths')
  })
})
