/** Harness span recording: explicit parenting, trace continuation, and bounded buffering. */

import { describe, expect, it } from 'vitest'
import { parseTraceparent, SpanRecorder, type SpanClock } from '../src/spans.ts'

function clock(): SpanClock {
  let t = 0
  let n = 0
  return {
    now: () => (t += 10),
    id: bytes => (++n).toString(16).padStart(bytes * 2, '0'),
  }
}

describe('SpanRecorder', () => {
  it('continues the desktop trace and nests children', () => {
    const recorder = new SpanRecorder(clock())
    const turn = recorder.start('bake.turn', `00-${'a'.repeat(32)}-${'b'.repeat(16)}-01`, { turn: 1 })
    const step = recorder.start('bake.step', turn, {}, 'agent-1')
    step.set({ step: 1 })
    step.end()
    turn.end('error', { reason: 'aborted' })
    turn.end()
    const [s, t] = recorder.drain()
    expect(t).toMatchObject({ name: 'bake.turn', traceId: 'a'.repeat(32), parentSpanId: 'b'.repeat(16), status: 'error', attrs: { turn: 1, reason: 'aborted' } })
    expect(s).toMatchObject({ name: 'bake.step', traceId: 'a'.repeat(32), parentSpanId: t?.spanId, lane: 'agent-1', attrs: { step: 1 } })
    expect(s!.endUs).toBeGreaterThan(s!.startUs)
    expect(recorder.drain()).toEqual([])
  })

  it('starts a new trace for a missing or malformed parent', () => {
    const recorder = new SpanRecorder(clock())
    recorder.start('a', 'garbage').end()
    const [span] = recorder.drain()
    expect(span?.parentSpanId).toBeUndefined()
    expect(span?.traceId).toHaveLength(32)
  })

  it('drops the oldest spans past its buffer', () => {
    const recorder = new SpanRecorder(clock(), 2)
    for (const name of ['a', 'b', 'c']) recorder.start(name).end()
    expect(recorder.dropped).toBe(1)
    expect(recorder.drain().map(s => s.name)).toEqual(['b', 'c'])
  })
})

describe('parseTraceparent', () => {
  it('rejects all-zero ids', () => {
    expect(parseTraceparent(`00-${'0'.repeat(32)}-${'b'.repeat(16)}-01`)).toBeUndefined()
    expect(parseTraceparent(`00-${'a'.repeat(32)}-${'0'.repeat(16)}-01`)).toBeUndefined()
    expect(parseTraceparent(42)).toBeUndefined()
  })
})
