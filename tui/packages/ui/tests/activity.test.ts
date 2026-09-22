/**
 * Turn header vocabulary. Runs under `bun test` because the module is pure:
 * the clock is a value the caller hands in.
 */

import { describe, expect, it } from 'bun:test'
import { activityWord, formatElapsed, lastTurn, phaseLabel, phaseOf, reasoningTicker, SPINNER, SPINNER_MS, spinnerFrame, turnSummary } from '../src/activity.ts'
import { dictionaries } from '../src/copy.ts'
import type { Row } from '../src/rows.ts'

const copy = dictionaries.en
const call: Row = { kind: 'tool-call', callId: 'c1', tool: 'bash', input: 'ls' }

describe('spinner', () => {
  it('plays the frames forward and back without repeating an end', () => {
    const frames = Array.from({ length: SPINNER.length * 2 - 2 }, (_, step) => spinnerFrame(step * SPINNER_MS))
    expect(frames).toEqual(['·', '✢', '✶', '✻', '✽', '✻', '✶', '✢'])
    expect(spinnerFrame(frames.length * SPINNER_MS)).toBe('·')
  })

  it('holds the first frame for a time before the turn started', () => {
    expect(spinnerFrame(-500)).toBe('·')
  })
})

describe('activity word', () => {
  it('keeps one word for one seed and draws it from the locale list', () => {
    const words = copy.activityWords.split('|')
    const word = activityWord(copy, 'session:4')
    expect(activityWord(copy, 'session:4')).toBe(word)
    expect(words).toContain(word)
  })

  it('varies across turns', () => {
    const chosen = new Set(Array.from({ length: 20 }, (_, turn) => activityWord(copy, `session:${turn}`)))
    expect(chosen.size).toBeGreaterThan(1)
  })

  it('falls back to the working label for an empty list', () => {
    expect(activityWord({ ...copy, activityWords: '' }, 'seed')).toBe(copy.working)
  })
})

describe('phase', () => {
  it('follows the newest live row', () => {
    expect(phaseOf([{ kind: 'reasoning', text: 'hm' }], undefined)).toEqual({ kind: 'thinking' })
    expect(phaseOf([{ kind: 'reasoning', text: 'hm' }, { kind: 'assistant', text: 'ok' }], undefined)).toEqual({ kind: 'writing' })
    expect(phaseOf([call], undefined)).toEqual({ kind: 'running', tool: 'bash' })
  })

  it('reads a committed call without its result as a running tool', () => {
    expect(phaseOf([], call)).toEqual({ kind: 'running', tool: 'bash' })
    expect(phaseOf([], { kind: 'assistant', text: 'done' })).toBeUndefined()
    expect(phaseOf([], undefined)).toBeUndefined()
  })

  it('localizes each phase', () => {
    expect(phaseLabel({ kind: 'thinking' }, copy)).toBe('thinking')
    expect(phaseLabel({ kind: 'writing' }, copy)).toBe('writing')
    expect(phaseLabel({ kind: 'running', tool: 'bash' }, copy)).toBe('running bash')
    expect(phaseLabel({ kind: 'thinking' }, dictionaries.zh)).toBe('思考')
    expect(phaseLabel(undefined, copy)).toBeUndefined()
  })
})

describe('reasoning ticker', () => {
  it('shows the last non-empty line while reasoning streams', () => {
    expect(reasoningTicker([{ kind: 'reasoning', text: 'first\n  second  \n\n' }])).toBe('second')
  })

  it('is absent once anything else is newest', () => {
    expect(reasoningTicker([{ kind: 'reasoning', text: 'first' }, { kind: 'assistant', text: 'ok' }])).toBeUndefined()
    expect(reasoningTicker([{ kind: 'reasoning', text: '\n\n' }])).toBeUndefined()
    expect(reasoningTicker([])).toBeUndefined()
  })
})

describe('elapsed', () => {
  it('counts whole seconds, then minutes with padded seconds', () => {
    expect(formatElapsed(0)).toBe('0s')
    expect(formatElapsed(8_999)).toBe('8s')
    expect(formatElapsed(65_000)).toBe('1m 05s')
    expect(formatElapsed(-1)).toBe('0s')
  })
})

describe('turn summary', () => {
  const end = (tone: 'info' | 'warn' | 'error', text = 'x'): Row => ({ kind: 'notice', placement: 'turn-end', tone, text })
  const ran = (callId: string, tool: string, ok = true): Row => ({ kind: 'tool-call', callId, tool, input: '', result: { ok, text: '' } })

  it('counts actions by past verb, edits first, then failures', () => {
    const rows: Row[] = [
      { kind: 'user', text: 'go' }, ran('a', 'read_file'), ran('b', 'bash'), ran('c', 'read_file', false), ran('d', 'apply_patch'), end('info'),
    ]
    expect(turnSummary(rows, copy, 65_000)).toEqual({ outcome: 'done', label: 'Completed', details: '1m 05s · edited 1 · ran 1 · read 2 · 1 failed' })
  })

  it('reads the outcome from the recorded turn end', () => {
    expect(turnSummary([end('warn', 'Blocked')], copy, 3_000)).toEqual({ outcome: 'stopped', label: 'Blocked', details: '3s' })
    expect(turnSummary([end('error')], dictionaries.zh, undefined)).toEqual({ outcome: 'failed', label: '失败', details: '' })
  })

  it('counts a standalone failed result', () => {
    expect(turnSummary([{ kind: 'tool-result', callId: 'z', ok: false, text: 'no' }], copy, undefined).details).toBe('1 failed')
  })

  it('finds the newest ended turn, and none once another has started', () => {
    const user: Row = { kind: 'user', text: 'go' }
    const first = [user, ran('a', 'bash'), end('info')]
    const second = [user, ran('b', 'read_file'), end('warn')]
    expect(lastTurn([...first, ...second])).toEqual(second)
    expect(lastTurn([...first, { kind: 'notice', tone: 'info', text: 'Model: x' }])).toEqual(first)
    expect(lastTurn([...first, user])).toBeUndefined()
    expect(lastTurn([user])).toBeUndefined()
  })
})
