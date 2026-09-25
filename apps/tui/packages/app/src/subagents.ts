/** Session-scoped child listing backed by the Harness subagent service. */
import type { Context } from '@deepseek-ai/cordis'
import type { SessionEvent } from '@deepseek-ai/dsh-session'
import type { Agent } from '@deepseek-ai/dsh-agent'
import type { SubagentRunInfo } from '@deepseek-ai/dsh-subagent'
import type { SubagentEntry } from '@dsh-tui/ui/subagents.tsx'
import type { TuiCopy } from '@dsh-tui/ui/copy.ts'

type SubagentListEntry = Awaited<ReturnType<NonNullable<Context['subagents']>['listChildren']>>[number]

export interface SubagentView {
  readonly entries: readonly SubagentListEntry[]
  readonly activeRuns: readonly SubagentRunInfo[]
  readonly error: string | undefined
  readonly outcomes: ReadonlyMap<string, SubagentEntry['outcome']>
}

/** Preserve catalog order and derive activity from live owners and run notifications. */
export function subagentEntries(view: SubagentView, ctx: Context, copy: TuiCopy): readonly SubagentEntry[] {
  const working = new Set(view.activeRuns.map(run => run.id))
  const listed = new Set(view.entries.map(entry => entry.id))
  return [
    ...view.entries.map((entry): SubagentEntry => {
      if (entry.kind === 'diagnostic') return { id: entry.id, label: entry.id, state: 'issue', detail: entry.reason, inspectable: false }
      return {
        id: entry.id, label: entry.label ?? entry.id,
        state: working.has(entry.id) || ctx.get('agents')?.get(entry.id)?.status === 'running' ? 'working'
          : entry.activity === 'running' ? 'live' : 'saved',
        detail: entry.mode === 'continuable' ? copy.subagentContinuable : copy.subagentOneShot,
        inspectable: true,
        ...view.outcomes.get(entry.id) === undefined ? {} : { outcome: view.outcomes.get(entry.id)! },
      }
    }),
    ...view.activeRuns.filter(run => !listed.has(run.id)).map((run): SubagentEntry => ({
      id: run.id, label: run.provider, state: 'working', detail: copy.subagentRemote, inspectable: false,
    })),
  ]
}

/** Keep the visible child list current without owning child lifecycle state. */
export class SubagentCatalog {
  private state: SubagentView = { entries: [], activeRuns: [], error: undefined, outcomes: new Map() }
  private readonly runs = new Map<string, SubagentRunInfo>()
  private abort: AbortController | undefined
  private revision = 0
  private closed = false
  private readonly pending = new Set<Promise<void>>()
  private readonly off: (() => void)[]

  constructor(private readonly ctx: Context, private readonly agent: Agent, private readonly changed: () => void) {
    const refresh = (): void => this.refresh()
    this.off = [
      ctx.on('agent/status', ({ agent: child }) => {
        if (child.session.header.parentSession === agent.id) this.changed()
      }),
      agent.ctx.on('subagent/start', info => {
        this.runs.set(info.runId, info)
        this.publishRuns()
        refresh()
      }),
      agent.ctx.on('subagent/end', info => {
        this.runs.delete(info.runId)
        this.publishRuns()
        refresh()
      }),
      ctx.on('session/event', (session, event) => {
        if (session.header.parentSession === agent.id
          && (event.type === 'subagent/descriptor' || event.type === 'turn/start' || event.type === 'turn/end')) refresh()
      }),
      ctx.on('session/disposed', session => {
        if (session.header.parentSession === agent.id) refresh()
      }),
    ]
    this.refresh()
  }

  get view(): SubagentView { return this.state }

  /** Read the authoritative list for an explicit command. */
  async list(signal: AbortSignal): Promise<readonly SubagentListEntry[] | undefined> {
    const service = this.ctx.get('subagents')
    if (service === undefined) return undefined
    const revision = ++this.revision
    const entries = await service.listChildren(this.agent.id, signal)
    signal.throwIfAborted()
    const outcomes = new Map<string, SubagentEntry['outcome']>()
    const query = this.ctx.get('sessionQuery')
    if (query !== undefined) {
      // Observations run one at a time. A parent with many children must not open every cold read at once.
      for (const entry of entries) {
        if (entry.kind !== 'child') continue
        try {
          using observation = await query.observeSession(entry.id, { signal, projectionMode: 'none' })
          outcomes.set(entry.id, childOutcome(observation.events, observation.inheritedEventCount))
        } catch {
          signal.throwIfAborted()
          // An unavailable outcome must not be shown as a successful completion.
        }
      }
    }
    signal.throwIfAborted()
    if (!this.closed && revision === this.revision) {
      this.state = { entries, activeRuns: [...this.runs.values()], error: undefined, outcomes }
      this.changed()
    }
    return entries
  }

  /** Refresh after a child appears, settles, or leaves the live session store. */
  refresh(): void {
    if (this.closed) return
    this.abort?.abort()
    const abort = this.abort = new AbortController()
    const revision = this.revision + 1
    const done = this.list(abort.signal).then(() => {}, error => {
      if (abort.signal.aborted || this.closed || revision !== this.revision) return
      this.state = { entries: [], activeRuns: [...this.runs.values()], outcomes: new Map(), error: error instanceof Error ? error.message : String(error) }
      this.changed()
    }).finally(() => { this.pending.delete(done) })
    this.pending.add(done)
  }

  close(): void {
    if (this.closed) return
    this.closed = true
    for (const off of this.off) off()
    this.abort?.abort()
  }

  private publishRuns(): void {
    if (this.closed) return
    this.state = { ...this.state, activeRuns: [...this.runs.values()] }
    this.changed()
  }

  async drain(): Promise<void> { await Promise.all(this.pending) }
}


/** Read the child's own latest turn boundary. Inherited parent turns do not describe its result. */
export function childOutcome(events: readonly SessionEvent[], inherited: number): SubagentEntry['outcome'] {
  for (let index = events.length - 1; index >= inherited; index--) {
    const event = events[index]!
    if (event.type === 'turn/start') return undefined
    if (event.type !== 'turn/end') continue
    switch (event.data.reason.kind) {
      case 'completed': return 'completed'
      case 'error': return 'failed'
      case 'aborted': case 'interrupted': case 'blocked': case 'max-tokens': return 'stopped'
      default: return undefined
    }
  }
  return undefined
}
