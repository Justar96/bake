/** Status-line reporting of the model, harness-owned context occupancy, and billed tokens. */
import React from 'react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { cleanup, render } from '../../../tests/render.tsx'
import { App, type AppProps } from '../src/app.tsx'
import { Tasks } from '../src/tasks.tsx'
import { SubagentRow } from '../src/subagents.tsx'
import { emptyTranscript } from '../src/transcript.ts'
import { dictionaries } from '../src/copy.ts'
import { renderToString } from 'ink'
import stringWidth from 'string-width'

afterEach(cleanup)

const statusRow = (frame: string | undefined) => (frame ?? '').split('\n').findLast(line => line.startsWith('  ')) ?? ''
/** The subagents' row, under the input's base rule and over the status line. */
const agentRow = (frame: string | undefined) => (frame ?? '').split('\n').find(line => /^ {2}[↓>] /.test(line)) ?? ''

it.each(['en', 'zh'] as const)('counts the children on one row under the input in %s', async locale => {
  const ui = render(<App {...props({ copy: dictionaries[locale], subagents: [
    { id: 'child-1', label: 'Review tests', state: 'working', detail: 'Continuable', inspectable: true },
    { id: 'child-2', label: 'Check types', state: 'saved', outcome: 'completed', detail: 'One-shot', inspectable: true },
    { id: 'child-3', label: 'Review security', state: 'saved', outcome: 'failed', detail: 'Continuable', inspectable: true },
    { id: 'child-4', label: 'Review docs', state: 'saved', outcome: 'stopped', detail: 'Continuable', inspectable: true },
    { id: 'child-5', label: 'Lost', state: 'issue', detail: 'Unreadable', inspectable: false },
  ] })} />)
  const copy = dictionaries[locale]
  // Names are the sheet's; the row counts what is working and what cannot be read.
  expect(agentRow(ui.lastFrame())).toBe(`  ↓ ${copy.subagentsTitle}: 5 · 1 ${copy.subagentWorking} · 1 ${copy.subagentUnreadable}`)
  expect(statusRow(ui.lastFrame())).not.toContain(dictionaries[locale].subagentsTitle)
  // The row sits between the base rule and the status line.
  const lines = (ui.lastFrame() ?? '').split('\n')
  expect(lines.at(-2)).toBe(agentRow(ui.lastFrame()))
  expect(lines.at(-3)).toMatch(/^─+$/)
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
  // Beside the task row the goal drops its own key and details; Tab reaches its sheet from any other.
  await vi.waitFor(() => expect(ui.lastFrame()).toMatch(/ ● Goal active$/m))
  expect(ui.lastFrame()).not.toContain('> ● Goal active')
  ui.stdin.write('Unsent draft')
  await vi.waitFor(() => expect(ui.lastFrame()).toContain('> Unsent draft▌'))
  ui.stdin.write('\x14')
  await vi.waitFor(() => expect(ui.lastFrame()).toContain('1/3 done'))
  ui.stdin.write('\x1b')
  await vi.waitFor(() => expect(ui.lastFrame()).not.toContain(dictionaries.en.sheetClose))
  expect(ui.lastFrame()).toContain('> Unsent draft▌')
})

it('thins the rows around the composer once tasks, a goal, and subagents are all shown', () => {
  const busy = {
    todos: plan, goal: { objective: 'Ship it', phase: 'active' as const, armed: true, rounds: 3, maxRounds: 256 },
    subagents: [{ id: 'child-1', label: 'Review tests', state: 'working' as const, detail: 'Continuable', inspectable: true }],
    context: { used: 45_000, window: 128_000 }, usage: { input: 1_234_000, output: 30_500, cached: 1_000_000 },
  }
  const frame = render(<App {...props(busy)} />).lastFrame() ?? ''
  const header = frame.split('\n').find(line => line.includes('Goal active'))!
  expect(header).not.toContain('Ctrl+O')
  expect(header).not.toContain('round 3/256')
  expect(header).not.toContain('Ship it')
  expect(header).toMatch(/● Goal active$/)
  const status = statusRow(frame)
  expect(agentRow(frame)).toBe('  ↓ Subagents: 1 · 1 Working')
  expect(status).toContain('ctx ~35%')
  for (const cost of ['Context:', 'in 1.2M', 'out 30.5k', 'cache hit']) expect(status).not.toContain(cost)
  expect(frame).toMatch(/^Tasks .*Ctrl\+T$/m)
  // One of them alone keeps every reading.
  const alone = render(<App {...props({ ...busy, todos: undefined, subagents: [] })} />).lastFrame() ?? ''
  expect(alone).toContain('Ctrl+O ● Goal active  round 3/256 · Ship it')
  expect(statusRow(alone)).toContain('Context: ~45k/128k (35%)  in 1.2M  out 30.5k  cache hit 81%')
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
  it.each(['en', 'zh'] as const)('names the access boundary where the session opens, not on the status line, in %s', locale => {
    const copy = dictionaries[locale]
    expect(render(<App {...props({ copy })} />).lastFrame()).not.toContain(copy.permission)
    for (const permission of ['workspace-write', 'read-only', 'danger-full-access', 'auto', 'custom']) {
      // A resumed session names it on its heading; a fresh one in the welcome block.
      const resumed = render(<App {...props({ copy, permission })} />).lastFrame() ?? ''
      expect(resumed).toContain(`${copy.session}: session-test · ${copy.permission} ${permission}`)
      expect(statusRow(resumed)).not.toContain(copy.permission)
      const fresh = render(<App {...props({ copy, permission, version: '1.2.3' })} />).lastFrame() ?? ''
      expect(fresh).toMatch(new RegExp(`│ ${copy.permission} ${permission} +│`))
      expect(statusRow(fresh)).not.toContain(copy.permission)
    }
  })

  it('names a child boundary without borrowing the parent permission', () => {
    const inspection = { sessionId: 'child', label: 'Review', committed: emptyTranscript,
      live: [], status: 'idle' as const, model: 'mock/child-model' }
    const bare = render(<App {...props({ permission: 'danger-full-access', thinkingLevel: 'high', inspection })} />).lastFrame() ?? ''
    expect(bare).not.toContain('Access')
    expect(bare).not.toContain('Think high')
    const own = render(<App {...props({ permission: 'danger-full-access', thinkingLevel: 'high',
      inspection: { ...inspection, permission: 'custom', thinkingLevel: 'low' } })} />).lastFrame() ?? ''
    expect(own).toContain('Parent: session-test > child · Access custom')
    expect(own).toContain('Think low')
    expect(own).not.toContain('danger-full-access')
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

it('selects the subagents row with Down, and moves the pointer past a child with no transcript', async () => {
  const onInspectSubagent = vi.fn()
  const onSubmit = vi.fn()
  const remote = { id: 'run-1', label: 'remote', state: 'working', detail: 'Remote run', inspectable: false } as const
  const saved = { id: 'old', label: 'Earlier', state: 'saved', outcome: 'completed', detail: 'One-shot', inspectable: true } as const
  const ui = render(<App {...props({ onInspectSubagent, onSubmit, subagents: [remote, saved] })} />)
  ui.stdin.write('\x1b[B')
  await vi.waitFor(() => expect(agentRow(ui.lastFrame())).toBe('  > Subagents: 2 · Enter opens'))
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

it('toggles each sheet on its own key and cycles the open one with Tab both ways', async () => {
  const goal = { objective: 'Ship it', phase: 'active' as const, armed: true, rounds: 2, maxRounds: 8 }
  const ui = render(<App {...props({ todos: plan, goal, subagents: [child] })} />)
  const current = (): string | undefined => {
    const frame = ui.lastFrame()!
    if (!frame.includes(dictionaries.en.sheetClose)) return undefined
    return frame.includes('1/3 done') ? 'tasks'
      : frame.includes('Review  Working') ? 'agents' : frame.includes('Objective') ? 'goal' : 'unknown'
  }
  // Ctrl+T opens the task sheet and, pressed again, closes it rather than moving on.
  ui.stdin.write('\x14')
  await vi.waitFor(() => expect(current()).toBe('tasks'))
  // Every view is named on the strip, the open one included, and Tab is named as the way between them.
  expect(ui.lastFrame()).toMatch(/ Tasks 1\/3 {3}Subagents 1 · 1 Working {3}Goal /)
  expect(ui.lastFrame()).toContain('Tab next')
  ui.stdin.write('\x14')
  await vi.waitFor(() => expect(current()).toBeUndefined())
  // Ctrl+G does the same for the subagents.
  ui.stdin.write('\x07')
  await vi.waitFor(() => expect(current()).toBe('agents'))
  ui.stdin.write('\x07')
  await vi.waitFor(() => expect(current()).toBeUndefined())
  // Inside a sheet, Tab steps forward and wraps; Shift-Tab steps back.
  ui.stdin.write('\x14')
  await vi.waitFor(() => expect(current()).toBe('tasks'))
  ui.stdin.write('\t')
  await vi.waitFor(() => expect(current()).toBe('agents'))
  ui.stdin.write('\t')
  await vi.waitFor(() => expect(current()).toBe('goal'))
  expect(ui.lastFrame()).toMatch(/━+─+ {2}round 2\/8/)
  ui.stdin.write('\t')
  await vi.waitFor(() => expect(current()).toBe('tasks'))
  ui.stdin.write('\x1b[Z')
  await vi.waitFor(() => expect(current()).toBe('goal'))
  // Another sheet's key switches to it; its own key closes it.
  ui.stdin.write('\x14')
  await vi.waitFor(() => expect(current()).toBe('tasks'))
  ui.stdin.write('\x0f')
  await vi.waitFor(() => expect(current()).toBe('goal'))
  ui.stdin.write('\x0f')
  await vi.waitFor(() => expect(current()).toBeUndefined())
})

it('leaves Ctrl+T alone without a task list', async () => {
  const ui = render(<App {...props({ subagents: [child] })} />)
  ui.stdin.write('\x14')
  ui.stdin.write('x')
  await vi.waitFor(() => expect(ui.lastFrame()).toContain('> x▌'))
  expect(ui.lastFrame()).not.toContain(dictionaries.en.sheetClose)
})

it('keeps Down in the composer while drafting', async () => {
  const onInspectSubagent = vi.fn()
  const ui = render(<App {...props({ onInspectSubagent, subagents: [
    { id: 'child', label: 'Review', state: 'saved', detail: 'Continuable', inspectable: true },
  ] })} />)
  ui.stdin.write('draft\x1b[B')
  await vi.waitFor(() => expect(ui.lastFrame()).toContain('draft'))
  expect(agentRow(ui.lastFrame())).toContain('↓ Subagents: 1')
  expect(ui.lastFrame()).not.toContain(dictionaries.en.sheetClose)
})

it('keeps the subagents on one row however many there are, and at any width', () => {
  const subagents = Array.from({ length: 12 }, (_, index) => ({
    id: `c${index}`, label: `Child number ${index}`, state: index < 7 ? 'working' as const : 'live' as const,
    detail: 'Continuable', inspectable: true }))
  const frame = render(<App {...props({ subagents })} />).lastFrame() ?? ''
  expect(agentRow(frame)).toBe('  ↓ Subagents: 12 · 7 Working')
  expect(frame).not.toContain('Child number')
  for (const columns of [8, 20, 40]) {
    const row = renderToString(<SubagentRow entries={subagents} copy={dictionaries.en} columns={columns} />, { columns })
    expect(row.split('\n'), `${columns}`).toHaveLength(1)
    expect(stringWidth(row), `${columns}`).toBeLessThanOrEqual(columns)
  }
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
