/** Status-line reporting of harness-owned context occupancy. */
import React from 'react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { cleanup, render } from 'ink-testing-library'
import { App, type AppProps } from '../src/app.tsx'
import { dictionaries } from '../src/copy.ts'

afterEach(cleanup)

function props(overrides: Partial<AppProps> = {}): AppProps {
  return {
    committed: [], live: [], pending: [], status: 'idle', stopping: false,
    command: undefined, notice: undefined, interaction: undefined,
    model: 'mock/model', cwd: '/workspace', sessionId: 'session-test',
    copy: dictionaries.en, context: undefined,
    onSubmit: vi.fn(), onCancel: vi.fn(), onInterrupt: vi.fn(), onAnswer: vi.fn(), ...overrides,
  }
}

describe('context occupancy', () => {
  it('reports used, capacity, and percent once the meter has measured a request', () => {
    const ui = render(<App {...props({ context: { used: 12_340, window: 1_000_000 } })} />)
    expect(ui.lastFrame()).toContain('Context: 12.3k/1M (1%)')
  })

  it('shows nothing before the meter reports', () => {
    // Both meter fields are optional: a session reports nothing until a request
    // measures it, and a model with no exact capacity never reports a window.
    // A fraction of an unknown whole would be worse than silence.
    const ui = render(<App {...props()} />)
    expect(ui.lastFrame()).not.toContain('Context')
  })

  it('labels the figure in the active locale', () => {
    const ui = render(<App {...props({ copy: dictionaries.zh, context: { used: 500, window: 128_000 } })} />)
    expect(ui.lastFrame()).toContain('上下文: 500/128k (0%)')
  })
})
