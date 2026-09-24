/**
 * Turn header vocabulary. Runs under `bun test` because the module is pure:
 * the clock is a value the caller hands in.
 */

import { describe, expect, it } from 'bun:test'
import { activityWord, FRAME_MS, formatElapsed, lastTurn, lightStride, phaseLabel, phaseOf, shimmer, shimmerStep, SHIMMER_LEVELS, SPINNER, SPINNER_REST, spinnerFrame, thinkingRows, turnSummary } from '../src/activity.ts'
import { dictionaries } from '../src/copy.ts'
import type { Row } from '../src/rows.ts'

const copy = dictionaries.en
const call: Row = { kind: 'tool-call', callId: 'c1', tool: 'bash', input: 'ls' }

describe('spinner', () => {
  it('moves the lower dot wave one column every two beats and loops without a blank', () => {
    const frames = ['⠠⠞⠁', '⠀⠴⠋', '⠁⠠⠞', '⠋⠀⠴', '⠞⠁⠠', '⠴⠋⠀']
    expect(SPINNER).toEqual(frames)
    for (let step = 0; step < 24; step++) {
      expect(spinnerFrame(step * FRAME_MS)).toBe(frames[Math.floor(step / 2) % 6])
    }
  })

  it('holds the centered wave before the turn starts and when motion is disabled', () => {
    expect(SPINNER_REST).toBe('⠠⠞⠁')
    expect(spinnerFrame(-500)).toBe(SPINNER_REST)
    expect(spinnerFrame(FRAME_MS * 2 - 1)).toBe(SPINNER_REST)
  })

  it('packs exactly six by three dots and preserves a right-moving shape across the seam', () => {
    for (const frame of SPINNER) expect(frame).not.toContain('\n')
    const pixels = SPINNER.map(frame => frame.split('\n').flatMap(line => {
      expect(line).toHaveLength(3)
      return Array.from({ length: 3 }, (_, row) => [...line].flatMap(cell => {
        const dots = cell.codePointAt(0)! - 0x2800
        expect(dots).toBeGreaterThanOrEqual(0)
        expect(dots).toBeLessThan(0x40)
        return [dots >> row & 1, dots >> (row + 3) & 1]
      }))
    }))
    expect(pixels[0]!.map(row => row.join(''))).toEqual([
      '000110', '001100', '011000',
    ])
    for (let step = 0; step < pixels.length; step++) {
      const previous = pixels[step]!
      const next = pixels[(step + 1) % pixels.length]!
      expect(next).toEqual(previous.map(row => [row.at(-1), ...row.slice(0, -1)]))
    }
  })
})

describe('shimmer', () => {
  const at = (length: number, step: number) => shimmer(length, step * FRAME_MS)

  it('sweeps a soft band in from the left, one cell a beat, and out on the right', () => {
    expect(Array.from({ length: 9 }, (_, step) => at(5, step).join(''))).toEqual([
      '10000', '21000', '32100', '23210', '12321', '01232', '00123', '00012', '00001',
    ])
    // Brightest at the band's centre, and never brighter than the glint.
    expect(Math.max(...at(5, 4))).toBe(SHIMMER_LEVELS)
  })

  it('rests unlit between sweeps, then starts again', () => {
    // Nine beats cross five cells, then twelve rest.
    const rest = Array.from({ length: 12 }, (_, index) => at(5, 9 + index))
    expect(rest.every(levels => levels.every(level => level === 0))).toBe(true)
    expect(at(5, 21)).toEqual(at(5, 0))
    expect(shimmerStep(5, 9 * FRAME_MS)).toBeUndefined()
    expect(shimmerStep(5, 21 * FRAME_MS)).toBe(0)
  })

  it('holds still within a beat, and before the turn started', () => {
    expect(shimmer(5, FRAME_MS * 3 + FRAME_MS - 1)).toEqual(at(5, 3))
    expect(shimmer(5, -100)).toEqual(at(5, 0))
    expect(shimmer(0, FRAME_MS)).toEqual([])
    expect(shimmerStep(0, FRAME_MS)).toBeUndefined()
  })

  it('crosses a wider rule in longer strides, so the sweep keeps its pace', () => {
    expect([1, 32, 33, 64, 65, 200].map(lightStride)).toEqual([1, 1, 2, 2, 3, 3])
    // Three cells a beat: 12 cells and a five-cell band take six beats, then rest.
    expect(Array.from({ length: 7 }, (_, beat) => shimmerStep(12, beat * FRAME_MS, 3))).toEqual([0, 3, 6, 9, 12, 15, undefined])
    expect(shimmer(12, FRAME_MS * 2, 3).join('')).toBe('001232100000')
    expect(shimmerStep(12, 18 * FRAME_MS, 3)).toBe(0)
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

  it('finds the running call inside a step\'s group', () => {
    const done: Row = { kind: 'tool-call', callId: 'd', tool: 'read', input: '', result: { ok: true, text: '' } }
    const group = { kind: 'tool-group' as const, calls: [done, call].filter(row => row.kind === 'tool-call') }
    expect(phaseOf([group], undefined)).toEqual({ kind: 'running', tool: 'bash' })
    expect(phaseOf([], group)).toEqual({ kind: 'running', tool: 'bash' })
  })

  it('localizes each phase', () => {
    expect(phaseLabel({ kind: 'thinking' }, copy)).toBe('thinking')
    expect(phaseLabel({ kind: 'writing' }, copy)).toBe('writing')
    expect(phaseLabel({ kind: 'running', tool: 'bash' }, copy)).toBe('running bash')
    expect(phaseLabel({ kind: 'thinking' }, dictionaries.zh)).toBe('思考')
    expect(phaseLabel(undefined, copy)).toBeUndefined()
  })
})

describe('thinking window', () => {
  const thinking = (text: string, width = 20, count = 3) => thinkingRows([{ kind: 'reasoning', text }], width, count)

  it('shows the newest rows across paragraphs, leaving blank lines out', () => {
    expect(thinking('first\n\n  second  \n\n')).toEqual(['first', 'second'])
    expect(thinking('one two three four five six seven eight nine ten eleven')).toEqual([
      'one two three four', 'five six seven eight', 'nine ten eleven',
    ])
    expect(thinking('one two three four five six seven eight nine ten eleven twelve thirteen')).toEqual([
      'five six seven eight', 'nine ten eleven', 'twelve thirteen',
    ])
  })

  it('reads through the markdown it cannot render', () => {
    expect(thinking('## Plan\n**Check** `startup.ts` first\n> quoted\ttabbed', 40)).toEqual([
      'Plan', 'Check startup.ts first', '> quoted tabbed',
    ])
  })

  it('never loses a row as text arrives, and keeps full rows where they are', () => {
    const text = 'The loader reads the profile before it resolves each plugin and then the session store opens.\nNext'
    let previous: readonly string[] = []
    for (let end = 1; end <= text.length; end++) {
      const rows = thinking(text.slice(0, end), 24)
      expect(rows.length).toBeGreaterThanOrEqual(previous.length)
      expect(rows.every(row => row.length <= 24)).toBe(true)
      // Rows above the newest keep their text while it grows, or move up one
      // together when it wraps; the row it wrapped from may give up a word.
      if (rows.length === previous.length && rows.length > 1) {
        const grew = Bun.deepEquals(rows.slice(0, -1), previous.slice(0, -1))
        const scrolled = Bun.deepEquals(rows.slice(0, -2), previous.slice(1, -1))
        expect(grew || scrolled, `${JSON.stringify(previous)} -> ${JSON.stringify(rows)}`).toBe(true)
      }
      previous = rows
    }
  })

  it('is empty once anything else is newest', () => {
    expect(thinkingRows([{ kind: 'reasoning', text: 'first' }, { kind: 'assistant', text: 'ok' }], 20, 3)).toEqual([])
    expect(thinking('\n\n')).toEqual([])
    expect(thinking('first', 20, 0)).toEqual([])
    expect(thinkingRows([], 20, 3)).toEqual([])
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

  it('counts the calls inside a step\'s group', () => {
    const calls = [ran('a', 'bash'), ran('b', 'bash', false)].filter(row => row.kind === 'tool-call')
    expect(turnSummary([{ kind: 'tool-group', calls }, end('info')], copy, undefined).details).toBe('ran 2 · 1 failed')
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
