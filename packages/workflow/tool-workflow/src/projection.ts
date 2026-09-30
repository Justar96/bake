/** Replayable workflow progress from the tool's durable run and member records. */
import { z } from 'zod'
import type { ProjectionDefinition } from '@deepseek-ai/dsh-session-projection'
import type {} from './types.ts'

const memberSchema = z.object({
  seq: z.number().int().positive(),
  childId: z.string(),
  label: z.string(),
  phase: z.string().optional(),
  outcome: z.enum(['completed', 'failed', 'cancelled']).optional(),
}).strict()

const runSchema = z.object({
  id: z.string(),
  name: z.string(),
  status: z.enum(['running', 'unfinished', 'completed', 'cancelled', 'error']),
  members: z.array(memberSchema),
}).strict()

const workflowsSchema = z.array(runSchema)

/** A run's recorded members and outcome; an open record alone does not prove live execution. */
export type WorkflowProgress = z.infer<typeof runSchema>

declare module '@deepseek-ai/dsh-session-projection/types' {
  interface SessionProjectionStateMap {
    workflows: WorkflowProgress[]
  }
  interface SessionProjectionMap {
    workflows: WorkflowProgress[]
  }
}

export const workflowsProjectionDefinition = {
  key: 'workflows',
  stateVersion: 1,
  stateSchema: workflowsSchema,
  init: () => [],
  apply: (state, event) => {
    // Forked history describes the parent's runs, not work the child owns.
    if (event.type === 'session/end-seed' && event.data.inherited === true) return []
    if (event.type === 'turn/start' || event.type === 'turn/end') {
      return state.some(run => run.status === 'running')
        ? state.map(run => run.status === 'running' ? { ...run, status: 'unfinished' } : run)
        : state
    }
    if (event.type === 'tool-workflow/run-start') {
      return [...state, { id: event.data.runId, name: event.data.name, status: 'running', members: [] }]
    }
    if (event.type === 'tool-workflow/agent-start') {
      const { runId, ...member } = event.data
      return state.map(run => run.id === runId ? { ...run, members: [...run.members, member] } : run)
    }
    if (event.type === 'tool-workflow/agent-end') {
      const { runId, seq, outcome } = event.data
      return state.map(run => run.id === runId
        ? { ...run, members: run.members.map(member => member.seq === seq ? { ...member, outcome } : member) }
        : run)
    }
    if (event.type === 'tool-workflow/run-end') {
      const { runId, stopReason } = event.data
      return state.map(run => run.id === runId ? { ...run, status: stopReason } : run)
    }
    return state
  },
  wire: { viewSchema: workflowsSchema, view: state => state },
} satisfies ProjectionDefinition<'workflows', WorkflowProgress[]>
