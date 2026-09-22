/** The action fold, run under `bun test` because it is pure. */

import { describe, expect, it } from 'bun:test'
import { Actions } from '../src/actions.ts'
import type { Row } from '../src/rows.ts'

const call = (callId: string): Row => ({ kind: 'tool-call', callId, tool: 'bash', input: callId })
const result = (callId: string, ok = true): Row => ({ kind: 'tool-result', callId, ok, text: `${callId} out` })

describe('actions', () => {
  it('holds a call until its result and releases one merged row', () => {
    const actions = new Actions()
    expect(actions.fold([call('a')])).toEqual([])
    expect(actions.pending).toEqual([call('a')])
    expect(actions.fold([result('a')])).toEqual([{ ...call('a'), result: { ok: true, text: 'a out' } }])
    expect(actions.pending).toEqual([])
  })

  it('releases in call order when results arrive out of order', () => {
    const actions = new Actions()
    actions.fold([call('a'), call('b')])
    expect(actions.fold([result('b')])).toEqual([])
    expect(actions.fold([result('a', false)]).map(row => row.kind === 'tool-call' ? [row.callId, row.result?.ok] : row.kind))
      .toEqual([['a', false], ['b', true]])
  })

  it('passes other rows through ahead of held calls, and a result it never held', () => {
    const actions = new Actions()
    actions.fold([call('a')])
    const note: Row = { kind: 'assistant', text: 'hi' }
    expect(actions.fold([note, result('z')])).toEqual([note, result('z')])
    expect(actions.pending).toEqual([call('a')])
  })

  it('settles every held call, answered or not', () => {
    const actions = new Actions()
    actions.fold([call('a'), call('b')])
    actions.fold([result('b')])
    const end: Row = { kind: 'notice', placement: 'turn-end', tone: 'warn', text: 'Interrupted' }
    expect(actions.fold([end], true)).toEqual([call('a'), { ...call('b'), result: { ok: true, text: 'b out' } }, end])
    expect(actions.pending).toEqual([])
  })
})
