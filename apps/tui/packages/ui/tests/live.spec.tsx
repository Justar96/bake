/** Live-region height while a turn runs: layout invariants L1 and L2, the input following the newest line. */
import React, { act } from 'react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { useWindowSize } from 'ink'
import { cleanup, render } from '../../../tests/render.tsx'
import { App, type AppProps } from '../src/app.tsx'
import { dictionaries } from '../src/copy.ts'
import { budgetFor, CHROME_ROWS, NOTICE_BUDGET, type WindowSize } from '../src/layout.ts'
import { appendTranscript, emptyTranscript } from '../src/transcript.ts'
import { SPINNER_MS, type Clock } from '../src/activity.ts'
import type { Row } from '../src/rows.ts'

afterEach(cleanup)

function props(overrides: Partial<AppProps> = {}): AppProps {
  return {
    files: { query: undefined, entries: [], loading: false, error: undefined }, onReferenceQuery: () => {},
    completion: { entries: [], loading: false, error: undefined }, completionLimit: 8, resultLines: 8,
    committed: emptyTranscript, live: [], pending: [], status: 'idle', stopping: false,
    command: undefined, notice: undefined, interaction: undefined, todos: undefined,
    model: 'mock/model', cwd: '/workspace', sessionId: 'session-live', copy: dictionaries.en, frame: 'round', quitting: false, context: undefined,
    onSubmit: vi.fn(), onCancel: vi.fn(), onInterrupt: vi.fn(), onAnswer: vi.fn(), ...overrides,
  }
}

/** Rendered rows, which is what the terminal scrolls — not the lines fed in. */
const heightOf = (frame: string | undefined): number => (frame ?? '').split('\n').length

/**
 * The size the components under test are laid out against.
 *
 * Read through the same hook as the layout, using the test's fixed-size stream.
 */
function measured(): WindowSize {
  let size: WindowSize | undefined
  function Probe(): null { size = useWindowSize(); return null }
  render(<Probe />)
  cleanup()
  return size!
}

/** One streaming answer, arriving a line at a time. */
const streamed = (lines: number): Row[] => [{
  kind: 'assistant',
  text: Array.from({ length: lines }, (_, index) => `streamed line ${index}`).join('\n'),
}]

describe('live region', () => {
  it('grows a row per streamed line until its window is full, then holds', () => {
    // Measured first: `measured` cleans up every rendered tree, this one too.
    const live = budgetFor(measured()).live
    const state = props({ status: 'running' })
    const ui = render(<App {...state} />)
    const idle = heightOf(ui.lastFrame())
    for (let lines = 1; lines <= 24; lines++) {
      ui.rerender(<App {...state} live={streamed(lines)} />)
      // No reserved block of empty rows: the input sits under the newest line.
      // The answer's own section blank is one of the rows it spends.
      expect(heightOf(ui.lastFrame())).toBe(idle + Math.min(lines + 1, live))
    }
  })

  it('returns the input to under the summary once the live output clears', () => {
    const state = props()
    const ui = render(<App {...state} />)
    const idle = heightOf(ui.lastFrame())
    // The turn header is the one row a running turn adds before it speaks.
    ui.rerender(<App {...state} status="running" />)
    expect(heightOf(ui.lastFrame())).toBe(idle + 1)
    ui.rerender(<App {...state} status="running" live={streamed(2)} />)
    expect(heightOf(ui.lastFrame())).toBe(idle + 1 + 3)
    // The header's row stays, holding the finished turn's summary, so the
    // input does not move up when the turn ends.
    ui.rerender(<App {...state} />)
    expect(heightOf(ui.lastFrame())).toBe(idle + 1)
  })

  it('keeps the newest lines when the stream outgrows its budget', () => {
    const live = budgetFor(measured()).live
    const ui = render(<App {...props({ status: 'running', live: streamed(live + 5) })} />)
    const frame = ui.lastFrame() ?? ''
    expect(frame).toContain(`streamed line ${live + 4}`)
    expect(frame).not.toContain('streamed line 0')
  })

  it('counts wrapped rows, not lines, against its window', () => {
    // Counting lines rather than rendered rows loses exactly here: one line of
    // prose past the measure occupies several rows, so a window of lines grows
    // past the budget the chrome was charged against.
    const live = budgetFor(measured()).live
    const state = props({ status: 'running' })
    const ui = render(<App {...state} />)
    const idle = heightOf(ui.lastFrame())
    const long = 'wrapped '.repeat(40).trim()
    ui.rerender(<App {...state} live={[{ kind: 'assistant', text: [long, long, long, 'newest'].join('\n') }]} />)
    expect(heightOf(ui.lastFrame()) - idle).toBeLessThanOrEqual(live)
    expect(ui.lastFrame()).toContain('newest')
  })

  it('collapses to one line behind a question, which outranks it', () => {
    const state = props({
      status: 'running',
      live: streamed(8),
      interaction: { id: 1, kind: 'approval', tool: 'bash', reason: 'rm -rf lib' },
    })
    const ui = render(<App {...state} />)
    const frame = ui.lastFrame() ?? ''
    expect(frame).toContain('streamed line 7')
    expect(frame).not.toContain('streamed line 6')
  })

  it('bounds a notice however long it runs', () => {
    // The composer and status line are the rows the user aims at. A notice
    // that grows with harness-owned content pushes them down the screen, and
    // one long enough clears the screen outright.
    const state = props()
    const ui = render(<App {...state} />)
    const bare = heightOf(ui.lastFrame())
    const heights = [1, 3, 8, 40, 400].map(lines => {
      ui.rerender(<App {...state} notice={Array.from({ length: lines }, (_, index) => `notice line ${index}`).join('\n')} />)
      return heightOf(ui.lastFrame()) - bare
    })
    expect(heights).toEqual([1, 3, NOTICE_BUDGET, NOTICE_BUDGET, NOTICE_BUDGET])
  })

  it('says how many notice lines it left out rather than dropping them silently', () => {
    const ui = render(<App {...props({ notice: Array.from({ length: 40 }, (_, index) => `line ${index}`).join('\n') })} />)
    expect(ui.lastFrame()).toContain(dictionaries.en.moreLines)
    expect(ui.lastFrame()).toContain('line 0')
    expect(ui.lastFrame()).not.toContain('line 39')
  })

  it('keeps the chrome on screen with every region loaded at once', () => {
    // Each region bounded against the whole dynamic region is not enough: three
    // of them at their own limit overrun it together. This is the case that
    // catches that — queued input, staged attachments, a task list, a notice
    // and a running turn, each longer than the terminal on its own.
    const many = (prefix: string, count: number) =>
      Array.from({ length: count }, (_, index) => `${prefix} ${index}`)
    const ui = render(<App {...props({
      status: 'running',
      live: streamed(60),
      todos: many('task', 60).map(text => ({ text, status: 'pending' as const })),
      pending: many('queued', 60).map((text, index) => ({ id: `p${index}`, target: 'next-turn' as const, text })),
      attachments: many('file', 60).map(name => ({ name, bytes: 4 })),
      command: 'x'.repeat(4000),
      notice: many('notice line', 60).join('\n'),
    })} />)
    const frame = ui.lastFrame() ?? ''
    // The chrome is at the bottom of the frame, where the user left the cursor
    // — not somewhere in the middle of a panel that overran. Which of its rows
    // carries which field is `Chrome`'s business, tested in line.spec.tsx.
    const chrome = frame.split('\n').slice(-CHROME_ROWS).join('\n')
    expect(chrome).toContain(dictionaries.en.steering)
    expect(chrome).toContain(dictionaries.en.working)
    // The heading is written above the dynamic region, so it is the one row of
    // the frame the budget does not own.
    const heading = 1
    expect(heightOf(frame)).toBeLessThanOrEqual(heading + budgetFor(measured()).dynamic)
  })

  it('shows quit feedback in one row above the input', () => {
    const state = props()
    const ui = render(<App {...state} />)
    const quiet = heightOf(ui.lastFrame())
    ui.rerender(<App {...state} quitting />)
    const frame = ui.lastFrame() ?? ''
    const rows = frame.split('\n')

    expect(frame).toContain(dictionaries.en.quit)
    expect(rows.findIndex(row => row.includes(dictionaries.en.quit))).toBeLessThan(rows.findIndex(row => row.includes('> ')))
    expect(heightOf(frame)).toBe(quiet + 1)
  })

  it('keeps the interrupt prompt and command feedback out of each other\'s way', () => {
    // They shared `notice` once, so a command result cleared the prompt and the
    // prompt cleared the result. Two channels, both on screen at once.
    const ui = render(<App {...props({ quitting: true, notice: 'Model set for the next turn' })} />)
    const frame = ui.lastFrame() ?? ''
    expect(frame).toContain(dictionaries.en.quit)
    expect(frame).toContain('Model set for the next turn')
    expect(frame.indexOf('Model set')).toBeLessThan(frame.indexOf(dictionaries.en.quit))
  })

  it('draws the chrome directly under the newest line, in exactly its charged rows', () => {
    // CHROME_ROWS is the denominator of every other budget, so it is measured
    // against the component rather than kept in step by hand. Nothing pads the
    // frame: the heading is the newest line, and the chrome follows it.
    const ui = render(<App {...props()} />)
    const rows = (ui.lastFrame() ?? '').split('\n')
    expect(rows).toHaveLength(1 + CHROME_ROWS)
    expect(rows[0]).toContain('session-live')
    expect(rows[1]!.trim()).toBe('')
    expect(rows[3]).toContain('> ')
    expect(rows[5]).toContain(dictionaries.en.ready)
  })
})

describe('live window under a streaming answer', () => {
  const call: Row = { kind: 'tool-call', callId: 'c1', tool: 'bash', input: '{"command":"ls"}' }
  const paragraph = (index: number): string =>
    `Paragraph ${index} runs long enough that at this width it wraps onto a second row of the prose measure, then ends ${index}.`
  const answer = (count: number): Row => ({ kind: 'assistant', text: Array.from({ length: count }, (_, index) => paragraph(index)).join('\n') })

  it('keeps its opening blank and holds its height once full', () => {
    const live = budgetFor(measured()).live
    const state = props({ status: 'running', committed: appendTranscript(emptyTranscript, [{ kind: 'user', text: 'go' }, call]) })
    const ui = render(<App {...state} />)
    const idle = heightOf(ui.lastFrame())
    let full: number | undefined
    for (let count = 1; count <= 12; count++) {
      ui.rerender(<App {...state} live={[answer(count)]} />)
      const frame = ui.lastFrame()!
      const rows = frame.split('\n')
      const newest = rows.findIndex(row => row.includes(`ends ${count - 1}.`))
      expect(rows[newest + 1]).toBe('')
      // The section's opening blank survives the cut.
      expect(rows[rows.findIndex(row => row.includes('{"command":"ls"}')) + 1]).toBe('')
      const height = heightOf(frame) - idle
      if (height === live) full ??= count
      // Once the window fills it stays full: the composer does not bob with
      // each paragraph as whole lines stop fitting.
      if (full !== undefined) expect(height).toBe(live)
    }
    expect(full).toBeDefined()
  })
})

/** A clock the test advances by hand, recording each interval it hands out. */
function fakeClock() {
  let time = 0
  const ticks = new Set<() => void>()
  const clock: Clock = {
    now: () => time,
    every: (_ms, tick) => {
      ticks.add(tick)
      return () => { ticks.delete(tick) }
    },
  }
  const advance = (ms: number): void => {
    time += ms
    for (const tick of ticks) act(tick)
  }
  return { clock, advance, active: () => ticks.size }
}

/** The turn header row: rail glyph, then the word and its ellipsis. */
const headerOf = (frame: string | undefined): string | undefined =>
  (frame ?? '').split('\n').find(line => /^\S \S+…/.test(line))

describe('turn header', () => {
  it('keeps one word through thinking, writing, a commit, and a running tool', () => {
    const state = props({ status: 'running', committed: appendTranscript(emptyTranscript, [{ kind: 'user', text: 'go' }]) })
    const ui = render(<App {...state} />)
    const word = headerOf(ui.lastFrame())!.split('…')[0]
    expect(word).toMatch(/^✻ \S+$/)
    const call: Row = { kind: 'tool-call', callId: 'c1', tool: 'bash', input: 'ls' }
    const steps: [Partial<AppProps>, RegExp][] = [
      [{ live: [{ kind: 'reasoning', text: 'look at files' }] }, /… {2}thinking$/],
      [{ live: [{ kind: 'reasoning', text: 'look at files' }, { kind: 'assistant', text: 'Listing.' }] }, /… {2}writing$/],
      [{ live: [], committed: appendTranscript(state.committed, [{ kind: 'assistant', text: 'Listing.' }, call]) }, /… {2}running bash$/],
    ]
    for (const [step, phase] of steps) {
      ui.rerender(<App {...state} {...step} />)
      const header = headerOf(ui.lastFrame())!
      expect(header.startsWith(`${word}…`)).toBe(true)
      expect(header).toMatch(phase)
    }
    ui.rerender(<App {...state} status="idle" />)
    expect(headerOf(ui.lastFrame())).toBeUndefined()
  })

  it('holds its height while reasoning streams, showing only the newest line', () => {
    const state = props({ status: 'running' })
    const ui = render(<App {...state} live={[{ kind: 'reasoning', text: 'line 1' }]} />)
    const height = heightOf(ui.lastFrame())
    for (let lines = 2; lines <= 30; lines++) {
      const text = Array.from({ length: lines }, (_, index) => `line ${index + 1}`).join('\n')
      ui.rerender(<App {...state} live={[{ kind: 'reasoning', text }]} />)
      const frame = ui.lastFrame()!
      expect(heightOf(frame)).toBe(height)
      expect(frame).toContain(`line ${lines}`)
      expect(frame).not.toContain(`line ${lines - 1}\n`)
      // Above the header, which is the row resting on the input.
      const rows = frame.split('\n')
      const header = rows.findIndex(line => /^\S \S+…/.test(line))
      expect(rows[header - 1]).toBe(`  line ${lines}`)
      expect(rows[header + 1]).toMatch(/^╭/)
    }
  })

  it('animates from the clock it is given and stops asking once the turn ends', () => {
    const { clock, advance, active } = fakeClock()
    const state = props({ status: 'running', clock })
    const ui = render(<App {...state} />)
    expect(active()).toBe(1)
    const before = headerOf(ui.lastFrame())!
    advance(SPINNER_MS)
    expect(headerOf(ui.lastFrame())![0]).not.toBe(before[0])
    advance(12_000)
    expect(headerOf(ui.lastFrame())).toMatch(/ {2}12s$/)
    ui.rerender(<App {...state} status="idle" />)
    expect(active()).toBe(0)
  })

  it('stands still for a screen reader or without a clock', () => {
    const ui = render(<App {...props({ status: 'running' })} />)
    expect(headerOf(ui.lastFrame())).toMatch(/^✻ \S+…$/)
  })

  it('counts seconds without motion, and leaves a running action unpulsed', () => {
    const { clock, advance } = fakeClock()
    const live: Row[] = [{ kind: 'tool-call', callId: 'c1', tool: 'bash', input: 'ls' }]
    const ui = render(<App {...props({ status: 'running', clock, motion: false, live })} />)
    const frames = new Set<string>()
    for (let step = 0; step < 8; step++) { advance(SPINNER_MS); frames.add(ui.lastFrame()!.replace(/\d+s$/m, '')) }
    expect(frames.size).toBe(1)
    expect(headerOf(ui.lastFrame())).toMatch(/^✻ \S+….* 0s$/)
    advance(2_000)
    expect(headerOf(ui.lastFrame())).toMatch(/ 2s$/)
  })
})

/** The row a finished turn leaves above the input: an outcome glyph, then its label. */
const summaryOf = (frame: string | undefined): string | undefined =>
  (frame ?? '').split('\n').find(line => /^[✓■✗] /.test(line))

describe('turn summary', () => {
  const user: Row = { kind: 'user', text: 'go' }
  const done = (ok: boolean): Row[] => [
    { kind: 'tool-call', callId: 'c1', tool: 'bash', input: 'ls', result: { ok: true, text: 'a' } },
    { kind: 'tool-call', callId: 'c2', tool: 'read_file', input: 'a', result: { ok, text: 'b' } },
    { kind: 'assistant', text: 'Done.' },
    { kind: 'notice', placement: 'turn-end', tone: ok ? 'info' : 'error', text: ok ? 'Completed' : 'E: broke' },
  ]

  it('replaces the header in place when the turn ends and holds until the next begins', () => {
    const { clock, advance } = fakeClock()
    const start = appendTranscript(emptyTranscript, [user])
    const ui = render(<App {...props({ status: 'running', clock, committed: start })} />)
    const height = heightOf(ui.lastFrame())
    advance(42_000)
    const end = appendTranscript(start, done(true))
    ui.rerender(<App {...props({ status: 'running', clock, committed: end })} />)
    const running = heightOf(ui.lastFrame())
    ui.rerender(<App {...props({ status: 'idle', clock, committed: end })} />)
    expect(summaryOf(ui.lastFrame())).toBe('✓ Completed  42s · ran 1 · read 1')
    expect(heightOf(ui.lastFrame())).toBe(running)
    expect(running).toBeGreaterThan(height)
    ui.rerender(<App {...props({ status: 'idle', clock, committed: end, notice: 'Model: x' })} />)
    expect(summaryOf(ui.lastFrame())).toBe('✓ Completed  42s · ran 1 · read 1')
    ui.rerender(<App {...props({ status: 'running', clock, committed: appendTranscript(end, [user]) })} />)
    expect(summaryOf(ui.lastFrame())).toBeUndefined()
    expect(headerOf(ui.lastFrame())).toBeDefined()
  })

  it('names a failed turn and its failed actions', () => {
    const start = appendTranscript(emptyTranscript, [user])
    const ui = render(<App {...props({ status: 'running', committed: start })} />)
    ui.rerender(<App {...props({ status: 'idle', committed: appendTranscript(start, done(false)) })} />)
    expect(summaryOf(ui.lastFrame())).toBe('✗ Failed  ran 1 · read 1 · 1 failed')
  })

  it('summarizes a replayed session\'s last turn untimed, and says Completed only there', () => {
    const ui = render(<App {...props({ committed: appendTranscript(emptyTranscript, [user, ...done(false), user, ...done(true)]) })} />)
    expect(summaryOf(ui.lastFrame())).toBe('✓ Completed  ran 1 · read 1')
    expect(ui.lastFrame()!.match(/Completed/g)).toHaveLength(1)
  })
})
