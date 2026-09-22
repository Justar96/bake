/** Live-region height while a turn runs: layout invariants L1 and L2. */
import React from 'react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { cleanup, render } from 'ink-testing-library'
import { App, type AppProps } from '../src/app.tsx'
import { dictionaries } from '../src/copy.ts'
import { CHROME_ROWS, NOTICE_BUDGET } from '../src/layout.ts'
import { emptyTranscript } from '../src/transcript.ts'
import type { Row } from '../src/rows.ts'

afterEach(cleanup)

function props(overrides: Partial<AppProps> = {}): AppProps {
  return {
    files: { query: undefined, entries: [], loading: false, error: undefined }, onReferenceQuery: () => {},
    completion: { entries: [], loading: false, error: undefined }, completionLimit: 8,
    committed: emptyTranscript, live: [], pending: [], status: 'idle', stopping: false,
    command: undefined, notice: undefined, interaction: undefined, todos: undefined,
    model: 'mock/model', cwd: '/workspace', sessionId: 'session-live', copy: dictionaries.en, context: undefined,
    onSubmit: vi.fn(), onCancel: vi.fn(), onInterrupt: vi.fn(), onAnswer: vi.fn(), ...overrides,
  }
}

/** Rendered rows, which is what the terminal scrolls — not the lines fed in. */
const heightOf = (frame: string | undefined): number => (frame ?? '').split('\n').length

/**
 * Rows the live region reserves, read back from the render rather than
 * recomputed.
 *
 * `useWindowSize()` falls back to the host terminal's height when the stream
 * reports none, so a budget computed here from a fixed size would agree with
 * the component on a developer's terminal and disagree in CI.
 */
function reservation(state: AppProps): number {
  const ui = render(<App {...state} status="idle" />)
  const idle = heightOf(ui.lastFrame())
  ui.rerender(<App {...state} status="running" />)
  const running = heightOf(ui.lastFrame())
  cleanup()
  return running - idle
}

/** One streaming answer, arriving a line at a time. */
const streamed = (lines: number): Row[] => [{
  kind: 'assistant',
  text: Array.from({ length: lines }, (_, index) => `streamed line ${index}`).join('\n'),
}]

describe('live region', () => {
  it('holds one height from the first chunk to the last while a turn runs', () => {
    const state = props({ status: 'running' })
    const ui = render(<App {...state} />)
    const heights = [heightOf(ui.lastFrame())]
    for (let lines = 1; lines <= 24; lines++) {
      ui.rerender(<App {...state} live={streamed(lines)} />)
      heights.push(heightOf(ui.lastFrame()))
    }
    expect(new Set(heights).size).toBe(1)
  })

  it('reserves the live budget while running and releases it at rest', () => {
    const state = props()
    expect(reservation(state)).toBeGreaterThan(0)
    const ui = render(<App {...state} />)
    const idle = heightOf(ui.lastFrame())
    ui.rerender(<App {...state} live={streamed(2)} />)
    expect(heightOf(ui.lastFrame())).toBe(idle + 2)
  })

  it('keeps the newest lines when the stream outgrows its budget', () => {
    const live = reservation(props())
    const ui = render(<App {...props({ status: 'running', live: streamed(live + 5) })} />)
    const frame = ui.lastFrame() ?? ''
    expect(frame).toContain(`streamed line ${live + 4}`)
    expect(frame).not.toContain('streamed line 0')
  })

  it('holds its height when a streamed line wraps past the prose measure', () => {
    // Counting lines rather than rendered rows loses exactly here: one line of
    // prose past the measure occupies two rows, so a padded list grows while a
    // held box does not.
    const state = props({ status: 'running' })
    const ui = render(<App {...state} />)
    const reserved = heightOf(ui.lastFrame())
    const long = 'wrapped '.repeat(40).trim()
    ui.rerender(<App {...state} live={[{ kind: 'assistant', text: [long, long, long].join('\n') }]} />)
    expect(heightOf(ui.lastFrame())).toBe(reserved)
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

  it('holds the chrome in place however long a notice runs', () => {
    // The status line and composer are the two rows the user aims at. Anything
    // drawn above them that grows with harness-owned content moves them down
    // the screen mid-read, and a notice long enough clears the screen outright.
    const state = props()
    const ui = render(<App {...state} />)
    const bare = heightOf(ui.lastFrame())
    const heights = [1, 3, 8, 40, 400].map(lines => {
      ui.rerender(<App {...state} notice={Array.from({ length: lines }, (_, index) => `notice line ${index}`).join('\n')} />)
      return heightOf(ui.lastFrame()) - bare
    })
    expect(Math.max(...heights)).toBeLessThanOrEqual(NOTICE_BUDGET)
    // A short notice still costs only what it uses: the bound is a ceiling, not
    // a reservation, so one line of feedback does not leave five blank rows.
    expect(heights[0]).toBe(1)
  })

  it('says how many notice lines it left out rather than dropping them silently', () => {
    const ui = render(<App {...props({ notice: Array.from({ length: 40 }, (_, index) => `line ${index}`).join('\n') })} />)
    expect(ui.lastFrame()).toContain(dictionaries.en.moreLines)
    expect(ui.lastFrame()).toContain('line 0')
    expect(ui.lastFrame()).not.toContain('line 39')
  })

  it('charges the chrome the rows it actually draws', () => {
    // CHROME_ROWS is the denominator of every other budget, so it is measured
    // against the component rather than kept in step by hand.
    const ui = render(<App {...props()} />)
    const heading = 1
    expect(heightOf(ui.lastFrame())).toBe(heading + CHROME_ROWS)
  })
})
