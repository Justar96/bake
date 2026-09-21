/** Session observers and human actions shared by the renderer and integration tests. */
import type { Context } from '@deepseek-ai/cordis'
import type { Agent, AssistantStreamFrame } from '@deepseek-ai/dsh-agent'
import { BlockAssembler, createUserMessage } from '@deepseek-ai/dsh-llm'
import { parseCommand } from '@deepseek-ai/dsh-commands'
import type { SessionEvent } from '@deepseek-ai/dsh-session'
import type {} from '@deepseek-ai/dsh-session-query'
import type {} from '@deepseek-ai/dsh-compaction'
import type {} from '@deepseek-ai/dsh-session-projection'
import { project, type Row } from '@dsh-tui/ui'
import type { TuiCopy } from '@dsh-tui/ui/copy.ts'
import { Interactions } from './interactions.ts'
import { listTargets, login } from './login.ts'
// Empty type import: the token meter declaration-merges `contextPressure` into
// the projection map, and that key is invisible to this module without it.
import type {} from '@deepseek-ai/dsh-token-meter'

/** One terminal's presentation over a live Agent and its durable projections. */
export class SessionController {
  readonly interactions: Interactions
  private committed: readonly Row[] = []
  private buffered: SessionEvent[] | undefined = []
  private cursor = -1
  private live: readonly Row[] = []
  private stream: { revision: number; attemptId: string; assembler: BlockAssembler } | undefined
  private stopping = false
  private command: { text: string; abort: AbortController; done: Promise<void> } | undefined
  private notice: string | undefined
  private closed = false
  private readonly off: (() => void)[] = []

  /**
   * Subscribe before publication so startup input and requests remain observable.
   * @param ctx - terminal-owned context.
   * @param agent - the exact agent being connected.
   * @param copy - localized labels.
   * @param credentialRefs - profile-owned credential names used by /login.
   * @param changed - renderer notification; ignored after closure.
   */
  constructor(private readonly ctx: Context, readonly agent: Agent, private readonly copy: TuiCopy,
    private readonly credentialRefs: readonly string[], private readonly changed: () => void) {
    this.interactions = new Interactions(ctx, agent, () => this.repaint())
    const commands = agent.ctx.get('commands')
    if (commands === undefined) throw new Error('tui: commands service is required')
    this.off.push(agent.ctx.effect(() => commands.register({
      name: 'login', description: copy.signIn, recordInput: false,
      handler: async ({ rawInput, signal }) => {
        await this.runLogin(rawInput.trim(), signal)
        return { kind: 'success' }
      },
    })))
    this.off.push(agent.ctx.effect(() => commands.register({
      name: 'help', description: copy.listCommands, recordInput: false,
      handler: () => {
        // The registry is the list: a command contributed by any plugin appears
        // here without this surface knowing it exists.
        this.notify(commands.list(this.agent)
          .map(command => `/${command.name} — ${command.description}`)
          .join('\n'))
        return { kind: 'success' }
      },
    })))
    this.off.push(ctx.on('session/event', (session, event) => {
      if (session !== agent.session) return
      if (this.buffered !== undefined) this.buffered.push(event)
      else this.append(event)
      if (event.type === 'assistant/message' || event.type === 'assistant/attempt') this.live = []
      this.repaint()
    }))
    this.off.push(ctx.on('agent/status', payload => {
      if (payload.agent !== agent) return
      if (payload.status === 'idle') this.stopping = false
      this.repaint()
    }))
    this.off.push(ctx.on('agent/assistant-stream', payload => {
      if (payload.agent === agent) this.streamFrame(payload.frame)
    }))
    const projections = ctx.get('sessionProjections')
    if (projections === undefined) throw new Error('tui: sessionProjections is required')
    this.off.push(projections.onChanged((session, key) => {
      if (session !== agent.session) return
      if (key === 'inbox' || key === 'contextPressure') this.repaint()
    }))
  }

  /**
   * Join a consistent history cut to buffered live events without duplicates.
   * @param signal - cancellation while obtaining the observation.
   */
  async replay(signal: AbortSignal): Promise<void> {
    const query = this.ctx.get('sessionQuery')
    if (query === undefined) throw new Error('tui: sessionQuery is required for transcript replay')
    using observation = await query.observeSession(this.agent.id, { signal, projectionMode: 'none' })
    signal.throwIfAborted()
    for (const event of observation.events) this.append(event)
    for (const event of this.buffered ?? []) this.append(event)
    this.buffered = undefined
    this.repaint()
  }

  /** Current renderer fields; inbox and activity are read from their harness owners. */
  get view() {
    const projections = this.ctx.get('sessionProjections')
    const inbox = projections?.stateOf(this.agent.session, 'inbox')
    if (inbox === undefined) throw new Error('tui: inbox projection is required')
    const pending = (['next-step', 'next-turn'] as const).flatMap(target => inbox[target]
      .filter(message => message.source.kind === 'user')
      .map(message => ({ id: message.id, target, text: message.content.flatMap(block => block.type === 'text' ? [block.text] : []).join('') })))
    // The meter owns these numbers. Both fields are optional there — a session
    // reports nothing until a request measures it, and a model with no exact
    // capacity never reports a window — so occupancy stays absent rather than
    // rendering a fraction of an unknown whole.
    const pressure = projections?.stateOf(this.agent.session, 'contextPressure')
    const used = pressure?.pressureTokens
    const window = pressure?.contextWindow
    return {
      committed: this.committed, live: this.live, pending, status: this.agent.status,
      stopping: this.stopping, command: this.command?.text, notice: this.notice,
      interaction: this.interactions.current,
      context: used === undefined || window === undefined ? undefined : { used, window },
    }
  }

  /**
   * Display application feedback outside the durable conversation.
   * @param text - message, or undefined to clear it.
   */
  notify(text: string | undefined): void { this.notice = text; this.repaint() }

  /**
   * Dispatch a registered command or identified user message.
   * @param text - submitted composer text.
   */
  submit(text: string): void {
    if (this.closed) return
    this.notice = undefined
    const parsed = parseCommand(text)
    if (parsed === undefined) {
      const message = createUserMessage({ content: [{ type: 'text', text }], source: { kind: 'user' } })
      if (this.agent.status === 'running') this.agent.steer(message)
      else this.agent.followup(message)
      this.repaint()
      return
    }
    if (this.command !== undefined) { this.notify(this.copy.commandBusy); return }
    const abort = new AbortController()
    const done = Promise.resolve().then(async () => {
      const commands = this.ctx.get('commands')
      if (commands === undefined) throw new Error('tui: commands service is required')
      const result = await commands.execute(this.agent, text, [], abort.signal)
      if (result === undefined) this.notify(`${this.copy.unknownCommand}: /${parsed.name}`)
    }).catch((error: unknown) => {
      this.notify(abort.signal.aborted ? this.copy.cancelled : error instanceof Error ? error.message : String(error))
    }).finally(() => { this.command = undefined; this.repaint() })
    this.command = { text, abort, done }
    this.repaint()
  }

  /** Cancel the nearest interaction or command; otherwise interrupt while retaining visible pending work. */
  cancel(): void {
    if (this.interactions.current !== undefined) { this.interactions.cancel(); return }
    if (this.command !== undefined) this.command.abort.abort()
    else {
      this.stopping = this.agent.status === 'running'
      this.agent.cancel({ kind: 'user' }, { keepInbox: true })
    }
    this.repaint()
  }

  /**
   * Report missing configured provider keys without reading their values.
   * @returns after credential metadata has been inspected.
   */
  async reportCredentials(): Promise<void> {
    const targets = await listTargets(this.ctx, this.credentialRefs)
    if (targets.some(target => target.kind === 'key' && !target.configured)) this.notify(this.copy.noCredentials)
  }

  /** Stop observers and settle human requests synchronously before terminal release. */
  close(): void {
    if (this.closed) return
    this.closed = true
    for (const off of this.off) off()
    this.interactions.dispose()
    this.command?.abort.abort()
  }

  /** @returns after an outstanding command has settled during shutdown. */
  async drain(): Promise<void> { await this.command?.done }

  private repaint(): void { if (!this.closed) this.changed() }

  private append(event: SessionEvent): void {
    if (event.seq <= this.cursor) return
    this.cursor = event.seq
    let rows: readonly Row[]
    if (event.type === 'command/run') rows = [{ kind: 'user', text: `/${event.data.name}${event.data.args ?? ''}` }]
    else if (event.type === 'command/done') rows = event.data.text === undefined ? [] : [{ kind: 'notice', tone: event.data.kind === 'error' ? 'error' : 'info', text: event.data.text }]
    else if (event.type === 'turn/end' && (event.data.reason.kind === 'aborted' || event.data.reason.kind === 'interrupted')) rows = [{ kind: 'notice', tone: 'warn', text: this.copy.cancelled }]
    else if (event.type === 'compaction/summary') rows = [{ kind: 'notice', tone: 'info', text: this.copy.compacted }]
    else rows = project(event)
    if (rows.length > 0) this.committed = [...this.committed, ...rows]
  }

  private streamFrame(frame: AssistantStreamFrame): void {
    if (frame.type === 'start') this.stream = { revision: frame.revision, attemptId: frame.attemptId, assembler: new BlockAssembler() }
    if (this.stream === undefined || this.stream.attemptId !== frame.attemptId) return
    if (frame.type !== 'start' && frame.revision <= this.stream.revision) return
    this.stream.revision = frame.revision
    if (frame.type === 'chunk') {
      this.stream.assembler.push(frame.chunk)
      this.live = this.stream.assembler.interruptedBlocks().flatMap((block): Row[] => {
        if (block.type === 'text') return [{ kind: 'assistant', text: block.text }]
        if (block.type === 'reasoning') return [{ kind: 'reasoning', text: block.text }]
        return []
      })
    } else if (frame.type === 'end') { this.stream = undefined; this.live = [] }
    this.repaint()
  }

  private async runLogin(id: string, signal: AbortSignal): Promise<void> {
    const targets = await listTargets(this.ctx, this.credentialRefs)
    signal.throwIfAborted()
    if (targets.length === 0) { this.notify(this.copy.noTargets); return }
    if (id === '') {
      this.notify(targets.map(target => `/login ${target.id} — ${target.label}: ${target.configured ? this.copy.configured : this.copy.notSet}${target.writable ? '' : ` (${this.copy.readOnly})`}`).join('\n'))
      return
    }
    const result = await login(this.ctx, targets, id, {
      notify: notice => this.notify([notice.message, notice.url, notice.code].filter(value => value !== undefined).join(' ')),
      prompt: prompt => this.interactions.prompt(prompt, signal),
    }, signal, this.copy.pasteCredential)
    this.notify(result.kind === 'stored' ? `${result.target}: ${this.copy.stored}`
      : result.kind === 'cancelled' ? this.copy.loginCancelled : `${this.copy.unknownTarget}: ${result.id}`)
  }
}
