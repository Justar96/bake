/** Terminal session navigation over Harness query, setup, and handle ownership. */
import type { Context } from '@deepseek-ai/cordis'
import type { Agent, AgentHandle } from '@deepseek-ai/dsh-agent'
import type { TuiCopy } from '@dsh-tui/ui/copy.ts'
import type { AttachmentOptions } from './attachments.ts'
import { SessionController } from './controller.ts'
import { openSession, type SessionOptions } from './session.ts'

interface ConnectedSession {
  readonly handle: AgentHandle
  readonly controller: SessionController
}

/** Owns the displayed session and one cancellable navigation operation. */
export class SessionNavigation {
  private current: ConnectedSession | undefined
  private candidate: SessionController | undefined
  private operation: { abort: AbortController; done: Promise<void>; committed: boolean } | undefined
  private closed = false

  /**
   * @param ctx - settled Harness services and owning plugin context.
   * @param options - startup identity and fresh-session preset.
   * @param copy - locale-owned application labels.
   * @param credentialRefs - configured login targets.
   * @param changed - renderer notification.
   */
  constructor(private readonly ctx: Context, private readonly options: SessionOptions & AttachmentOptions,
    private readonly copy: TuiCopy, private readonly credentialRefs: readonly string[],
    private readonly changed: () => void) {}

  /** The displayed session; unavailable until start resolves. */
  get controller(): SessionController | undefined { return this.current?.controller }
  /** Navigation owns input until preparation or retirement has settled. */
  get busy(): boolean { return this.operation !== undefined }

  /**
   * Open the startup session and replay its history.
   * @param signal - application startup lifetime.
   */
  async start(signal: AbortSignal): Promise<void> {
    this.current = await this.connect(this.options, signal)
  }

  /**
   * Send input to the displayed controller, refusing submissions during navigation.
   * @param text - composer submission.
   * @returns acceptance; rejected or pending admission must retain the composer draft.
   */
  submit(text: string): boolean | Promise<boolean> {
    if (this.closed) return false
    if (this.busy) { this.controller?.notify(this.copy.sessionsBusy); return false }
    return this.controller?.submit(text) ?? false
  }

  /** Cancel preparation before handoff, or delegate to the displayed session. */
  cancel(): void {
    if (this.operation !== undefined) {
      if (!this.operation.committed) this.operation.abort.abort()
    } else this.controller?.cancel()
  }

  /** Close observers before terminal release; drain owns asynchronous disposal. */
  close(): void {
    if (this.closed) return
    this.closed = true
    this.operation?.abort.abort()
    this.controller?.close()
    this.candidate?.close()
  }

  /** @returns after navigation, commands, discovery, and owned handles reach quiescence. */
  async drain(): Promise<void> {
    await this.operation?.done
    if (this.current !== undefined) await this.dispose(this.current)
  }

  private async connect(options: SessionOptions, signal: AbortSignal): Promise<ConnectedSession> {
    let handle: AgentHandle | undefined
    let controller: SessionController | undefined
    try {
      handle = await openSession(this.ctx, options, signal, (agent, selection) => {
        signal.throwIfAborted()
        if (this.closed) throw new Error(this.copy.sessionsCancelled)
        const commands = agent.ctx.get('commands')
        if (commands === undefined) throw new Error('tui: commands service is required')
        agent.ctx.effect(() => commands.register({
          name: 'sessions', description: this.copy.chooseSession, recordInput: false,
          handler: ({ rawInput }) => {
            if (rawInput.trim() !== '') return { kind: 'error', text: this.copy.sessionsUsage }
            this.request(agent)
            return { kind: 'success' }
          },
        }))
        controller = new SessionController(this.ctx, agent, this.copy, this.credentialRefs,
          () => { if (this.controller === controller && !this.closed) this.changed() }, this.options, selection)
        this.candidate = controller
      })
      if (controller === undefined) throw new Error('tui: agent setup did not connect the session')
      await controller.replay(signal)
      signal.throwIfAborted()
      return { handle, controller }
    } catch (error) {
      controller?.close()
      try { await controller?.drain() } finally { await handle?.dispose() }
      throw error
    } finally { this.candidate = undefined }
  }

  private request(agent: Agent): void {
    if (this.closed || this.busy || agent !== this.controller?.agent) return
    const abort = new AbortController()
    // The registry must finish command/done before its Session can be retired.
    const done = Promise.resolve().then(async () => {
      await this.controller?.drain()
      abort.signal.throwIfAborted()
      await this.navigate(abort.signal)
    }).catch((error: unknown) => {
      this.controller?.notify(abort.signal.aborted ? this.copy.sessionsCancelled
        : `${this.copy.sessionsError}: ${error instanceof Error ? error.message : String(error)}`)
    }).finally(() => { this.operation = undefined; if (!this.closed) this.changed() })
    this.operation = { abort, done, committed: false }
    this.changed()
  }

  private assertAvailable(agent: Agent): void {
    if (this.controller?.attachments.pending) throw new Error(this.copy.attachmentsBeforeNavigation)
    if (agent.status !== 'idle') throw new Error(this.copy.sessionsIdle)
    if (agent.inbox.nextStep.length > 0 || agent.inbox.nextTurn.length > 0) throw new Error(this.copy.sessionsPending)
  }

  private async navigate(commandSignal: AbortSignal): Promise<void> {
    const previous = this.current!
    const { agent } = previous.handle
    this.assertAvailable(agent)
    const changed = new AbortController()
    const signal = AbortSignal.any([commandSignal, changed.signal])
    const recheck = (): void => {
      try { this.assertAvailable(agent) } catch (error) { changed.abort(error) }
    }
    const offStatus = this.ctx.on('agent/status', payload => { if (payload.agent === agent) recheck() })
    const projections = this.ctx.get('sessionProjections')
    const agents = this.ctx.get('agents')
    if (projections === undefined || agents === undefined) throw new Error('tui: sessionProjections and agents are required')
    const offInbox = projections.onChanged((session, key) => {
      if (session === agent.session && key === 'inbox') recheck()
    })
    let next: ConnectedSession | undefined
    try {
      const query = this.ctx.get('sessionQuery')
      if (query === undefined) throw new Error('tui: sessionQuery is required')
      previous.controller.notify(this.copy.sessionsLoading)
      const records = (await query.filterSessions([{ kind: 'cwd', values: [agent.session.header.cwd ?? null] }], signal))
        .filter(record => record.header.origin !== 'subagent'
          && (record.header.id === agent.id || (record.persisted && agents.get(record.header.id) === undefined)))
      const titles = await query.readTitleSnapshots(records.map(record => record.header.id), signal)
      signal.throwIfAborted()
      const names = new Map(titles.flatMap(result => result.status === 'fulfilled'
        && result.value.title !== undefined ? [[result.sessionId, result.value.title.title] as const] : []))
      previous.controller.notify(undefined)
      const selected = await previous.controller.interactions.choose({
        title: this.copy.chooseSession, initial: agent.id,
        choices: [
          { value: '', label: this.copy.newSession },
          ...records.map(record => ({ value: record.header.id, label: names.get(record.header.id) ?? record.header.id,
            description: `${record.header.id} · ${new Date(record.header.createdAt).toISOString()}`,
            current: record.header.id === agent.id })),
        ],
        ...titles.some(result => result.status === 'rejected') ? { warning: this.copy.sessionTitlesUnavailable } : {},
      }, signal)
      signal.throwIfAborted()
      if (selected === undefined) { previous.controller.notify(this.copy.sessionsCancelled); return }
      if (selected === agent.id) return
      this.assertAvailable(agent)
      previous.controller.notify(this.copy.sessionOpening)
      next = await this.connect(selected === ''
        ? { ...this.options.preset === undefined ? {} : { preset: this.options.preset } }
        : { resume: selected }, signal)
      signal.throwIfAborted()
      this.assertAvailable(agent)
      // From this point the new controller owns input; Escape cannot undo retirement.
      this.operation!.committed = true
      this.current = next
      next = undefined
      previous.controller.close()
      this.changed()
      await this.dispose(previous)
    } finally {
      offStatus(); offInbox()
      if (next !== undefined) await this.dispose(next)
    }
  }

  private async dispose(session: ConnectedSession): Promise<void> {
    session.controller.close()
    try { await session.controller.drain() } finally { await session.handle.dispose() }
  }
}
