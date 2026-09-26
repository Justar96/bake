/** Goal state projected into the shared turn header. */
import React from 'react'
import { useStdout } from 'ink'
import { render as renderInk } from 'ink-testing-library'
import { afterEach, expect, it, vi } from 'vitest'
import { cleanup, render } from '../../../tests/render.tsx'
import { App, type AppProps } from '../src/app.tsx'
import { goalState, type GoalEntry } from '../src/goal.ts'
import { appendTranscript, emptyTranscript } from '../src/transcript.ts'
import { dictionaries } from '../src/copy.ts'

const copy = dictionaries.en
const active: GoalEntry = { objective: 'Ship it', phase: 'active', armed: true, rounds: 2, maxRounds: 8 }

afterEach(cleanup)

function props(goal: GoalEntry, status: AppProps['status'] = 'idle'): AppProps {
  return {
    files: { query: undefined, entries: [], loading: false, error: undefined }, onReferenceQuery: () => {},
    completion: { entries: [], loading: false, error: undefined }, completionLimit: 8, resultLines: 8,
    committed: emptyTranscript, live: [], pending: [], status, stopping: false,
    command: undefined, notice: undefined, interaction: undefined, todos: undefined,
    model: 'mock/model', cwd: '/workspace', sessionId: 'session-goal', copy, frame: 'round', quitting: false, context: undefined,
    goal, onSubmit: () => {}, onCancel: () => {}, onInterrupt: () => {}, onAnswer: () => {},
  }
}

it('keeps the goal beside turn status on one header row', () => {
  const ui = render(<App {...props(active, 'running')} />)
  const rows = ui.lastFrame()!.split('\n')
  const goalRows = rows.filter(row => row.includes(copy.goalActive))
  expect(goalRows).toHaveLength(1)
  expect(goalRows[0]).toContain('round 2/8 · Ship it')
  expect(goalRows[0]).toMatch(/….*● Goal active/)
  expect(rows).not.toContain('└ Ship it')
  ui.rerender(<App {...props(active)} />)
  expect(ui.lastFrame()!.split('\n').filter(row => row.includes(copy.goalActive))).toHaveLength(1)
})

it('projects the goal phases and their useful details', () => {
  expect(goalState(active, copy)).toMatchObject({ glyph: '●', label: copy.goalActive, details: 'round 2/8 · Ship it' })
  expect(goalState({ ...active, armed: false }, copy)).toMatchObject({ glyph: '○', label: copy.goalHeld,
    details: `${copy.goalResume} · Ship it` })
  expect(goalState({ ...active, phase: 'paused' }, copy)).toMatchObject({ glyph: '○', label: copy.goalPaused })
  expect(goalState({ ...active, phase: 'blocked', blocked: 'Round limit reached' }, copy)).toMatchObject({
    glyph: '✗', label: copy.goalBlocked, details: 'Round limit reached · Ship it',
  })
  expect(goalState({ ...active, phase: 'complete' }, copy)).toMatchObject({ glyph: '✓', label: copy.goalComplete,
    details: 'round 2/8 · Ship it' })
  expect(goalState(undefined, copy)).toBeUndefined()
})

it('truncates a long header goal and opens its complete text with Up and Enter', async () => {
  const objective = Array.from({ length: 40 }, (_, index) => `Goal line ${index}`).join('\n')
  const ui = render(<App {...props({ ...active, objective })} />)
  const initialHeight = ui.lastFrame()!.split('\n').length
  const header = ui.lastFrame()!.split('\n').find(row => row.includes(copy.goalActive))!
  expect(header).toContain('Ctrl+O ● Goal active')
  expect(header).not.toContain('Goal line 39')
  ui.stdin.write('\x1b[A')
  await vi.waitFor(() => expect(ui.lastFrame()).toContain('> ● Goal active'))
  ui.stdin.write('\r')
  await vi.waitFor(() => expect(ui.lastFrame()).toContain(`${copy.sheetScroll} · ${copy.sheetClose}`))
  expect(ui.lastFrame()).toContain('Goal line 0')
  expect(ui.lastFrame()).not.toContain('Goal line 39')
  ui.stdin.write('\x1b[F')
  await vi.waitFor(() => expect(ui.lastFrame()).toContain('Goal line 39'))
  ui.stdin.write('\x1b')
  await vi.waitFor(() => expect(ui.lastFrame()).toContain('Ctrl+O ● Goal active'))
  await vi.waitFor(() => expect(ui.lastFrame()!.split('\n')).toHaveLength(initialHeight))
})

it('recalls input history with Up before selecting the goal, then restores the unsent draft', async () => {
  const committed = appendTranscript(emptyTranscript, [{ kind: 'user', text: 'Earlier prompt' }])
  const ui = render(<App {...props(active)} committed={committed} />)
  ui.stdin.write('Unsent draft')
  await vi.waitFor(() => expect(ui.lastFrame()).toContain('> Unsent draft▌'))
  ui.stdin.write('\x1b[A')
  await vi.waitFor(() => expect(ui.lastFrame()).toContain('> Earlier prompt▌'))
  expect(ui.lastFrame()).not.toContain('> ● Goal active')
  // Past the oldest entry the goal is selected, and the composer holds the
  // draft again rather than a stale entry one Enter would rerun.
  ui.stdin.write('\x1b[A')
  await vi.waitFor(() => expect(ui.lastFrame()).toContain('> ● Goal active'))
  expect(ui.lastFrame()).toContain('> Unsent draft▌')
  ui.stdin.write('\r')
  await vi.waitFor(() => expect(ui.lastFrame()).toContain(copy.sheetClose))
  ui.stdin.write('\x1b')
  await vi.waitFor(() => expect(ui.lastFrame()).not.toContain(copy.sheetClose))
  expect(ui.lastFrame()).toContain('Ctrl+O ● Goal active')
  expect(ui.lastFrame()).toContain('> Unsent draft▌')
  // History starts again from the newest entry.
  ui.stdin.write('\x1b[A')
  await vi.waitFor(() => expect(ui.lastFrame()).toContain('> Earlier prompt▌'))
})

it('opens from a draft with Ctrl+O, shows blocked status and reason, and keeps the draft', async () => {
  const onSubmit = vi.fn()
  const onCancel = vi.fn()
  const goal = { ...active, phase: 'blocked' as const, blocked: 'Round limit reached' }
  const ui = render(<App {...props(goal)} onSubmit={onSubmit} onCancel={onCancel} />)
  ui.stdin.write('Unsent draft')
  await vi.waitFor(() => expect(ui.lastFrame()).toContain('Unsent draft'))
  ui.stdin.write('\x0f')
  await vi.waitFor(() => expect(ui.lastFrame()).toContain(`${copy.sheetScroll} · ${copy.sheetClose}`))
  expect(ui.lastFrame()).toContain('✗ Goal blocked')
  expect(ui.lastFrame()).toContain('Round limit reached')
  ui.stdin.write('ignored\r')
  await vi.waitFor(() => expect(ui.lastFrame()).toContain('Unsent draft'))
  expect(onSubmit).not.toHaveBeenCalled()
  expect(onCancel).not.toHaveBeenCalled()
})

it('shows the full goal on a short terminal where the composer leaves no room for the panel', async () => {
  function SmallTerminal({ children }: { readonly children: React.ReactNode }): React.ReactElement {
    const { stdout } = useStdout()
    Object.defineProperties(stdout, { columns: { value: 40, configurable: true }, rows: { value: 8, configurable: true } })
    return <>{children}</>
  }
  const goal = { ...active, objective: `${'long goal '.repeat(30)}END_OF_GOAL` }
  const ui = renderInk(<SmallTerminal><App {...props(goal)} /></SmallTerminal>)
  ui.stdin.write('\x1b[A\r')
  await vi.waitFor(() => expect(ui.lastFrame()).toContain(copy.sheetScroll))
  ui.stdin.write('\x1b[F')
  await vi.waitFor(() => expect(ui.lastFrame()).toContain('END_OF_GOAL'))
  ui.stdin.write('\x1b')
  await vi.waitFor(() => expect(ui.lastFrame()).toContain('Ctrl+O ● Goal active'))
})
