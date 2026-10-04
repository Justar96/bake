/** The agent's background jobs, read from the host job registry for the row under the input. */
import type { Context } from '@deepseek-ai/cordis'
import type { Agent } from '@deepseek-ai/dsh-agent'
import type { JobSnapshot } from '@deepseek-ai/dsh-jobs'
import type { BackgroundEntry } from '@dsh-tui/ui/background.tsx'

/**
 * Producer kinds the background row leaves out: a delegated child has the
 * subagents row, which already says whether it works.
 */
const LISTED_ELSEWHERE: ReadonlySet<string> = new Set(['subagent'])

/**
 * Background work for the row under the input, oldest first.
 * @param jobs - the agent's job snapshots, as the registry lists them.
 * @returns one entry per job the row may count.
 */
export function backgroundEntries(jobs: readonly JobSnapshot[]): readonly BackgroundEntry[] {
  return jobs.filter(job => !LISTED_ELSEWHERE.has(job.kind)).sort((a, b) => a.startedAt - b.startedAt)
    .map(job => ({ id: job.id, tool: job.kind, label: job.label, running: job.status === 'running' || job.status === 'stopping' }))
}

/**
 * Read the agent's jobs without owning them. The registry is the authority,
 * so this keeps no copy: the caller reads it on every repaint.
 * @param ctx - the host context the registry is mounted on.
 * @param agent - the session's agent, whose jobs are listed.
 * @returns its jobs, or none when the profile mounts no registry or it is being torn down.
 */
export function listBackground(ctx: Context, agent: Agent): readonly BackgroundEntry[] {
  const jobs = ctx.get('jobs')
  if (jobs === undefined) return []
  try {
    return backgroundEntries(jobs.list(agent))
  } catch {
    // A registry disposed under a closing session has nothing left to show.
    return []
  }
}
