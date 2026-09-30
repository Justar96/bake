/**
 * Turn header vocabulary. Runs under `bun test` because the module is pure.
 * The clock is a value the caller hands in.
 */

import { describe, expect, it } from 'bun:test'
import {
  activityWord, FOLD_REST, FOLD_SPINNER, foldFrame, FRAME_MS, formatElapsed, lastTurn, phaseLabel, phaseOf, SPINNER, SPINNER_REST, spinnerFrame,
  RATE_MIN_MS, RATE_MIN_TOKENS, rateLabel, turnSummary,
} from '../src/activity.ts'
import { dictionaries } from '../src/copy.ts'
import type { Row } from '../src/rows.ts'

const copy = dictionaries.en
const call: Row = { kind: 'tool-call', callId: 'c1', tool: 'bash', input: 'ls' }

describe('spinner', () => {
  /** A frame's dots as four rows of six, top to bottom. */
  const pixels = (frame: string): string[] => {
    const bits = [[0x01, 0x02, 0x04, 0x40], [0x08, 0x10, 0x20, 0x80]]
    return Array.from({ length: 4 }, (_, row) => [...frame].map(cell => {
      const dots = cell.codePointAt(0)! - 0x2800
      return bits.map(column => dots & column[row]! ? '#' : '.').join('')
    }).join(''))
  }

  it('kneads a round ball of dough one frame a beat, folding from each side in turn', () => {
    const frames = ['⠰⣿⠆', '⢴⣶⡦', '⣠⣤⣄', '⣰⣤⣄', '⣰⣦⣄', '⢠⣶⡄', '⠰⣿⠆', '⢴⣶⡦', '⣠⣤⣄', '⣠⣤⣆', '⣠⣴⣆', '⢠⣶⡄']
    expect(SPINNER).toEqual(frames)
    for (let step = 0; step < 36; step++) expect(spinnerFrame(step * FRAME_MS)).toBe(frames[step % 12])
    for (let step = 0; step < 6; step++) {
      expect(pixels(SPINNER[step + 6]!)).toEqual(pixels(SPINNER[step]!).map(row => [...row].reverse().join('')))
    }
  })

  it('holds the ball before the turn starts and when motion is disabled', () => {
    expect(SPINNER_REST).toBe('⠰⣿⠆')
    expect(pixels(SPINNER_REST)).toEqual(['..##..', '.####.', '.####.', '..##..'])
    expect(spinnerFrame(-500)).toBe(SPINNER_REST)
    expect(spinnerFrame(FRAME_MS - 1)).toBe(SPINNER_REST)
  })

  it('draws round dough in three Braille cells, resting on the bottom rows', () => {
    for (const frame of SPINNER) {
      expect([...frame]).toHaveLength(3)
      for (const cell of frame) expect(cell.codePointAt(0)! >> 8).toBe(0x28)
      const rows = pixels(frame).filter(row => row.includes('#'))
      expect(pixels(frame)[3]).toMatch(/#{2}/u)
      // Rounded, never a block: the top of the dough is narrower than its widest row.
      const width = (row: string): number => row.replaceAll('.', '').length
      expect(width(rows[0]!)).toBeLessThan(Math.max(...rows.map(width)))
    }
  })

  it('laminates a sheet of dough for compaction: ends folded in to a block, pressed, and rolled out', () => {
    const frames = ['⣀⣀⣀', '⢄⣀⡠', '⠢⣀⠔', '⠰⣀⠆', '⠠⣒⠄', '⠀⣶⠀', '⢀⣶⡀', '⢠⣤⡄', '⣀⣤⣀']
    expect(FOLD_SPINNER).toEqual(frames)
    expect(FOLD_SPINNER.map(pixels)).toEqual([
      // A flat sheet on the bench.
      ['......', '......', '......', '######'],
      // Its ends lift, swing up, stand, and fold over to meet in the middle.
      ['......', '......', '#....#', '.####.'],
      ['......', '#....#', '.#..#.', '..##..'],
      ['......', '.#..#.', '.#..#.', '..##..'],
      ['......', '..##..', '.#..#.', '..##..'],
      // Three layers in a compact block.
      ['......', '..##..', '..##..', '..##..'],
      // Pressed down.
      ['......', '..##..', '..##..', '.####.'],
      ['......', '......', '.####.', '.####.'],
      // Rolled out from the middle, back to the sheet.
      ['......', '......', '..##..', '######'],
    ])
    // The kneading's beat, looping.
    for (let step = 0; step < 27; step++) expect(foldFrame(step * FRAME_MS)).toBe(frames[step % frames.length])
    expect(foldFrame(-500)).toBe(frames[0])
    // Never a kneading frame, so the two animations cannot be mistaken.
    for (const frame of FOLD_SPINNER) expect(SPINNER).not.toContain(frame)
  })

  it('holds the folded block when motion is disabled, square where the kneading ball is round', () => {
    expect(FOLD_REST).toBe('⢠⣤⡄')
    expect(pixels(FOLD_REST)).toEqual(['......', '......', '.####.', '.####.'])
    expect(FOLD_SPINNER).toContain(FOLD_REST)
    expect(FOLD_REST).not.toBe(SPINNER_REST)
  })

  it('folds both ends at once, so every frame is centred in three cells on the bottom row', () => {
    for (const frame of FOLD_SPINNER) {
      expect([...frame]).toHaveLength(3)
      for (const cell of frame) expect(cell.codePointAt(0)! >> 8).toBe(0x28)
      const rows = pixels(frame)
      expect(rows[3]).toMatch(/#{2}/u)
      // Its own mirror image: the dough never drifts to one side.
      expect(rows).toEqual(rows.map(row => [...row].reverse().join('')))
    }
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
    expect(turnSummary(rows, copy, 65_000)).toEqual({ outcome: 'done', label: 'Completed', details: '1m 05s · edited 1 · ran 1 · read 2 · 1 failed', brief: '1m 05s' })
  })

  it('counts the calls inside a step\'s group', () => {
    const calls = [ran('a', 'bash'), ran('b', 'bash', false)].filter(row => row.kind === 'tool-call')
    expect(turnSummary([{ kind: 'tool-group', calls }, end('info')], copy, undefined).details).toBe('ran 2 · 1 failed')
  })

  it('reads the outcome from the recorded turn end', () => {
    expect(turnSummary([end('warn', 'Blocked')], copy, 3_000)).toEqual({ outcome: 'stopped', label: 'Blocked', details: '3s', brief: '3s' })
    expect(turnSummary([end('error')], dictionaries.zh, undefined)).toEqual({ outcome: 'failed', label: '失败', details: '', brief: '' })
  })

  it('closes the details with the final answer\'s rate, only for a sample long enough to mean something', () => {
    const answer = (tokens: number, ms: number): Row[] => [ran('a', 'bash'), { kind: 'assistant', text: 'Done.' }, { kind: 'rate', tokens, ms }, end('info')]
    expect(turnSummary(answer(856, 20_240), copy, 72_000).details).toBe('1m 12s · ran 1 · 42 tok/s')
    expect(turnSummary(answer(856, 20_240), dictionaries.zh, undefined).details).toBe('ran 1 · 42 token/秒')
    // Four tokens over 74 ms, or a second of a handful of tokens, is not a speed.
    expect(turnSummary(answer(4, 74), copy, undefined).details).toBe('ran 1')
    expect(turnSummary(answer(RATE_MIN_TOKENS - 1, 5_000), copy, undefined).details).toBe('ran 1')
    expect(turnSummary(answer(500, RATE_MIN_MS - 1), copy, undefined).details).toBe('ran 1')
    expect(rateLabel({ tokens: RATE_MIN_TOKENS, ms: RATE_MIN_MS }, copy)).toBe('64 tok/s')
    // A rate without finite numbers, such as a row from an older build, reports nothing rather than `NaN tok/s`.
    for (const broken of [{ tokens: Number.NaN, ms: 5_000 }, { tokens: 500, ms: Number.NaN }, { tokens: Number.POSITIVE_INFINITY, ms: 5_000 },
      { tokens: 500, ms: Number.POSITIVE_INFINITY }]) expect(rateLabel(broken, copy)).toBeUndefined()
    const legacy = { kind: 'rate', text: '4 tokens · 54.1 tok/s' } as unknown as Row
    expect(turnSummary([ran('a', 'bash'), legacy, end('info')], copy, undefined).details).toBe('ran 1')
    // A failed turn still reports the answer it gave before the failure.
    expect(turnSummary([{ kind: 'rate', tokens: 300, ms: 2_000 }, end('error')], copy, undefined))
      .toEqual({ outcome: 'failed', label: 'Failed', details: '150 tok/s', brief: '' })
    // A narrow header keeps the elapsed time once the counts and the rate give way.
    expect(turnSummary(answer(856, 20_240), copy, 72_000).brief).toBe('1m 12s')
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
