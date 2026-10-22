/** Code-mode presentation uses the same logged calls and outcomes in live views and replay. */
import { describe, expect, it } from 'bun:test'
import type { SessionEvent } from 'bake-session'
import { Actions, foldEvent } from '../src/actions.ts'
import { phaseOf, turnSummary } from '../src/activity.ts'
import { fittedAction, fittedGroup, present, type ResultBound } from '../src/present.ts'
import { project, projector } from '../src/project.ts'
import type { Row, ToolCallRow } from '../src/rows.ts'
import { bound as base, codeModeTools, copy, event } from './fixtures/code-mode.ts'

const bound: ResultBound = { ...base, failures: copy.summaryFailures }
const seam = () => projector(copy, codeModeTools)
const root = (code = 'const first = await tools.read({ path: "a.ts" });\nreturn first;') => event('tool/call', {
  callId: 'script', name: 'run_code', arguments: JSON.stringify({ code, description: 'Inspect project files' }),
})
const dispatch = (type: 'tool/ptc-dispatch-start' | 'tool/ptc-dispatch', id: string, extra: object = {}) => event(type, {
  rootCallId: 'script', parentCallId: 'script', subCallId: id, name: 'read', arguments: { path: `${id}.ts` }, ...extra,
})
const ended = event('tool/result', { message: { content: [{ toolCallId: 'script', content: [{ type: 'text', text: 'All inspected' }] }] } })

function replay(events: readonly SessionEvent[]): readonly Row[] {
  const view = seam()
  const actions = new Actions()
  return [...events, event('step/end', {})].flatMap(item => foldEvent(item, project(item, view), actions))
}

describe('code-mode transcript', () => {
  it('shows a script description above literal, language-tagged source', () => {
    const [row] = project(root('const first = 1;\r\n\treturn first;\n'), seam())
    expect(row).toMatchObject({
      kind: 'tool-call', input: 'Inspect project files', detail: [
        { text: 'const first = 1;', source: 'typescript', codeStart: true },
        { text: '    return first;', source: 'typescript' },
      ],
    })
    const lines = present(row!, bound)
    expect(lines[1]?.text).toBe('Script(Inspect project files)')
    expect(lines.slice(2).map(line => [line.text, line.literal, line.tone])).toEqual([
      ['const first = 1;', true, 'plain'], ['    return first;', true, 'plain'],
    ])
  })

  it('highlights only the bounded script preview, in separate grammar runs across its gap', () => {
    const [row] = project(root(Array.from({ length: 100 }, (_, i) => `const line${i} = ${i};`).join('\n')), seam())
    const runs: string[][] = []
    const lines = present(row!, { ...bound, code: (source, language) => {
      expect(language).toBe('typescript')
      runs.push([...source])
      return source.map(line => [{ length: line.length, color: '#abcdef' }])
    } })
    expect(lines.map(line => line.text)).toEqual([
      '', 'Script(Inspect project files)', 'const line0 = 0;', 'const line1 = 1;', '+96 more lines', 'const line98 = 98;', 'const line99 = 99;',
    ])
    expect(runs).toEqual([['const line0 = 0;', 'const line1 = 1;'], ['const line98 = 98;', 'const line99 = 99;']])
    expect(lines[2]?.spans?.[0]?.color).toBe('#abcdef')
    expect(lines[4]?.literal).not.toBe(true)
  })

  it('keeps nested calls under their script, pairs out-of-order outcomes, and counts each operation once', () => {
    const events = [root(), dispatch('tool/ptc-dispatch-start', 'first'), dispatch('tool/ptc-dispatch-start', 'second'),
      dispatch('tool/ptc-dispatch', 'second', { isError: true, content: [{ type: 'text', text: 'Permission denied' }] }),
      dispatch('tool/ptc-dispatch', 'first', { isError: false, content: [{ type: 'text', text: 'Loaded first file' }] }), ended]
    const [row] = replay(events)
    expect(row?.kind).toBe('tool-call')
    const call = row as ToolCallRow
    expect(call.dispatches?.map(child => [child.callId, child.result?.ok])).toEqual([['first', true], ['second', false]])
    expect(call.result?.text).toBe('All inspected')
    const lines = present(call, bound)
    const texts = lines.map(line => line.text)
    // A call that succeeded folds to its head and size; a failure keeps its error.
    expect(texts.indexOf('Read(first.ts)  1 line')).toBeLessThan(texts.indexOf('Read(second.ts)'))
    expect(texts).not.toContain('Loaded first file')
    expect(texts.indexOf('Permission denied')).toBeLessThan(texts.indexOf(copy.scriptOutput))
    expect(texts.indexOf(copy.scriptOutput)).toBeLessThan(texts.indexOf('All inspected'))
    expect(texts.filter(text => text === 'Script(Inspect project files)  2 calls \u00b7 1 failed')).toHaveLength(1)
    const failed = lines.find(line => line.text === 'Read(second.ts)')
    expect(failed).toMatchObject({ tone: 'failed', branch: '\u2514', badge: { tone: 'failed' } })
    expect(lines.find(line => line.text === 'Permission denied')?.branch).toBe(' ')
    expect(turnSummary([call], copy, undefined).details).toBe('ran 1 · read 2 · 1 failed')
  })

  it('uses the running nested tool for the phase and preserves earlier live snapshots', () => {
    const view = seam()
    const actions = new Actions()
    const fold = (item: SessionEvent) => foldEvent(item, project(item, view), actions)
    fold(root())
    fold(dispatch('tool/ptc-dispatch-start', 'first'))
    const before = actions.pending
    expect(phaseOf(before, undefined)).toEqual({ kind: 'running', tool: 'read' })
    fold(dispatch('tool/ptc-dispatch', 'first', { isError: false, content: [{ type: 'text', text: 'Loaded first file' }] }))
    expect((before[0] as ToolCallRow).dispatches?.[0]?.result).toBeUndefined()
    expect(phaseOf(actions.pending, undefined)).toEqual({ kind: 'running', tool: 'run_code' })
    fold(ended)
    expect(phaseOf(actions.pending, undefined)).toBeUndefined()
    expect(fold(event('step/end', {}))).toEqual(replay([root(), dispatch('tool/ptc-dispatch-start', 'first'),
      dispatch('tool/ptc-dispatch', 'first', { isError: false, content: [{ type: 'text', text: 'Loaded first file' }] }), ended]))
  })

  it('keeps a caught nested failure visible even with output collapsed', () => {
    const [row] = replay([root(), dispatch('tool/ptc-dispatch-start', 'first'),
      dispatch('tool/ptc-dispatch', 'first', { isError: true, content: [{ type: 'text', text: 'Failed inside script' }] }), ended])
    const lines = present(row!, { ...bound, lines: 0 })
    expect(lines.find(line => line.text === 'Read(first.ts)')?.tone).toBe('failed')
    expect((row as ToolCallRow).result?.ok).toBe(true)
  })

  it('shows an orphan nested call rather than losing it when its parent is absent', () => {
    const [row] = replay([dispatch('tool/ptc-dispatch-start', 'first'),
      dispatch('tool/ptc-dispatch', 'first', { isError: false, content: [{ type: 'text', text: 'Loaded first file' }] })])
    expect(row).toMatchObject({ kind: 'tool-call', callId: 'first', input: 'first.ts', result: { ok: true, text: 'Loaded first file' } })
  })

  it('reconstructs completion-only dispatches in released logs and does not duplicate a late start', () => {
    const completed = dispatch('tool/ptc-dispatch', 'first', { isError: false, content: [{ type: 'text', text: 'Loaded first file' }] })
    const withoutStart = replay([root(), completed, ended])
    const lateStart = replay([root(), completed, dispatch('tool/ptc-dispatch-start', 'first'), ended])
    expect(lateStart).toEqual(withoutStart)
    expect((withoutStart[0] as ToolCallRow).dispatches).toMatchObject([
      { callId: 'first', tool: 'read', input: 'first.ts', result: { ok: true, text: 'Loaded first file' } },
    ])
    const texts = present(withoutStart[0]!, bound).map(line => line.text)
    expect(texts).toContain('Read(first.ts)  1 line')
    expect(texts).not.toContain('[first]')
  })

  it.each([false, true])('keeps a completion-only dispatch with empty content (error=%s)', isError => {
    const [row] = replay([root(), dispatch('tool/ptc-dispatch', 'first', { isError, content: [] }), ended])
    expect((row as ToolCallRow).dispatches).toMatchObject([
      { callId: 'first', tool: 'read', input: 'first.ts', result: { ok: !isError, text: '' } },
    ])
    const lines = present(row!, bound)
    expect(lines.find(line => line.text === 'Read(first.ts)')?.badge?.tone).toBe(isError ? 'failed' : 'done')
  })

  it('prints a long script with its first and last calls, its failures, and a count of the rest', () => {
    const events = [root(), ...Array.from({ length: 12 }, (_, index) => [
      dispatch('tool/ptc-dispatch-start', `f${index}`),
      dispatch('tool/ptc-dispatch', `f${index}`, { isError: index === 5, content: [{ type: 'text', text: index === 5 ? 'Permission denied' : 'Loaded' }] }),
    ]).flat(), ended]
    const [row] = replay(events)
    const texts = present(row!, bound).map(line => line.text)
    expect(texts.filter(text => text.startsWith('Read(') || text.startsWith('+'))).toEqual([
      'Read(f0.ts)  1 line', 'Read(f1.ts)  1 line', `+3 ${copy.moreCalls}`, 'Read(f5.ts)', `+4 ${copy.moreCalls}`,
      'Read(f10.ts)  1 line', 'Read(f11.ts)  1 line',
    ])
    expect(texts).toContain('Script(Inspect project files)  12 calls \u00b7 1 failed')
    // Without the phrase for the count, every call is printed.
    const whole = present(row!, { ...bound, moreCalls: undefined }).filter(line => line.text.startsWith('Read('))
    expect(whole).toHaveLength(12)
  })

  it('draws a lone folded call instead of a count of one, and formats only the calls it prints', () => {
    const [row] = project(root(), seam())
    let reads = 0
    const dispatches: ToolCallRow[] = Array.from({ length: 10_000 }, (_, index) => ({
      kind: 'tool-call', callId: `child-${index}`, tool: 'read',
      get input() { reads++; return `file-${index}.ts` },
      result: { ok: true, text: 'Loaded' },
    }))
    const texts = present({ ...row as ToolCallRow, dispatches, result: { ok: true, text: 'done' } }, bound).map(line => line.text)
    expect(texts).toContain(`+9996 ${copy.moreCalls}`)
    expect(reads).toBeLessThan(20)
    const five = present({ ...row as ToolCallRow, dispatches: dispatches.slice(0, 5), result: { ok: true, text: 'done' } }, bound)
    expect(five.filter(line => line.text.startsWith('Read('))).toHaveLength(5)
  })

  it('labels a failed script\'s result as its error', () => {
    const failed = event('tool/result', { message: { content: [{ toolCallId: 'script', isError: true, content: [{ type: 'text', text: 'Error: boom' }] }] } })
    const [row] = replay([root(), failed])
    const texts = present(row!, bound).map(line => line.text)
    expect(texts).toContain(copy.scriptError)
    expect(texts).not.toContain(copy.scriptOutput)
  })

  it('blinks a running nested call on its branch and does not count failures before the script ends', () => {
    const view = seam()
    const actions = new Actions()
    for (const item of [root(), dispatch('tool/ptc-dispatch-start', 'first'),
      dispatch('tool/ptc-dispatch', 'first', { isError: true, content: [{ type: 'text', text: 'Permission denied' }] }),
      dispatch('tool/ptc-dispatch-start', 'second')]) foldEvent(item, project(item, view), actions)
    const lines = fittedAction(actions.pending[0] as ToolCallRow, bound, 20)
    expect(lines.find(line => line.text === 'Read(second.ts)')).toMatchObject({ branch: '\u2514', badge: { pulse: true } })
    expect(lines.find(line => line.text.startsWith('Script('))?.text).toBe('Script(Inspect project files)  2 calls')
  })

  it.each([6, 40])('windows a large dispatch history before formatting a %i-row live frame', rows => {
    const [row] = project(root(), seam())
    let reads = 0
    const dispatches: ToolCallRow[] = Array.from({ length: 10_000 }, (_, index) => ({
      kind: 'tool-call', callId: `child-${index}`, tool: 'read',
      get input() { reads++; return `file-${index}.ts` },
      result: { ok: true, text: 'Loaded' },
    }))
    const lines = fittedAction({ ...row as ToolCallRow, dispatches }, bound, rows)
    expect(lines.length).toBeLessThanOrEqual(rows)
    expect(lines.some(line => line.text.startsWith('Script(Inspect project files)'))).toBe(true)
    expect(lines.some(line => line.text.startsWith('Read(file-9999.ts)'))).toBe(true)
    // Each nested call in the window is formatted a fixed number of times, not once per fold.
    expect(reads).toBeLessThan(rows * 8)
  })

  it('windows a script inside a step with other calls', () => {
    const [row] = project(root(), seam())
    let reads = 0
    const dispatches: ToolCallRow[] = Array.from({ length: 10_000 }, (_, index) => ({
      kind: 'tool-call', callId: `child-${index}`, tool: 'read',
      get input() { reads++; return `file-${index}.ts` },
      result: { ok: true, text: 'Loaded' },
    }))
    const other: ToolCallRow = { kind: 'tool-call', callId: 'other', tool: 'read', input: 'b.ts', result: { ok: true, text: 'Loaded' } }
    const lines = fittedGroup([{ ...row as ToolCallRow, dispatches }, other], bound, 12)
    expect(lines.length).toBeLessThanOrEqual(12)
    expect(lines.some(line => line.text.startsWith('Script(Inspect project files)'))).toBe(true)
    expect(lines.some(line => line.text.startsWith('Read(file-9999.ts)'))).toBe(true)
    expect(lines.some(line => line.text.startsWith('Read(b.ts)'))).toBe(true)
    expect(reads).toBeLessThan(100)
  })

  it('does not project compaction rewrites as duplicate nested effects', () => {
    const view = seam()
    for (const type of ['tool/ptc-dispatch-start', 'tool/ptc-dispatch'] as const) {
      expect(project({ ...dispatch(type, 'first'), surfaceOp: 'replace' } as SessionEvent, view)).toEqual([])
    }
  })
})
