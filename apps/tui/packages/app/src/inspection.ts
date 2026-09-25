/** Read-only observation of a child transcript. The subagent keeps its own handle. */
import type { Context } from '@deepseek-ai/cordis'
import type { SessionEvent, SessionId } from '@deepseek-ai/dsh-session'
import type { ProjectionSnapshot } from '@deepseek-ai/dsh-session-projection'
import type {} from '@deepseek-ai/dsh-token-meter'
import type {} from '@deepseek-ai/dsh-permission-presets/types'
import { Actions, announcedCalls, appendTranscript, emptyTranscript, project, projector, SETTLES } from '@dsh-tui/ui'
import type { TuiCopy } from '@dsh-tui/ui/copy.ts'
import { LiveBlocks } from './live.ts'
import { contextFor, usageFor } from './status.ts'

/** Subscribe before replay, so a child can keep running while its history opens. */
export class SubagentInspection {
  private committed = emptyTranscript
  private readonly actions = new Actions()
  private buffered: SessionEvent[] | undefined = []
  private cursor = -1
  private model = ''
  private thinkingLevel: string | undefined
  private savedSurface: ProjectionSnapshot | undefined
  private stream: { attempt: string; revision: number; blocks: LiveBlocks } | undefined
  private readonly off: (() => void)[]
  private readonly projection
  private closed = false

  constructor(private readonly ctx: Context, readonly id: SessionId, readonly label: string,
    copy: TuiCopy, changed: () => void) {
    this.projection = projector(copy, name => ctx.get('agents')?.get(id)?.ctx.get('tools')?.get(name))
    this.off = [
      ctx.on('session/event', (session, event) => {
        if (session.id !== id || this.closed) return
        if (this.buffered !== undefined) this.buffered.push(event)
        else this.append(event)
        if (event.type === 'assistant/message' || event.type === 'assistant/attempt' || event.type === 'turn/end') this.stream = undefined
        changed()
      }),
      ctx.on('agent/status', ({ agent }) => { if (agent.id === id && !this.closed) changed() }),
      ctx.on('agent/assistant-stream', ({ agent, frame }) => {
        if (agent.id !== id || this.closed) return
        // A visit can begin halfway through an attempt. Show the arriving tail
        // until the durable message supplies the complete response.
        if (frame.type === 'chunk' && this.stream === undefined) {
          this.stream = { attempt: frame.attemptId, revision: frame.revision - 1, blocks: new LiveBlocks() }
        }
        if (frame.type === 'start') this.stream = { attempt: frame.attemptId, revision: frame.revision, blocks: new LiveBlocks() }
        else if (this.stream?.attempt === frame.attemptId && frame.revision > this.stream.revision) {
          this.stream.revision = frame.revision
          if (frame.type === 'chunk') this.stream.blocks.push(frame.chunk)
          else this.stream = undefined
        }
        changed()
      }),
    ]
    const projections = ctx.get('sessionProjections')
    const offProjection = projections?.onChanged((session, key) => {
      if (session.id !== id || !['permissions', 'contextPressure', 'tokenUsage'].includes(key) || this.closed) return
      // Keep the last authoritative values if the live child is released.
      this.savedSurface = projections.snapshot(session, ['permissions', 'contextPressure', 'tokenUsage'])
      changed()
    })
    if (offProjection !== undefined) this.off.push(offProjection)
  }

  /** Read a consistent history cut, then join the events received during that read. */
  async replay(signal: AbortSignal): Promise<void> {
    const query = this.ctx.get('sessionQuery')
    if (query === undefined) throw new Error('tui: sessionQuery is required')
    using observation = await query.observeSession(this.id, { signal })
    signal.throwIfAborted()
    if (this.closed) return
    const session = this.ctx.sessions.get(this.id)
    const surface = session === undefined ? observation.projections
      : this.ctx.get('sessionProjections')?.snapshot(session, ['permissions', 'contextPressure', 'tokenUsage'])
    if (surface !== undefined && (this.savedSurface === undefined || surface.asOfSeq >= this.savedSurface.asOfSeq)) {
      // Prepared observations include unrelated projections. Keep only status readings.
      const { permissions, contextPressure, tokenUsage } = surface.values
      this.savedSurface = { asOfSeq: surface.asOfSeq, values: {
        ...permissions === undefined ? {} : { permissions },
        ...contextPressure === undefined ? {} : { contextPressure },
        ...tokenUsage === undefined ? {} : { tokenUsage },
      } }
    }
    for (const event of observation.events) this.append(event)
    for (const event of this.buffered ?? []) this.append(event)
    this.buffered = undefined
  }

  get view() {
    const agent = this.ctx.get('agents')?.get(this.id)
    const session = this.ctx.sessions.get(this.id)
    const surface = (session === undefined ? this.savedSurface
      : this.ctx.get('sessionProjections')?.snapshot(session, ['permissions', 'contextPressure', 'tokenUsage']))?.values
    const permission = surface?.permissions?.currentValue
    const lastRequest = session?.requestHeader()
    const thinkingLevel = lastRequest === undefined ? agent?.options.reasoningEffort ?? this.thinkingLevel
      : lastRequest.config.reasoningEffort
    const model = lastRequest === undefined
      ? agent === undefined ? this.model : `${agent.options.provider}/${agent.options.model}`
      : `${lastRequest.config.provider}/${lastRequest.config.model}`
    const context = contextFor(surface?.contextPressure, model)
    const usage = usageFor(surface?.tokenUsage)
    return {
      sessionId: this.id, label: this.label, committed: this.committed,
      live: this.actions.live(this.stream?.blocks.rows()), status: agent?.status ?? 'idle' as const,
      model, context,
      ...usage === undefined ? {} : { usage },
      ...permission === undefined ? {} : { permission },
      ...thinkingLevel === undefined ? {} : { thinkingLevel },
    }
  }

  /** Detach observers without changing the child or its owner. */
  close(): void {
    if (this.closed) return
    this.closed = true
    for (const off of this.off) off()
  }

  private append(event: SessionEvent): void {
    if (event.seq <= this.cursor) return
    this.cursor = event.seq
    if (event.type === 'request/header') {
      const config = event.data.header.config
      this.model = `${config.provider}/${config.model}`
      this.thinkingLevel = config.reasoningEffort
    }
    const rows = project(event, this.projection)
    this.committed = appendTranscript(this.committed, this.actions.fold(rows, SETTLES.has(event.type)))
    if (event.type === 'assistant/message') this.actions.announce(announcedCalls(event))
  }
}
