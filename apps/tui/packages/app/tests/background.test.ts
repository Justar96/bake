/** The background row's entries, read from the job registry's snapshots. */
import { describe, expect, test } from 'bun:test'
import type { JobSnapshot } from 'bake-jobs'
import { backgroundEntries } from '../src/background.ts'

const job = (id: string, kind: string, status: JobSnapshot['status'], startedAt: number): JobSnapshot =>
  ({ id, kind, label: `${kind} work`, status, startedAt, reported: false }) as JobSnapshot

describe('backgroundEntries', () => {
  test('lists jobs oldest first, counting a stopping job as running, and leaves delegations to the subagents row', () => {
    expect(backgroundEntries([
      job('bash-2', 'bash', 'completed', 20), job('subagent-1', 'subagent', 'running', 5),
      job('bash-1', 'bash', 'stopping', 10), job('pwsh-1', 'pwsh', 'running', 30),
    ])).toEqual([
      { id: 'bash-1', tool: 'bash', label: 'bash work', running: true },
      { id: 'bash-2', tool: 'bash', label: 'bash work', running: false },
      { id: 'pwsh-1', tool: 'pwsh', label: 'pwsh work', running: true },
    ])
  })
})
