/** Status-line reporting of the model, harness-owned context occupancy, and billed tokens. */
import React from 'react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { cleanup, render } from '../../../tests/render.tsx'
import { App, type AppProps } from '../src/app.tsx'
import { Tasks } from '../src/tasks.tsx'
import { emptyTranscript } from '../src/transcript.ts'
import { dictionaries } from '../src/copy.ts'
import { renderToString } from 'ink'
import stringWidth from 'string-width'

afterEach(cleanup)

const statusRow = (frame: string | undefined) => (frame ?? '').split('\n').findLast(line => line.startsWith('  ')) ?? ''

it.each(['en', 'zh'] as const)('shows the subagent entry on the status line in %s', async locale => {
  const ui = render(<App {...props({ copy: dictionaries[locale], subagents: [
    { id: 'child-1', label: 'Review tests', state: 'working', detail: 'Continuable', inspectable: true },
    { id: 'child-2', label: 'Check types', state: 'saved', outcome: 'completed', detail: 'One-shot', inspectable: true },
    { id: 'child-3', label: 'Review security', state: 'saved', outcome: 'failed', detail: 'Continuable', inspectable: true },
    { id: 'child-4', label: 'Review docs', state: 'saved', outcome: 'stopped', detail: 'Continuable', inspectable: true },
  ] })} />)
  expect(statusRow(ui.lastFrame())).toContain(`↓ ${dictionaries[locale].subagentsTitle}: 4 · 1 ${dictionaries[locale].subagentWorking}`)
  expect(ui.lastFrame()).not.toContain('Review tests')
  await expect(ui.lastFrame() + '\n').toMatchFileSnapshot(`./expected/subagents.${locale}.txt`)
})

const plan = [
  { text: 'Read startup', status: 'completed' },
  { text: 'Thread the home', status: 'in_progress' },
  { text: 'Test it', status: 'pending' },
] as const

it('folds the task list into one row with progress, the current task, and its key', () => {
  const frame = render(<App {...props({ todos: plan })} />).lastFrame() ?? ''
  const row = frame.split('\n').find(line => line.startsWith('Tasks'))!
  expect(row).toMatch(/^Tasks {2}━━━━──────── {2}1\/3 · ▸ Thread the home +Ctrl\+T$/)
  expect(frame).not.toContain('Read startup')
  expect(frame).not.toContain('Test it')
})

it('names the next open task when none is in progress, and leaves once all are done', () => {
  const draw = (todos: Parameters<typeof Tasks>[0]['todos'], columns = 60) =>
    renderToString(<Tasks todos={todos} copy={dictionaries.en} columns={columns} hint="Ctrl+T" />, { columns })
  expect(draw([{ text: 'Read startup', status: 'completed' }, { text: 'Test it', status: 'pending' }]))
    .toMatch(/1\/2 · □ Test it +Ctrl\+T$/)
  expect(draw(plan.map(item => ({ ...item, status: 'completed' as const })))).toBe('')
  // The key gives way before the task is cut below a few cells.
  expect(draw(plan, 44)).toBe('Tasks  ━━━━────────  1/3 · ▸ Thread…  Ctrl+T')
  expect(draw(plan, 40)).not.toContain('Ctrl+T')
  expect(draw(plan, 40)).toContain('▸ Thread')
  for (const columns of [1, 12, 24, 40]) expect(stringWidth(draw(plan, columns)), `${columns}`).toBeLessThanOrEqual(columns)
})

it('selects the task row from the composer and opens the full list', async () => {
  const onSubmit = vi.fn()
  const ui = render(<App {...props({ todos: plan, onSubmit })} />)
  ui.stdin.write('\x1b[A')
  await vi.waitFor(() => expect(ui.lastFrame()).toMatch(/> Tasks .* Enter opens/))
  ui.stdin.write('\r')
  await vi.waitFor(() => expect(ui.lastFrame()).toContain(' Tasks 1/3 '))
  expect(ui.lastFrame()).toMatch(/━+─+ {2}1\/3 done · 1 in progress · 1 left/)
  for (const task of ['✓ 1 Read startup', '▸ 2 Thread the home', '□ 3 Test it']) expect(ui.lastFrame()).toContain(task)
  ui.stdin.write('\x1b')
  await vi.waitFor(() => expect(ui.lastFrame()).not.toContain(dictionaries.en.sheetClose))
  expect(ui.lastFrame()).toMatch(/^Tasks .*Ctrl\+T$/m)
  expect(onSubmit).not.toHaveBeenCalled()
})

it('walks Up from the goal to the task row, Down back, and opens the list with Ctrl+T over a draft', async () => {
  const goal = { objective: 'Ship it', phase: 'active' as const, armed: true, rounds: 2, maxRounds: 8 }
  const ui = render(<App {...props({ todos: plan, goal })} />)
  ui.stdin.write('\x1b[A')
  await vi.waitFor(() => expect(ui.lastFrame()).toContain('> ● Goal active'))
  ui.stdin.write('\x1b[A')
  await vi.waitFor(() => expect(ui.lastFrame()).toMatch(/> Tasks /))
  expect(ui.lastFrame()).not.toContain('> ● Goal active')
  ui.stdin.write('\x1b[B')
  await vi.waitFor(() => expect(ui.lastFrame()).toContain('> ● Goal active'))
  ui.stdin.write('\x1b[B')
  await vi.waitFor(() => expect(ui.lastFrame()).toContain('Ctrl+O ● Goal active'))
  ui.stdin.write('Unsent draft')
  await vi.waitFor(() => expect(ui.lastFrame()).toContain('> Unsent draft▌'))
  ui.stdin.write('\x14')
  await vi.waitFor(() => expect(ui.lastFrame()).toContain('1/3 done'))
  ui.stdin.write('\x1b')
  await vi.waitFor(() => expect(ui.lastFrame()).not.toContain(dictionaries.en.sheetClose))
  expect(ui.lastFrame()).toContain('> Unsent draft▌')
})

function props(overrides: Partial<AppProps> = {}): AppProps {
  return {
    files: { query: undefined, entries: [], loading: false, error: undefined }, onReferenceQuery: () => {},
    completion: { entries: [], loading: false, error: undefined }, completionLimit: 8, resultLines: 8,
    committed: emptyTranscript, live: [], pending: [], status: 'idle', stopping: false,
    command: undefined, notice: undefined, interaction: undefined, todos: undefined,
    model: 'mock/model', cwd: '/workspace', sessionId: 'session-test',
    copy: dictionaries.en, frame: 'round', quitting: false, context: undefined,
    onSubmit: vi.fn(), onCancel: vi.fn(), onInterrupt: vi.fn(), onAnswer: vi.fn(), ...overrides,
  }
}

describe('permission boundary', () => {
  it.each(['en', 'zh'] as const)('follows the supplied session projection in %s', locale => {
    const copy = dictionaries[locale]
    const ui = render(<App {...props({ copy })} />)
    expect(ui.lastFrame()).not.toContain(copy.permission)
    for (const permission of ['workspace-write', 'read-only', 'danger-full-access', 'auto', 'custom']) {
      ui.rerender(<App {...props({ copy, permission })} />)
      expect(ui.lastFrame()).toContain(`${copy.permission} ${permission}`)
    }
  })

  it('shows a child boundary without borrowing the parent permission', () => {
    const inspection = { sessionId: 'child', label: 'Review', committed: emptyTranscript,
      live: [], status: 'idle' as const, model: 'mock/child-model' }
    const ui = render(<App {...props({ permission: 'danger-full-access', thinkingLevel: 'high', inspection })} />)
    expect(ui.lastFrame()).not.toContain('Access')
    expect(ui.lastFrame()).not.toContain('Think high')
    ui.rerender(<App {...props({ permission: 'danger-full-access', thinkingLevel: 'high',
      inspection: { ...inspection, permission: 'custom', thinkingLevel: 'low' } })} />)
    expect(ui.lastFrame()).toContain('Access custom')
    expect(ui.lastFrame()).toContain('Think low')
    expect(ui.lastFrame()).not.toContain('danger-full-access')
    ui.rerender(<App {...props({ permission: 'danger-full-access', thinkingLevel: 'high' })} />)
    expect(ui.lastFrame()).toContain('Access danger-full-access')
    expect(ui.lastFrame()).toContain('Think high')
  })
})

it('shows only the inspected child’s context and usage, then restores the parent’s', () => {
  const parent = props({ context: { used: 9_000, window: 128_000 }, usage: { input: 9000, output: 400 } })
  const child = { sessionId: 'child', label: 'Review', committed: emptyTranscript,
    live: [], status: 'idle' as const, model: 'mock/child-model' }
  const ui = render(<App {...parent} inspection={child} />)
  expect(ui.lastFrame()).not.toContain('Context:')
  expect(ui.lastFrame()).not.toContain('in 9k')
  ui.rerender(<App {...parent} inspection={{ ...child, context: { used: 750, window: 8192 },
    usage: { input: 700, output: 50 } }} />)
  expect(ui.lastFrame()).toContain('Context: ~750/8.2k (9%)')
  expect(ui.lastFrame()).toContain('in 700  out 50')
  expect(ui.lastFrame()).not.toContain('Context: ~9k/128k')
  ui.rerender(<App {...parent} />)
  expect(ui.lastFrame()).toContain('Context: ~9k/128k (7%)')
})

describe('thinking level', () => {
  it.each(['en', 'zh'] as const)('labels a selected effort in %s and omits unknown levels', locale => {
    const copy = dictionaries[locale]
    const ui = render(<App {...props({ copy, permission: 'workspace-write' })} />)
    expect(ui.lastFrame()).not.toContain(copy.thinking)
    ui.rerender(<App {...props({ copy, permission: 'workspace-write', thinkingLevel: 'high' })} />)
    expect(ui.lastFrame()).toContain(`${copy.thinking} high`)
    ui.rerender(<App {...props({ copy, permission: 'workspace-write', thinkingLevel: copy.providerDefault })} />)
    expect(ui.lastFrame()).toContain(`${copy.thinking} ${copy.providerDefault}`)
  })
})

describe('context occupancy', () => {
  it('reports used, capacity, and percent once the meter has measured a request', () => {
    const ui = render(<App {...props({ context: { used: 12_340, window: 1_000_000 } })} />)
    expect(ui.lastFrame()).toContain('Context: ~12.3k/1M (1%)')
  })

  it('shows nothing before the meter reports', () => {
    // Both meter fields are optional. A session reports nothing until a request
    // measures it, and a model with no exact capacity never reports a window.
    // A fraction of an unknown whole would be worse than silence.
    const ui = render(<App {...props()} />)
    expect(ui.lastFrame()).not.toContain('Context')
  })

  it('labels the figure in the active locale', () => {
    const ui = render(<App {...props({ copy: dictionaries.zh, context: { used: 500, window: 128_000 } })} />)
    expect(ui.lastFrame()).toContain('上下文: ~500/128k (0%)')
  })

  it.each(['en', 'zh'] as const)('shows the discard action beside retained input in %s', async locale => {
    const copy = dictionaries[locale]
    const ui = render(<App {...props({ copy, context: { used: 12_340, window: 128_000 }, pending: [{ id: 'pending-1', target: 'next-step', text: 'Queued direction' }] })} />)
    expect(ui.lastFrame()).toContain('Queued direction')
    expect(ui.lastFrame()).toContain(copy.pendingHelp)
    expect(ui.lastFrame()).toContain('/clear-pending')
    await expect(ui.lastFrame() + '\n').toMatchFileSnapshot(`./expected/pending-context.${locale}.txt`)
  })
})

it.each([
  [{ active: true, pending: false }, dictionaries.en.planActive],
  [{ active: false, pending: true }, dictionaries.en.planEntryPending],
  [{ active: true, pending: true }, dictionaries.en.planExitPending],
] as const)('shows the projected plan state %s in the status line', (plan, label) => {
  const ui = render(<App {...props({ plan })} />)
  expect(ui.lastFrame()).toContain(label)
})

it('omits the plan indicator when the profile has no plan projection', () => {
  const ui = render(<App {...props()} />)
  expect(ui.lastFrame()).not.toContain(dictionaries.en.planEntryPending)
  expect(ui.lastFrame()).not.toContain(dictionaries.en.planActive)
})

describe('model and billed tokens', () => {
  it('names the model where the state word was, and no token fields before a request reports', () => {
    const ui = render(<App {...props({ status: 'running' })} />)
    const row = statusRow(ui.lastFrame())
    expect(row).toMatch(/^ {2}Model: model {2}/)
    expect(row).not.toContain(dictionaries.en.working)
    expect(row).not.toContain(' in ')
  })

  it.each(['en', 'zh'] as const)('reports input, output, and the cache hit the provider reported in %s', locale => {
    const copy = dictionaries[locale]
    const ui = render(<App {...props({ copy, context: { used: 500, window: 128_000 }, usage: { input: 12_340, output: 1_200, cached: 10_000 } })} />)
    expect(statusRow(ui.lastFrame())).toContain(
      `${copy.model}: model  ${locale === 'en' ? 'Context' : '上下文'}: ~500/128k (0%)  ${copy.tokensIn} 12.3k  ${copy.tokensOut} 1.2k  ${copy.cacheHit} 81%  /workspace`)
  })

  it('leaves the cache field out for a provider that reports no cache traffic', () => {
    const ui = render(<App {...props({ usage: { input: 900, output: 100 } })} />)
    const row = statusRow(ui.lastFrame())
    expect(row).toContain('in 900  out 100  /workspace')
    expect(row).not.toContain(dictionaries.en.cacheHit)
  })
})


const child = { id: 'child', label: 'Review', state: 'working', detail: 'Continuable', inspectable: true } as const

it('opens the agents sheet from the shortcut, inspects the child, and preserves the parent draft', async () => {
  const onInspectSubagent = vi.fn()
  const onCancel = vi.fn()
  const onSubmit = vi.fn()
  const state = props({ onInspectSubagent, onCancel, onSubmit, subagents: [child] })
  const ui = render(<App {...state} />)
  ui.stdin.write('Unsent parent draft')
  await vi.waitFor(() => expect(ui.lastFrame()).toContain('Unsent parent draft'))
  ui.stdin.write('\x07')
  await vi.waitFor(() => expect(ui.lastFrame()).toContain(' Subagents 1 · 1 Working '))
  expect(ui.lastFrame()).toContain('▸ ● Review  Working')
  expect(ui.lastFrame()).toContain('Continuable · child')
  ui.stdin.write('\r')
  await vi.waitFor(() => expect(onInspectSubagent).toHaveBeenCalledWith('child'))
  ui.rerender(<App {...state} inspection={{ sessionId: 'child', label: 'Review', committed: emptyTranscript,
    live: [{ kind: 'assistant', text: 'Checking the child session' }], status: 'running', model: 'mock/child' }} />)
  await vi.waitFor(() => expect(ui.lastFrame()).toContain(dictionaries.en.subagentBack))
  ui.stdin.write('do not submit\r')
  ui.stdin.write('\x1b')
  await vi.waitFor(() => expect(onCancel).toHaveBeenCalledOnce())
  expect(onSubmit).not.toHaveBeenCalled()
  ui.rerender(<App {...state} />)
  await vi.waitFor(() => expect(ui.lastFrame()).toContain('Unsent parent draft'))
})

it('selects the status-line subagents entry with Down, and moves the pointer past a child with no transcript', async () => {
  const onInspectSubagent = vi.fn()
  const onSubmit = vi.fn()
  const remote = { id: 'run-1', label: 'remote', state: 'working', detail: 'Remote run', inspectable: false } as const
  const saved = { id: 'old', label: 'Earlier', state: 'saved', outcome: 'completed', detail: 'One-shot', inspectable: true } as const
  const ui = render(<App {...props({ onInspectSubagent, onSubmit, subagents: [remote, saved] })} />)
  ui.stdin.write('\x1b[B')
  await vi.waitFor(() => expect(statusRow(ui.lastFrame())).toContain('> Subagents: 2 · 1 Working · Enter opens'))
  ui.stdin.write('\r')
  // The pointer starts on the first child it can open.
  await vi.waitFor(() => expect(ui.lastFrame()).toContain('▸ ○ Earlier  Completed · Saved'))
  expect(ui.lastFrame()).toContain('Remote run · run-1 · No local transcript available')
  ui.stdin.write('\x1b[A')
  await vi.waitFor(() => expect(ui.lastFrame()).toContain('▸ ● remote'))
  // Enter on a child it cannot open keeps the sheet.
  ui.stdin.write('\r')
  await new Promise(resolve => setTimeout(resolve, 20))
  expect(onInspectSubagent).not.toHaveBeenCalled()
  ui.stdin.write('\x1b[B\r')
  await vi.waitFor(() => expect(onInspectSubagent).toHaveBeenCalledWith('old'))
  expect(onSubmit).not.toHaveBeenCalled()
})

it('cycles tasks, agents, and the goal on Ctrl+T and closes after the last; Tab wraps both ways', async () => {
  const goal = { objective: 'Ship it', phase: 'active' as const, armed: true, rounds: 2, maxRounds: 8 }
  const ui = render(<App {...props({ todos: plan, goal, subagents: [child] })} />)
  const current = (): string | undefined => {
    const frame = ui.lastFrame()!
    if (!frame.includes(dictionaries.en.sheetClose)) return undefined
    return frame.includes('Ctrl+T next') && frame.includes('1/3 done') ? 'tasks'
      : frame.includes('Review  Working') ? 'agents' : frame.includes('Objective') ? 'goal' : 'unknown'
  }
  ui.stdin.write('\x14')
  await vi.waitFor(() => expect(current()).toBe('tasks'))
  // Every view is named on the strip, the open one included.
  expect(ui.lastFrame()).toMatch(/ Tasks 1\/3 {3}Subagents 1 · 1 Working {3}Goal /)
  ui.stdin.write('\x14')
  await vi.waitFor(() => expect(current()).toBe('agents'))
  ui.stdin.write('\x14')
  await vi.waitFor(() => expect(current()).toBe('goal'))
  expect(ui.lastFrame()).toMatch(/━+─+ {2}round 2\/8/)
  ui.stdin.write('\x14')
  await vi.waitFor(() => expect(current()).toBeUndefined())
  ui.stdin.write('\x14')
  await vi.waitFor(() => expect(current()).toBe('tasks'))
  ui.stdin.write('\x1b[Z')
  await vi.waitFor(() => expect(current()).toBe('goal'))
  ui.stdin.write('\t')
  await vi.waitFor(() => expect(current()).toBe('tasks'))
  // A view's own key closes it when it is the one open.
  ui.stdin.write('\x0f')
  await vi.waitFor(() => expect(current()).toBe('goal'))
  ui.stdin.write('\x0f')
  await vi.waitFor(() => expect(current()).toBeUndefined())
})

it('keeps Down in the composer while drafting', async () => {
  const onInspectSubagent = vi.fn()
  const ui = render(<App {...props({ onInspectSubagent, subagents: [
    { id: 'child', label: 'Review', state: 'saved', detail: 'Continuable', inspectable: true },
  ] })} />)
  ui.stdin.write('draft\x1b[B')
  await vi.waitFor(() => expect(ui.lastFrame()).toContain('draft'))
  expect(statusRow(ui.lastFrame())).toContain('↓ Subagents: 1')
  expect(ui.lastFrame()).not.toContain(dictionaries.en.sheetClose)
})

it('counts working children only while some are working', () => {
  const ui = render(<App {...props({ subagents: [
    { id: 'a', label: 'Review', state: 'saved', outcome: 'completed', detail: 'One-shot', inspectable: true },
    { id: 'b', label: 'Check', state: 'live', detail: 'Continuable', inspectable: true },
  ] })} />)
  expect(statusRow(ui.lastFrame())).toContain('↓ Subagents: 2  ')
  expect(statusRow(ui.lastFrame())).not.toContain('Working')
})

it('steps thinking on Shift-Tab without touching the draft or a completion Tab would take', async () => {
  const onCycleThinking = vi.fn()
  const ui = render(<App {...props({ onCycleThinking })} />)
  // An open completion menu, whose Tab would complete the draft.
  ui.stdin.write('/mo')
  await vi.waitFor(() => expect(ui.lastFrame()).toContain(dictionaries.en.tabCompletes))
  ui.stdin.write('\x1b[Z')
  await vi.waitFor(() => expect(onCycleThinking).toHaveBeenCalledOnce())
  expect(ui.lastFrame()).toMatch(/> \/mo▌/)
})

it('names an available update in the status line, after every reading of the session and before the path', () => {
  const frame = render(<App {...props({ update: { version: '0.2.0', installed: false }, usage: { input: 1200, output: 300 } })} />).lastFrame()!
  const status = frame.split('\n').find(line => line.includes('Model:'))!
  expect(status).toMatch(/update\s+v0\.2\.0 · \/update/)
  // Last of the bounded fields, so it is the first a narrow line gives up.
  expect(status.indexOf('out')).toBeLessThan(status.indexOf('update'))
  expect(status.indexOf('update')).toBeLessThan(status.indexOf('/workspace'))
})

it('asks for a restart once the newer release is installed', () => {
  const frame = render(<App {...props({ update: { version: '0.2.0', installed: true } })} />).lastFrame()!
  const status = frame.split('\n').find(line => line.includes('Model:'))!
  expect(status).toContain(`v0.2.0 · ${dictionaries.en.updateRestart}`)
})
