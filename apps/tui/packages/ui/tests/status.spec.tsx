/** Status-line reporting of the model, harness-owned context occupancy, and billed tokens. */
import React from 'react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { cleanup, render } from '../../../tests/render.tsx'
import { App, type AppProps } from '../src/app.tsx'
import { Tasks } from '../src/tasks.tsx'
import { InspectionBar, SubagentRow } from '../src/subagents.tsx'
import { emptyTranscript } from '../src/transcript.ts'
import { dictionaries } from '../src/copy.ts'
import type { GitState } from '../src/git.ts'
import { renderToString } from 'ink'
import stringWidth from 'string-width'

afterEach(cleanup)

const statusRow = (frame: string | undefined) => (frame ?? '').split('\n').findLast(line => line.startsWith('  ')) ?? ''
/** The subagents' row, under the input's base rule and over the status line, its icon in the rail. */
const agentRow = (frame: string | undefined) => (frame ?? '').split('\n').find(line => /^[↳>] (?:Subagents|子代理) /u.test(line)) ?? ''

it.each(['en', 'zh'] as const)('shows workflow progress and distinguishes its members from direct delegation in %s', async locale => {
  const copy = dictionaries[locale]
  const ui = render(<App {...props({ copy, workflows: [{ id: 'run', name: 'review', state: 'working', completed: 1, total: 2 }],
    subagents: [
      { id: 'member', label: 'Review files', state: 'working', detail: 'One-shot · Inspect', workflow: 'review', inspectable: true },
      { id: 'direct', label: 'Check docs', state: 'saved', outcome: 'completed', detail: 'Continuable', inspectable: true },
    ],
  })} />)
  expect(ui.lastFrame()).toContain(`${copy.workflowTitle} review`)
  ui.stdin.write('\x07')
  await vi.waitFor(() => expect(ui.lastFrame()).toContain(`${copy.workflowTitle} review · ${copy.subagentWorking} · 1/2 ${copy.subagentCountDone}`))
  expect(ui.lastFrame()).toContain(copy.subagentDirect)
  expect(ui.lastFrame()).toContain('One-shot · Inspect')
  await expect(ui.lastFrame() + '\n').toMatchFileSnapshot(`./expected/workflow.${locale}.txt`)
})

it('pages through workflow-only progress without a child selection', async () => {
  const workflows = Array.from({ length: 40 }, (_, index) => ({ id: `run-${index + 1}`, name: `workflow-${index + 1}`, state: 'working' as const, completed: index, total: 40 }))
  const ui = render(<App {...props({ workflows })} />)
  ui.stdin.write('\x07')
  await vi.waitFor(() => expect(ui.lastFrame()).toContain('Workflow workflow-1'))
  expect(ui.lastFrame()).not.toContain('Workflow workflow-40')
  ui.stdin.write('\x1b[6~')
  await vi.waitFor(() => expect(ui.lastFrame()).toContain('Workflow workflow-14'))
  ui.stdin.write('\x1b[F')
  await vi.waitFor(() => expect(ui.lastFrame()).toContain('Workflow workflow-40'))
})

it('opens workflow progress before any child exists and preserves the draft', async () => {
  const onInspectSubagent = vi.fn(), onSubmit = vi.fn()
  const ui = render(<App {...props({ onInspectSubagent, onSubmit,
    workflows: [{ id: 'run', name: 'audit', state: 'working', completed: 0, total: 0 }],
  })} />)
  expect(ui.lastFrame()).toContain('Workflow audit')
  ui.stdin.write('draft')
  await vi.waitFor(() => expect(ui.lastFrame()).toContain('draft'))
  ui.stdin.write('\x07')
  await vi.waitFor(() => expect(ui.lastFrame()).toContain(dictionaries.en.workflowNoChildren))
  ui.stdin.write('\x1b[B\r')
  expect(onInspectSubagent).not.toHaveBeenCalled()
  expect(onSubmit).not.toHaveBeenCalled()
  ui.stdin.write('\x1b')
  await vi.waitFor(() => expect(ui.lastFrame()).not.toContain(dictionaries.en.sheetClose))
  expect(ui.lastFrame()).toContain('draft')
  for (const columns of [8, 20, 40, 80]) {
    const row = renderToString(<SubagentRow entries={[]} workflows={[{ id: 'run', name: 'audit', state: 'working', completed: 0, total: 0 }]}
      copy={dictionaries.en} columns={columns} />, { columns })
    expect(row.split('\n')).toHaveLength(1)
    expect(stringWidth(row)).toBeLessThanOrEqual(columns)
  }
})

it.each(['en', 'zh'] as const)('counts the children on one row under the input in %s', async locale => {
  const ui = render(<App {...props({ copy: dictionaries[locale], subagents: [
    { id: 'child-1', label: 'Review tests', state: 'working', detail: 'Continuable', inspectable: true },
    { id: 'child-2', label: 'Check types', state: 'saved', outcome: 'completed', detail: 'One-shot', inspectable: true },
    { id: 'child-3', label: 'Review security', state: 'saved', outcome: 'failed', detail: 'Continuable', inspectable: true },
    { id: 'child-4', label: 'Review docs', state: 'saved', outcome: 'stopped', detail: 'Continuable', inspectable: true },
    { id: 'child-5', label: 'Lost', state: 'issue', detail: 'Unreadable', inspectable: false },
  ] })} />)
  const copy = dictionaries[locale]
  // Names are the sheet's; the row counts what is working, what is done, and what
  // cannot be read, in lowercase and without a colon, and names its key at the right edge.
  expect(agentRow(ui.lastFrame())).toMatch(new RegExp(
    `^↳ ${copy.subagentsTitle} 5 · 1 ${copy.subagentCountWorking} · 1 ${copy.subagentCountDone} · 1 ${copy.subagentUnreadable} +Ctrl\\+G$`, 'u'))
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

it('folds the task list into one row with its icon, count, progress, the current task, and its key', () => {
  const frame = render(<App {...props({ todos: plan })} />).lastFrame() ?? ''
  const row = frame.split('\n').find(line => line.startsWith('☐ Tasks'))!
  expect(row).toMatch(/^☐ Tasks 1\/3 {2}━━━━──────── {2}▸ Thread the home +Ctrl\+T$/)
  expect(frame).not.toContain('Read startup')
  expect(frame).not.toContain('Test it')
})

it('names the next open task when none is in progress, and leaves once all are done', () => {
  const draw = (todos: Parameters<typeof Tasks>[0]['todos'], columns = 60) =>
    renderToString(<Tasks todos={todos} copy={dictionaries.en} columns={columns} hint="Ctrl+T" />, { columns })
  expect(draw([{ text: 'Read startup', status: 'completed' }, { text: 'Test it', status: 'pending' }]))
    .toMatch(/^☐ Tasks 1\/2 {2}━━━━━━────── {2}□ Test it +Ctrl\+T$/)
  expect(draw(plan.map(item => ({ ...item, status: 'completed' as const })))).toBe('')
  // Below 60 columns the key goes, as the composer's hint does.
  expect(draw(plan, 59)).toBe('☐ Tasks 1/3  ━━━━────────  ▸ Thread the home')
  // Above it, the key gives way before the task is cut below a few cells.
  const long = [{ text: 'Thread the resolved home through startup and the session store', status: 'in_progress' as const }]
  expect(draw(long, 60)).toMatch(/^☐ Tasks 0\/1 {2}─{12} {2}▸ Thread the resolved \S*… {2}Ctrl\+T$/)
  expect(stringWidth(draw(long, 60))).toBe(60)
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
  expect(ui.lastFrame()).toMatch(/^☐ Tasks .*Ctrl\+T$/m)
  expect(onSubmit).not.toHaveBeenCalled()
})

it('walks Up from the goal to the task row, Down back, and opens the list with Ctrl+T over a draft', async () => {
  const goal = { objective: 'Ship it', phase: 'active' as const, armed: true, rounds: 2, maxRounds: 8 }
  const ui = render(<App {...props({ todos: plan, goal })} />)
  ui.stdin.write('\x1b[A')
  await vi.waitFor(() => expect(ui.lastFrame()).toContain('> ● Goal 2/8'))
  ui.stdin.write('\x1b[A')
  await vi.waitFor(() => expect(ui.lastFrame()).toMatch(/> Tasks /))
  expect(ui.lastFrame()).not.toContain('> ● Goal 2/8')
  ui.stdin.write('\x1b[B')
  await vi.waitFor(() => expect(ui.lastFrame()).toContain('> ● Goal 2/8'))
  ui.stdin.write('\x1b[B')
  // Beside the task row the goal keeps its key and its count.
  await vi.waitFor(() => expect(ui.lastFrame()).toMatch(/ Ctrl\+O ● Goal 2\/8$/m))
  expect(ui.lastFrame()).not.toContain('> ● Goal 2/8')
  ui.stdin.write('Unsent draft')
  await vi.waitFor(() => expect(ui.lastFrame()).toContain('> Unsent draft▌'))
  ui.stdin.write('\x14')
  await vi.waitFor(() => expect(ui.lastFrame()).toContain('1/3 done'))
  ui.stdin.write('\x1b')
  await vi.waitFor(() => expect(ui.lastFrame()).not.toContain(dictionaries.en.sheetClose))
  expect(ui.lastFrame()).toContain('> Unsent draft▌')
})

it('keeps one grammar around the composer once tasks, a goal, and subagents are all shown', () => {
  const busy = {
    todos: plan, goal: { objective: 'Ship it', phase: 'active' as const, armed: true, rounds: 3, maxRounds: 256 },
    subagents: [{ id: 'child-1', label: 'Review tests', state: 'working' as const, detail: 'Continuable', inspectable: true }],
    context: { used: 45_000, window: 128_000 }, usage: { input: 1_234_000, output: 30_500, cached: 1_000_000 },
  }
  const frame = render(<App {...props(busy)} />).lastFrame() ?? ''
  // No mode thins the goal: it keeps its key and its count beside the other rows.
  const header = frame.split('\n').find(line => line.includes('● Goal 3/256'))!
  expect(header).toMatch(/Ctrl\+O ● Goal 3\/256$/)
  expect(header).not.toContain('Ship it')
  expect(agentRow(frame)).toMatch(/^↳ Subagents 1 · 1 working +Ctrl\+G$/)
  expect(frame).toMatch(/^☐ Tasks .*Ctrl\+T$/m)
  // The status line is one layout in every mode: every reading that fits stays.
  const status = '  model  ctx ~35% (45k/128k)  in 1.2M  out 30.5k  cache hit 81%  /workspace'
  expect(statusRow(frame)).toBe(status)
  expect(statusRow(render(<App {...props({ ...busy, goal: undefined })} />).lastFrame())).toBe(status)
  // One of them alone reads the same.
  const alone = render(<App {...props({ ...busy, todos: undefined, subagents: [] })} />).lastFrame() ?? ''
  expect(alone).toMatch(/Ctrl\+O ● Goal 3\/256$/m)
  expect(alone).not.toContain('Ship it')
  expect(statusRow(alone)).toBe(status)
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
    expect(bare).not.toContain('think high')
    const own = render(<App {...props({ permission: 'danger-full-access', thinkingLevel: 'high',
      inspection: { ...inspection, permission: 'custom', thinkingLevel: 'low' } })} />).lastFrame() ?? ''
    expect(own).toContain('Parent: session-test > child · Access custom')
    expect(own).toContain('think low')
    expect(own).not.toContain('danger-full-access')
  })
})

it('shows only the inspected child’s context and usage, then restores the parent’s', () => {
  const parent = props({ context: { used: 9_000, window: 128_000 }, usage: { input: 9000, output: 400 } })
  const child = { sessionId: 'child', label: 'Review', committed: emptyTranscript,
    live: [], status: 'idle' as const, model: 'mock/child-model' }
  const ui = render(<App {...parent} inspection={child} />)
  expect(ui.lastFrame()).not.toContain('ctx ~')
  expect(ui.lastFrame()).not.toContain('in 9k')
  ui.rerender(<App {...parent} inspection={{ ...child, context: { used: 750, window: 8192 },
    usage: { input: 700, output: 50 } }} />)
  expect(ui.lastFrame()).toContain('ctx ~9% (750/8.2k)')
  expect(ui.lastFrame()).toContain('in 700  out 50')
  expect(ui.lastFrame()).not.toContain('ctx ~7% (9k/128k)')
  ui.rerender(<App {...parent} />)
  expect(ui.lastFrame()).toContain('ctx ~7% (9k/128k)')
})

describe('thinking level', () => {
  it.each(['en', 'zh'] as const)('labels a selected effort in %s and omits unknown levels', locale => {
    const copy = dictionaries[locale]
    const ui = render(<App {...props({ copy, permission: 'workspace-write' })} />)
    expect(statusRow(ui.lastFrame())).not.toContain(copy.think)
    ui.rerender(<App {...props({ copy, permission: 'workspace-write', thinkingLevel: 'high' })} />)
    expect(statusRow(ui.lastFrame())).toContain(`${copy.think} high`)
    ui.rerender(<App {...props({ copy, permission: 'workspace-write', thinkingLevel: copy.providerDefault })} />)
    expect(statusRow(ui.lastFrame())).toContain(`${copy.think} ${copy.providerDefault}`)
  })
})

describe('context occupancy', () => {
  it('reports used, capacity, and percent once the meter has measured a request', () => {
    const ui = render(<App {...props({ context: { used: 12_340, window: 1_000_000 } })} />)
    // The percentage leads; the absolute count follows it in brackets.
    expect(statusRow(ui.lastFrame())).toContain('ctx ~1% (12.3k/1M)')
  })

  it('shows nothing before the meter reports', () => {
    // Both meter fields are optional. A session reports nothing until a request
    // measures it, and a model with no exact capacity never reports a window.
    // A fraction of an unknown whole would be worse than silence.
    const ui = render(<App {...props()} />)
    expect(statusRow(ui.lastFrame())).toBe('  model  /workspace')
  })

  it('labels the figure in the active locale', () => {
    const ui = render(<App {...props({ copy: dictionaries.zh, context: { used: 500, window: 128_000 } })} />)
    expect(statusRow(ui.lastFrame())).toContain('上下文 ~0% (500/128k)')
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
  it('names the model where the state word was, without a label, and no token fields before a request reports', () => {
    const ui = render(<App {...props({ status: 'running' })} />)
    const row = statusRow(ui.lastFrame())
    expect(row).toBe('  model  /workspace')
    expect(row).not.toContain(dictionaries.en.working)
    expect(row).not.toContain(' in ')
  })

  it.each(['en', 'zh'] as const)('reports input, output, and the cache hit the provider reported in %s', locale => {
    const copy = dictionaries[locale]
    const ui = render(<App {...props({ copy, context: { used: 500, window: 128_000 }, usage: { input: 12_340, output: 1_200, cached: 10_000 } })} />)
    expect(statusRow(ui.lastFrame())).toBe(
      `  model  ${copy.context} ~0% (500/128k)  ${copy.tokensIn} 12.3k  ${copy.tokensOut} 1.2k  ${copy.cacheHit} 81%  /workspace`)
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
  await vi.waitFor(() => expect(ui.lastFrame()).toContain(' Subagents 1 · 1 working '))
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

it('names the open child and the way back on a bar under the input', () => {
  const entries = [
    { id: 'other', label: 'Earlier', state: 'saved', outcome: 'completed', detail: 'One-shot', inspectable: true },
    { id: 'child', label: 'Review', state: 'working', detail: 'Continuable', inspectable: true },
  ] as const
  const inspection = { sessionId: 'child', label: 'Review', committed: emptyTranscript,
    live: [], status: 'running' as const, model: 'mock/child-model' }
  const frame = render(<App {...props({ subagents: entries, inspection })} />).lastFrame() ?? ''
  const lines = frame.split('\n')
  const bar = lines.find(line => /^↳ Review /u.test(line)) ?? ''
  expect(bar).toMatch(/^↳ Review 2\/2 · Working · Read-only +Esc back to parent$/)
  // The bar takes the slot of the parent's row, and the row is not drawn beside it.
  expect(lines.at(-2)).toBe(bar)
  expect(agentRow(frame)).toBe('')
  for (const columns of [8, 12, 18, 20, 40, 59, 60, 80]) {
    const row = renderToString(<InspectionBar label="Review" entries={entries} id="child" working copy={dictionaries.en} columns={columns} />, { columns })
    expect(row.split('\n'), `${columns}`).toHaveLength(1)
    expect(stringWidth(row), `${columns}`).toBeLessThanOrEqual(columns)
    // The way back is never the part given up while any of it fits.
    if (columns >= 18) expect(row, `${columns}`).toContain('Esc back to parent')
  }
  const zh = renderToString(<InspectionBar label="Review" entries={entries} id="child" working copy={dictionaries.zh} columns={80} />, { columns: 80 })
  expect(zh).toMatch(/^↳ Review 2\/2 · 工作中 · 只读 +Esc 返回父会话$/u)
})

it('keeps the bar for a child the catalog does not list', () => {
  const inspection = { sessionId: 'gone', label: 'Gone', committed: emptyTranscript,
    live: [], status: 'idle' as const, model: 'mock/child-model' }
  const frame = render(<App {...props({ subagents: [], inspection })} />).lastFrame() ?? ''
  expect(frame.split('\n').at(-2)).toMatch(/^↳ Gone · Read-only +Esc back to parent$/)
})

it('selects the subagents row with Down, and moves the pointer past a child with no transcript', async () => {
  const onInspectSubagent = vi.fn()
  const onSubmit = vi.fn()
  const remote = { id: 'run-1', label: 'remote', state: 'working', detail: 'Remote run', inspectable: false } as const
  const saved = { id: 'old', label: 'Earlier', state: 'saved', outcome: 'completed', detail: 'One-shot', inspectable: true } as const
  const ui = render(<App {...props({ onInspectSubagent, onSubmit, subagents: [remote, saved] })} />)
  ui.stdin.write('\x1b[B')
  await vi.waitFor(() => expect(agentRow(ui.lastFrame())).toMatch(/^> Subagents 2 · 1 working · 1 done +Enter opens$/))
  ui.stdin.write('\r')
  // The pointer starts on the first child it can open.
  await vi.waitFor(() => expect(ui.lastFrame()).toContain('▸ ○ Earlier  Completed · Saved'))
  expect(ui.lastFrame()).not.toContain('Remote run')
  ui.stdin.write('\x1b[A')
  await vi.waitFor(() => expect(ui.lastFrame()).toContain('▸ ● remote'))
  expect(ui.lastFrame()).toContain('Remote run · run-1 · No local transcript available')
  // Enter on a child it cannot open keeps the sheet.
  ui.stdin.write('\r')
  await new Promise(resolve => setTimeout(resolve, 20))
  expect(onInspectSubagent).not.toHaveBeenCalled()
  ui.stdin.write('\x1b[B\r')
  await vi.waitFor(() => expect(onInspectSubagent).toHaveBeenCalledWith('old'))
  expect(onSubmit).not.toHaveBeenCalled()
})

it('summarises a crowded subagent sheet and gives only the selected child a detail line', async () => {
  const subagents = Array.from({ length: 10 }, (_, index) => ({
    id: `c${index}`, label: `Child ${index}`, detail: 'One-shot', inspectable: true,
    ...index < 2 ? { state: 'working' as const }
      : index === 2 ? { state: 'saved' as const, outcome: 'failed' as const }
      : { state: 'saved' as const, outcome: 'completed' as const },
  }))
  const ui = render(<App {...props({ subagents })} />)
  ui.stdin.write('\x07')
  await vi.waitFor(() => expect(ui.lastFrame()).toContain(' Subagents 10 · 2 working · 7 done '))
  const frame = ui.lastFrame() ?? ''
  expect(frame).toMatch(/━+─+ {2}7\/10 done · 2 working · 1 failed/)
  expect(frame).toContain('▸ ● Child 0  Working')
  expect(frame).toContain('One-shot · c0')
  expect(frame).not.toContain('One-shot · c1')
  expect(frame).not.toContain('One-shot · c5')
  ui.stdin.write('\x1b[B')
  await vi.waitFor(() => expect(ui.lastFrame()).toContain('One-shot · c1'))
  expect(ui.lastFrame()).not.toContain('One-shot · c0')
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
  expect(ui.lastFrame()).toMatch(/ Tasks 1\/3 {3}Subagents 1 · 1 working {3}Goal /)
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
  expect(agentRow(ui.lastFrame())).toMatch(/^↳ Subagents 1 +Ctrl\+G$/)
  expect(ui.lastFrame()).not.toContain(dictionaries.en.sheetClose)
})

it('keeps the subagents on one row however many there are, and at any width', () => {
  const subagents = Array.from({ length: 12 }, (_, index) => ({
    id: `c${index}`, label: `Child number ${index}`, state: index < 7 ? 'working' as const : 'live' as const,
    detail: 'Continuable', inspectable: true }))
  const frame = render(<App {...props({ subagents })} />).lastFrame() ?? ''
  expect(agentRow(frame)).toMatch(/^↳ Subagents 12 · 7 working +Ctrl\+G$/)
  expect(frame).not.toContain('Child number')
  for (const columns of [8, 20, 40, 59, 60, 80]) {
    const row = renderToString(<SubagentRow entries={subagents} copy={dictionaries.en} columns={columns} hint="Ctrl+G" />, { columns })
    expect(row.split('\n'), `${columns}`).toHaveLength(1)
    expect(stringWidth(row), `${columns}`).toBeLessThanOrEqual(columns)
    // Below 60 columns the key goes, as the composer's hint does.
    expect(row.includes('Ctrl+G'), `${columns}`).toBe(columns >= 60)
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
  const status = statusRow(frame)
  expect(status).toMatch(/update\s+v0\.2\.0 · \/update/)
  // Last of the bounded fields, so it is the first a narrow line gives up.
  expect(status.indexOf('out')).toBeLessThan(status.indexOf('update'))
  expect(status.indexOf('update')).toBeLessThan(status.indexOf('/workspace'))
})

it('asks for a restart once the newer release is installed', () => {
  const frame = render(<App {...props({ update: { version: '0.2.0', installed: true } })} />).lastFrame()!
  const status = statusRow(frame)
  expect(status).toContain(`v0.2.0 · ${dictionaries.en.updateRestart}`)
})

const clean: GitState = { branch: 'main', detached: false, ahead: 0, behind: 0, staged: 0, modified: 0, untracked: 0, conflicted: 0 }

describe('git field', () => {
  const status = (overrides: Partial<AppProps>) =>
    statusRow(render(<App {...props(overrides)} />).lastFrame())

  it('names the branch after the context reading and before the cost readings', () => {
    const row = status({ git: { ...clean, staged: 1, modified: 2, untracked: 3, ahead: 4, behind: 5 },
      context: { used: 3_000, window: 128_000 }, usage: { input: 1200, output: 300 } })
    expect(row).toContain('\u2387 main +1 ~2 ?3 \u21914 \u21935')
    expect(row.indexOf(dictionaries.en.context)).toBeLessThan(row.indexOf('\u2387 main'))
    expect(row.indexOf('\u2387 main')).toBeLessThan(row.indexOf(dictionaries.en.tokensIn))
    expect(row.indexOf('\u2387 main')).toBeLessThan(row.indexOf('/workspace'))
  })

  it('shows a clean tree as the branch alone, a detached HEAD by its commit, and nothing outside a repository', () => {
    expect(status({ git: clean })).toMatch(/\u2387 main {2}\/workspace$/)
    expect(status({ git: { ...clean, branch: '0123456', detached: true } })).toContain('\u2387 (0123456)')
    expect(status({})).not.toContain('\u2387')
  })

  it('draws ASCII where the terminal draws the classic frame', () => {
    const row = status({ frame: 'classic', git: { ...clean, modified: 1, ahead: 2, behind: 3 } })
    expect(row).toContain('main ~1 ^2 v3  /workspace')
    expect(row).not.toContain('\u2387')
  })
})
