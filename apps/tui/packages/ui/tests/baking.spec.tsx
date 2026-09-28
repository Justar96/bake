/** The `/update` loaf row: where it sits, what it draws, and that its steam moves only on the beat. */
import React, { act } from 'react'
import { afterEach, expect, it, vi } from 'vitest'
import { cleanup, render } from '../../../tests/render.tsx'
import { App, type AppProps } from '../src/app.tsx'
import { dictionaries } from '../src/copy.ts'
import { emptyTranscript } from '../src/transcript.ts'
import type { Clock } from '../src/activity.ts'
import { LOAF_FRAME_MS, loafFrame } from '../src/loaf.ts'

afterEach(cleanup)

const copy = dictionaries.en

function props(overrides: Partial<AppProps> = {}): AppProps {
  return {
    files: { query: undefined, entries: [], loading: false, error: undefined }, onReferenceQuery: () => {},
    completion: { entries: [], loading: false, error: undefined }, completionLimit: 8, resultLines: 8,
    committed: emptyTranscript, live: [], pending: [], status: 'idle', stopping: false,
    command: '/update', notice: undefined, interaction: undefined, todos: undefined,
    model: 'mock/model', cwd: '/workspace', sessionId: 'session-baking', copy, frame: 'round', quitting: false, context: undefined,
    onSubmit: vi.fn(), onCancel: vi.fn(), onInterrupt: vi.fn(), onAnswer: vi.fn(), ...overrides,
  }
}

const label = `${copy.updateDownloading} v0.2.0… 48% (4.8 / 10.0 MB)`
const loafRow = (frame: string | undefined): string | undefined => frame?.split('\n').find(row => row.includes(label))

it('draws the loaf and the step above the composer, and leaves once the install ends', () => {
  const ui = render(<App {...props({ baking: { label, level: 0.4 } })} />)
  const rows = ui.lastFrame()!.split('\n')
  const at = rows.findIndex(row => row.includes(label))
  const still = loafFrame(0, 0.4, 'unicode')
  expect(rows[at]).toBe(`${still.steam} ${still.loaf}  ${label}`)
  // Over the header and the input's rule, with the other rows around the input.
  expect(rows.slice(at + 1).some(row => /^─+$/u.test(row))).toBe(true)
  ui.rerender(<App {...props()} />)
  expect(loafRow(ui.lastFrame())).toBeUndefined()
})

it('draws the ASCII loaf where the terminal draws the classic frame', () => {
  const ui = render(<App {...props({ baking: { label, level: 1 }, frame: 'classic' })} />)
  expect(loafRow(ui.lastFrame())).toMatch(/^[ -~]{3} \(#####\) {2}/u)
})

it('moves the steam on the surface beat, and holds it still without a clock', () => {
  let time = 0
  const ticks = new Set<() => void>()
  const clock: Clock = { now: () => time, every: (_ms, tick) => { ticks.add(tick); return () => { ticks.delete(tick) } } }
  const ui = render(<App {...props({ baking: { label, level: 0.4 }, clock })} />)
  const seen = new Set<string>()
  for (let step = 0; step < 6; step++) {
    seen.add(loafRow(ui.lastFrame())!.slice(0, 3))
    time += LOAF_FRAME_MS
    for (const tick of ticks) act(tick)
  }
  expect(seen.size).toBeGreaterThan(3)
  // The row takes no timer of its own once the install is over.
  ui.rerender(<App {...props({ clock })} />)
  expect(ticks.size).toBe(0)
  const still = render(<App {...props({ baking: { label, level: 0.4 } })} />)
  expect(loafRow(still.lastFrame())!.slice(0, 3)).toBe(loafFrame(0, 0.4, 'unicode').steam)
})
