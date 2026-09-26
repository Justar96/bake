/** Connected child identities and activity come from the application. */
import type { TuiCopy } from './copy.ts'

export interface SubagentEntry {
  readonly id: string
  readonly label: string
  readonly state: 'working' | 'live' | 'saved' | 'issue'
  /** Latest recorded child turn outcome; absent during a new run or without a terminal record. */
  readonly outcome?: 'completed' | 'failed' | 'stopped'
  readonly detail: string
  readonly inspectable: boolean
}

const STATE = {
  working: 'subagentWorking', live: 'subagentLive', saved: 'subagentSaved', issue: 'subagentIssue',
} as const satisfies Record<SubagentEntry['state'], keyof TuiCopy>

/** Keep the recorded outcome separate from whether the session is resident. */
export function subagentStatus(entry: SubagentEntry, copy: TuiCopy): string {
  const activity = copy[STATE[entry.state]]
  if (entry.state === 'working' || entry.outcome === undefined) return activity
  return `${copy[entry.outcome === 'completed' ? 'subagentCompleted' : entry.outcome === 'failed' ? 'subagentFailed' : 'subagentStopped']} · ${activity}`
}
