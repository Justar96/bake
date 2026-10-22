/** Running calls' live output and finish, projected over the live rows and drawn, under `bun test` because both are pure. */

import { describe, expect, it } from 'bun:test'
import type { SessionEvent } from 'bake-session'
import { Actions, foldEvent } from '../src/actions.ts'
import { fittedGroup, present, type ResultBound } from '../src/present.ts'
import { CallProgress, LIVE_TAIL_LINES, outputTail } from '../src/progress.ts'
import type { Row, ToolCallRow } from '../src/rows.ts'

const bound: ResultBound = { lines: 3, unit: 'lines', more: 'more lines', failures: 'failed' }
const call = (callId: string): ToolCallRow => ({ kind: 'tool-call', callId, tool: 'bash', input: `cmd ${callId}` })
const event = (partial: unknown): SessionEvent => partial as SessionEvent
const resultEvent = (callId: string): SessionEvent =>
  event({ type: 'tool/result', data: { message: { source: { kind: 'tool', callId } } } })

describe('outputTail', () => {
  it('splits lines, keeps what a carriage return rewrote last, and bounds the count', () => {
    expect(outputTail('one\r\ntwo\n')).toEqual(['one', 'two'])
    expect(outputTail('10%\r50%\r90%\rdone\npartial')).toEqual(['done', 'partial'])
    expect(outputTail('a\rb\r\n')).toEqual(['b'])
    const many = Array.from({ length: LIVE_TAIL_LINES + 5 }, (_, index) => `line ${index}`).join('\n')
    const tail = outputTail(many)
    expect(tail).toHaveLength(LIVE_TAIL_LINES)
    expect(tail.at(-1)).toBe(`line ${LIVE_TAIL_LINES + 4}`)
  })
})

describe('CallProgress', () => {
  it('lays live state over waiting calls only, inside a group as well', () => {
    const progress = new CallProgress()
    expect(progress.decorate([call('a')])).toEqual([call('a')])
    progress.progress('a', 'first\nsecond\n')
    progress.finished('b', false)
    const done: ToolCallRow = { ...call('c'), result: { ok: true, text: 'out' } }
    progress.progress('c', 'stale')
    const rows: Row[] = [{ kind: 'assistant', text: 'hi' }, { kind: 'tool-group', calls: [call('a'), call('b'), done] }]
    expect(progress.decorate(rows)).toEqual([
      { kind: 'assistant', text: 'hi' },
      { kind: 'tool-group', calls: [
        { ...call('a'), live: { tail: ['first', 'second'] } },
        { ...call('b'), live: { finished: { ok: false } } },
        done,
      ] },
    ])
  })

  it('keeps a finished call\'s tail, and a newer snapshot replaces the last', () => {
    const progress = new CallProgress()
    progress.progress('a', 'one\n')
    progress.progress('a', 'one\ntwo\n')
    progress.finished('a', true)
    expect(progress.decorate([call('a')])).toEqual([{ ...call('a'), live: { tail: ['one', 'two'], finished: { ok: true } } }])
  })

  it('drops a call\'s state at its logged result, and every call\'s at the step end', () => {
    const progress = new CallProgress()
    progress.progress('a', 'x')
    progress.progress('b', 'y')
    progress.fold(resultEvent('a'))
    expect(progress.decorate([call('a'), call('b')])).toEqual([call('a'), { ...call('b'), live: { tail: ['y'] } }])
    progress.fold(event({ type: 'step/end', data: {} }))
    expect(progress.any).toBe(false)
  })

  it('never reaches the committed transcript', () => {
    const actions = new Actions()
    const progress = new CallProgress()
    foldEvent(event({ type: 'tool/call', data: {} }), [call('a')], actions)
    progress.progress('a', 'live only')
    expect(progress.decorate(actions.live())).toEqual([{ ...call('a'), live: { tail: ['live only'] } }])
    const committed = foldEvent(event({ type: 'step/end', data: {} }), [], actions)
    expect(committed).toEqual([call('a')])
  })
})

describe('drawing live state', () => {
  it('hangs the newest output lines under a running call, cut to one row each', () => {
    const lines = present({ ...call('a'), live: { tail: ['l1', 'l2', 'l3', 'l4', `long ${'x'.repeat(80)}`] } }, bound, undefined, 30)
    const head = lines.find(line => line.text.startsWith('Bash('))!
    expect(head.pulse).toBe(true)
    const body = lines.filter(line => line.zone === true).map(line => line.text)
    expect(body).toEqual(['l3', 'l4', `long ${'x'.repeat(17)}…`])
    expect(body.every(text => text.length <= 30 - 7)).toBe(true)
  })

  it('shows no tail at a collapsed bound', () => {
    const lines = present({ ...call('a'), live: { tail: ['l1'] } }, { ...bound, lines: 0 })
    expect(lines.some(line => line.zone === true)).toBe(false)
  })

  it('stops a finished call\'s marker blinking and colours it by its outcome', () => {
    const ok = present({ ...call('a'), live: { finished: { ok: true } } }, bound).find(line => line.text.startsWith('Bash('))!
    expect(ok.pulse).toBeUndefined()
    expect(ok.markerTone).toBe('done')
    const failed = present({ ...call('a'), live: { finished: { ok: false } } }, bound).find(line => line.text.startsWith('Bash('))!
    expect(failed.markerTone).toBe('failed')
  })

  it('keeps a step running while any call runs, and folds a finished one first', () => {
    const calls: ToolCallRow[] = [
      { ...call('a'), live: { tail: ['a1', 'a2', 'a3'], finished: { ok: true } } },
      { ...call('b'), live: { tail: ['b1', 'b2', 'b3'] } },
    ]
    const whole = fittedGroup(calls, bound, 100)
    expect(whole[1]!.pulse).toBe(true)
    const tight = fittedGroup(calls, bound, 7)
    const texts = tight.map(line => line.text)
    expect(texts).toContain('b3')
    expect(texts).not.toContain('a3')
    const settled = fittedGroup(calls.map(row => ({ ...row, live: { finished: { ok: true } } })), bound, 100)
    expect(settled[1]!.pulse).toBeUndefined()
    expect(settled[1]!.markerTone).toBe('done')
  })
})
