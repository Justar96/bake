/** The action fold, run under `bun test` because it is pure. */

import { describe, expect, it } from 'bun:test'
import { Actions, SETTLES } from '../src/actions.ts'
import { PENDING_ARGUMENTS } from '../src/present.ts'
import type { Row, ToolCallRow } from '../src/rows.ts'

const call = (callId: string): Row => ({ kind: 'tool-call', callId, tool: 'bash', input: callId })
const result = (callId: string, ok = true): Row => ({ kind: 'tool-result', callId, ok, text: `${callId} out` })

describe('actions', () => {
  it('holds a call until its step ends and releases one merged row', () => {
    const actions = new Actions()
    expect(actions.fold([call('a')])).toEqual([])
    expect(actions.pending).toEqual([call('a')])
    // Finished, it waits for the step, in case the step makes another call.
    expect(actions.fold([result('a')])).toEqual([])
    expect(actions.pending).toEqual([{ ...call('a'), result: { ok: true, text: 'a out' } }])
    expect(actions.fold([], true)).toEqual([{ ...call('a'), result: { ok: true, text: 'a out' } }])
    expect(actions.pending).toEqual([])
  })

  it('releases a step\'s calls as one group, in call order, whatever order their results arrived in', () => {
    const actions = new Actions()
    actions.fold([call('a'), call('b')])
    expect(actions.pending).toEqual([{ kind: 'tool-group', calls: [call('a'), call('b')] }])
    expect(actions.fold([result('b')])).toEqual([])
    expect(actions.fold([result('a', false)])).toEqual([])
    const [group, ...rest] = actions.fold([], true)
    expect(rest).toEqual([])
    expect(group?.kind === 'tool-group' ? group.calls.map(row => [row.callId, row.result?.ok]) : group)
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
    expect(actions.fold([end], true)).toEqual([{ kind: 'tool-group', calls: [call('a'), { ...call('b'), result: { ok: true, text: 'b out' } }] }, end])
    expect(actions.pending).toEqual([])
  })

  it('settles on the end of a step, and on a message or turn end for a log without one', () => {
    expect([...SETTLES].sort()).toEqual(['assistant/message', 'step/end', 'turn/end'])
  })

  it('draws a step\'s calls as one block from the moment they stream to the moment they print', () => {
    // Every stage the block passes through has the same calls in the same
    // shape: a stage that dropped one, or split the group, would give up rows
    // the frame holds blank until history next prints.
    const actions = new Actions()
    const streamed = (callId: string): Row => ({ kind: 'tool-call', callId, tool: 'bash', input: PENDING_ARGUMENTS })
    const text: Row = { kind: 'assistant', text: 'Checking' }
    expect(actions.live([text, streamed('a'), streamed('b')]))
      .toEqual([text, { kind: 'tool-group', calls: [streamed('a'), streamed('b')] }])
    // The message commits and settles; the stream has not ended yet.
    actions.fold([], true)
    actions.announce([streamed('a'), streamed('b')] as ToolCallRow[])
    expect(actions.live([streamed('a'), streamed('b')])).toEqual([{ kind: 'tool-group', calls: [streamed('a'), streamed('b')] }])
    // The stream ends, and the loop dispatches one call at a time.
    expect(actions.live()).toEqual([{ kind: 'tool-group', calls: [streamed('a'), streamed('b')] }])
    actions.fold([call('a')])
    expect(actions.live()).toEqual([{ kind: 'tool-group', calls: [call('a'), streamed('b')] }])
    actions.fold([result('a'), call('b')])
    expect(actions.pending).toEqual([{ kind: 'tool-group', calls: [{ ...call('a'), result: { ok: true, text: 'a out' } }, call('b')] }])
    // A call the loop never dispatched leaves with its step.
    actions.announce([streamed('a'), streamed('b'), streamed('c')] as ToolCallRow[])
    expect(actions.fold([result('b')], false)).toEqual([])
    expect(actions.fold([], true)).toHaveLength(1)
    expect(actions.live()).toEqual([])
  })
})
