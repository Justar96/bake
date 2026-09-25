/** Terminal session navigation over Harness query, setup, and handle ownership. */
import type { Context } from '@deepseek-ai/cordis'
import type { Agent, AgentHandle } from '@deepseek-ai/dsh-agent'
import type { TuiCopy } from '@dsh-tui/ui/copy.ts'
import { formatAge } from '@dsh-tui/ui/format.ts'
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

  /** The displayed session. Unavailable until `start` resolves. */
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
   * @returns acceptance. A rejected or pending admission must keep the composer draft.
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

  /** Close observers before terminal release. `drain` owns asynchronous disposal. */
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
        for (const [name, description] of [
          ['sessions', this.copy.chooseSession], ['resume', this.copy.resumeSession],
          ['new', this.copy.newSessionCommand], ['clear', this.copy.clearSessionCommand],
        ] as const) agent.ctx.effect(() => commands.register({
          name, description, recordInput: false,
          handler: ({ rawInput }) => {
            if (rawInput.trim() !== '') return { kind: 'error', text: name === 'new' || name === 'clear'
              ? this.copy.newSessionUsage : this.copy.sessionsUsage }
            this.request(agent, name === 'new' || name === 'clear')
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

  private request(agent: Agent, newSession: boolean): void {
    if (this.closed || this.busy || agent !== this.controller?.agent) return
    const abort = new AbortController()
    // The registry must finish `command/done` before its Session can be retired.
    const done = Promise.resolve().then(async () => {
      await this.controller?.drain()
      abort.signal.throwIfAborted()
      await this.navigate(abort.signal, newSession)
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

  private async navigate(commandSignal: AbortSignal, newSession: boolean): Promise<void> {
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
    if (projections === undefined) throw new Error('tui: sessionProjections is required')
    const offInbox = projections.onChanged((session, key) => {
      if (session === agent.session && key === 'inbox') recheck()
    })
    let next: ConnectedSession | undefined
    try {
      const selected = newSession ? '' : await this.selectSession(previous, signal)
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
      // From this point the new controller owns input. Escape cannot undo retirement.
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

  private async selectSession(previous: ConnectedSession, signal: AbortSignal): Promise<string | undefined> {
    const { agent } = previous.handle
    const query = this.ctx.get('sessionQuery')
    const agents = this.ctx.get('agents')
    if (query === undefined || agents === undefined) throw new Error('tui: sessionQuery and agents are required')
    previous.controller.notify(this.copy.sessionsLoading)
    const records = (await query.filterSessions([{ kind: 'cwd', values: [agent.session.header.cwd ?? null] }], signal))
      .filter(record => record.header.origin !== 'subagent'
        && (record.header.id === agent.id || (record.persisted && agents.get(record.header.id) === undefined)))
    const titles = await query.readTitleSnapshots(records.map(record => record.header.id), signal)
    signal.throwIfAborted()
    const names = new Map(titles.flatMap(result => result.status === 'fulfilled'
      && result.value.title !== undefined ? [[result.sessionId, result.value.title.title] as const] : []))
    const saved = records.filter(record => record.header.id !== agent.id)
      .sort((left, right) => right.header.createdAt - left.header.createdAt
        || left.header.id.localeCompare(right.header.id))
    const current = records.find(record => record.header.id === agent.id) ?? { header: agent.session.header }
    previous.controller.notify(undefined)
    const now = Date.now()
    const age = { now: this.copy.ageNow, minutes: this.copy.ageMinutes, hours: this.copy.ageHours, days: this.copy.ageDays }
    return previous.controller.interactions.choose({
      title: this.copy.chooseSession,
      initial: agent.id,
      choices: [
        ...[current, ...saved].map(record => {
          const name = names.get(record.header.id)
          // An untitled session is labelled by its id, so the id is not repeated.
          // The full id stays searchable either way through the choice's value.
          const when = formatAge(record.header.createdAt, now, age)
          return {
            value: record.header.id, label: name ?? record.header.id,
            description: name === undefined ? when : `${when} · ${shortId(record.header.id)}`,
            role: record.header.id === agent.id ? 'session-current' as const : 'session-saved' as const,
          }
        }),
        // Pinned. A long history must not scroll the way to a fresh session out of view.
        { value: '', label: this.copy.newSession, role: 'session-new' as const, pinned: true },
      ],
      ...titles.some(result => result.status === 'rejected') ? { warning: this.copy.sessionTitlesUnavailable } : {},
    }, signal)
  }

  private async dispose(session: ConnectedSession): Promise<void> {
    session.controller.close()
    try { await session.controller.drain() } finally { await session.handle.dispose() }
  }
}

/**
 * Enough of a session id to tell sessions apart.
 *
 * That is its first block of eight hex digits, as a UUID's is. Any other id
 * is short enough, or opaque enough, to show whole.
 * @param id - the session id.
 * @returns the id's leading hex block, or the id.
 */
function shortId(id: string): string {
  return /[0-9a-f]{8}/i.exec(id)?.[0] ?? id
}
