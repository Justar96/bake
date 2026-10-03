/** The `/update` row: where it sits, what it draws, and that it moves only on the beat. */
import React, { act } from 'react'
import { afterEach, expect, it, vi } from 'vitest'
import { cleanup, render } from '../../../tests/render.tsx'
import { App, type AppProps } from '../src/app.tsx'
import { dictionaries } from '../src/copy.ts'
import { emptyTranscript } from '../src/transcript.ts'
import type { Clock } from '../src/activity.ts'
import { orbit } from '../src/install-progress.ts'

afterEach(cleanup)

const copy = dictionaries.en

function props(overrides: Partial<AppProps> = {}): AppProps {
  return {
    files: { query: undefined, entries: [], loading: false, error: undefined }, onReferenceQuery: () => {},
    completion: { entries: [], loading: false, error: undefined }, completionLimit: 8, resultLines: 8,
    committed: emptyTranscript, live: [], pending: [], status: 'idle', stopping: false,
    command: '/update', notice: undefined, interaction: undefined,
    model: 'mock/model', cwd: '/workspace', sessionId: 'session-installing', copy, frame: 'round', quitting: false, context: undefined,
    onSubmit: vi.fn(), onCancel: vi.fn(), onInterrupt: vi.fn(), onAnswer: vi.fn(), ...overrides,
  }
}

const label = `${copy.updateDownloading} v0.2.0`
const step = { label, fraction: 0.48, detail: '4.8 / 10.0 MB' }
const installRow = (frame: string | undefined): string | undefined => frame?.split('\n').find(row => row.includes(label))

it('draws the orbit, the step, and its meter above the composer, and leaves once the install ends', () => {
  const ui = render(<App {...props({ installing: step })} />)
  const rows = ui.lastFrame()!.split('\n')
  const at = rows.findIndex(row => row.includes(label))
  expect(rows[at]).toMatch(new RegExp(`^${orbit(0, 'unicode').map(run => run.text).join('')} ${label}  [\u2501\u2578\u2500]+ {3}48%  4\\.8 / 10\\.0 MB$`, 'u'))
  // Over the header and the input's rule, with the other rows around the input.
  expect(rows.slice(at + 1).some(row => /^─+$/u.test(row))).toBe(true)
  ui.rerender(<App {...props()} />)
  expect(installRow(ui.lastFrame())).toBeUndefined()
})

it('runs the comet without a percentage while the step has no length', () => {
  const ui = render(<App {...props({ installing: { label } })} />)
  const row = installRow(ui.lastFrame())!
  expect(row).toMatch(/\u2501/u)
  expect(row).not.toContain('%')
})

it('draws ASCII where the terminal draws the classic frame', () => {
  const ui = render(<App {...props({ installing: step, frame: 'classic' })} />)
  expect(installRow(ui.lastFrame())).toMatch(/^[ -~]+$/u)
})

it('moves on the surface beat, and holds still without a clock', () => {
  let time = 0
  const ticks = new Set<() => void>()
  const clock: Clock = { now: () => time, every: (_ms, tick) => { ticks.add(tick); return () => { ticks.delete(tick) } } }
  const ui = render(<App {...props({ installing: step, clock })} />)
  const seen = new Set<string>()
  for (let beat = 0; beat < 6; beat++) {
    seen.add(installRow(ui.lastFrame())!)
    time += 150
    for (const tick of ticks) act(tick)
  }
  expect(seen.size).toBeGreaterThan(3)
  // The row takes no timer of its own once the install is over.
  ui.rerender(<App {...props({ clock })} />)
  expect(ticks.size).toBe(0)
  const still = render(<App {...props({ installing: step })} />)
  expect(installRow(still.lastFrame())!.slice(0, 2)).toBe(orbit(0, 'unicode').map(run => run.text).join(''))
})
