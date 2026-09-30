import { afterEach, expect, it } from 'vitest'
import { Context } from '@deepseek-ai/cordis'
import Sessions, { SessionId } from '@deepseek-ai/dsh-session'
import Projections from '@deepseek-ai/dsh-session-projection'
import { WorkflowRunId } from '@deepseek-ai/dsh-workflow'
import { workflowsProjectionDefinition } from '../src/projection.ts'

const contexts: Context[] = []
afterEach(async () => { await Promise.all(contexts.splice(0).map(ctx => ctx.fiber.dispose())) })

async function setup() {
  const ctx = new Context()
  contexts.push(ctx)
  await ctx.plugin(Sessions)
  await ctx.plugin(Projections)
  ctx.sessionProjections.register(workflowsProjectionDefinition)
  const session = ctx.sessions.create()
  const view = () => ctx.sessionProjections.snapshot(session, ['workflows']).values.workflows
  return { ctx, session, view }
}

it('replays interleaved workflows with member phases and independent outcomes', async () => {
  const { session, view } = await setup()
  const first = WorkflowRunId('review'), second = WorkflowRunId('tests')
  session.append('tool-workflow/run-start', { runId: first, name: 'review' })
  session.append('tool-workflow/run-start', { runId: second, name: 'tests' })
  session.append('tool-workflow/agent-start', { runId: first, seq: 1, childId: SessionId('reviewer'), label: 'Review', phase: 'Inspect' })
  session.append('tool-workflow/agent-start', { runId: second, seq: 1, childId: SessionId('tester'), label: 'Test' })
  expect(view()).toMatchObject([{ status: 'running', members: [{ phase: 'Inspect' }] }, { status: 'running' }])
  session.append('tool-workflow/agent-end', { runId: first, seq: 1, outcome: 'completed' })
  session.append('tool-workflow/run-end', { runId: first, stopReason: 'completed' })
  session.append('tool-workflow/agent-end', { runId: second, seq: 1, outcome: 'failed' })
  session.append('tool-workflow/run-end', { runId: second, stopReason: 'error' })
  expect(view()).toEqual([
    { id: first, name: 'review', status: 'completed', members: [{ seq: 1, childId: 'reviewer', label: 'Review', phase: 'Inspect', outcome: 'completed' }] },
    { id: second, name: 'tests', status: 'error', members: [{ seq: 1, childId: 'tester', label: 'Test', outcome: 'failed' }] },
  ])
})

it('retains incomplete records without carrying live activity into the next turn or inherited fork history', async () => {
  const { session, view } = await setup()
  session.append('turn/start', { turn: 1 })
  session.append('tool-workflow/run-start', { runId: WorkflowRunId('unfinished'), name: 'unfinished' })
  session.append('turn/end', { turn: 1, reason: { kind: 'aborted' } })
  expect(view()).toMatchObject([{ status: 'unfinished' }])
  session.append('turn/start', { turn: 2 })
  expect(view()).toMatchObject([{ status: 'unfinished' }])
  session.append('tool-workflow/run-start', { runId: WorkflowRunId('new'), name: 'new' })
  session.append('tool-workflow/run-end', { runId: WorkflowRunId('new'), stopReason: 'cancelled' })
  expect(view()).toMatchObject([{ status: 'unfinished' }, { status: 'cancelled' }])
  session.append('session/end-seed', { inherited: true })
  expect(view()).toEqual([])
})
