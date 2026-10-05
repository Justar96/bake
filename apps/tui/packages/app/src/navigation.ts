/** Terminal session navigation over Harness query, setup, and handle ownership. */
import type { Context } from '@deepseek-ai/cordis'
import type { Agent, AgentHandle } from 'bake-agent'
import { SessionInUseError } from '@deepseek-ai/dsh-cmdline'
import type { TuiCopy } from '@dsh-tui/ui/copy.ts'
import { formatAge } from '@dsh-tui/ui/format.ts'
import type { AttachmentOptions } from './attachments.ts'
import { SessionController } from './controller.ts'
import { needsPreset, openElsewhere, openSession, type SessionOptions } from './session.ts'
import type { Updates } from './update.ts'
import type { Preferences } from './preferences.ts'
import type { LoginSources } from './login.ts'

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
   * @param login - the key references and flows `/login` offers.
   * @param changed - renderer notification.
   * @param updates - the process's updater, registered as `/update` in every session. Absent, there is no `/update`.
   * @param preferences - the terminal's settings, registered as `/settings` in every session. Absent, there is no `/settings`.
   */
  constructor(private readonly ctx: Context, private readonly options: SessionOptions & AttachmentOptions,
    private readonly copy: TuiCopy, private readonly login: LoginSources,
    private readonly changed: () => void, private readonly updates?: Updates,
    private readonly preferences?: Preferences) {}

  /** The displayed session. Unavailable until `start` resolves. */
  get controller(): SessionController | undefined { return this.current?.controller }
  /** Navigation owns input until preparation or retirement has settled. */
  get busy(): boolean { return this.operation !== undefined }

  /**
   * Open the startup session and replay its history.
   * @param signal - application startup lifetime.
   * @throws {SessionInUseError} when another Bake process has the `--resume` session open.
   */
  async start(signal: AbortSignal): Promise<void> {
    try {
      this.current = await this.connect(this.options, signal)
    } catch (error) {
      // Refused before the first frame, so the launcher reports it on stderr.
      if (openElsewhere(error)) throw new SessionInUseError(`${error.sessionId}: ${this.copy.sessionInUseLaunch}`, error)
      throw error
    }
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
        // Two commands, each under a second name. The aliases say whose they
        // are, so the menu and /help do not read as four features.
        for (const [name, description] of [
          ['resume', this.copy.resumeSession], ['sessions', `${this.copy.aliasOf} /resume`],
          ['new', this.copy.newSessionCommand], ['clear', `${this.copy.aliasOf} /new`],
        ] as const) agent.ctx.effect(() => commands.register({
          name, description, recordInput: false,
          handler: ({ rawInput }) => {
            if (rawInput.trim() !== '') return { kind: 'error', text: `${this.copy.usage}: /${name}` }
            // Refused now, so the command's record says why instead of a later notice.
            const refused = this.busy ? this.copy.sessionsBusy : this.unavailable(agent)
            if (refused !== undefined) return { kind: 'error', text: refused }
            this.request(agent, name === 'new' || name === 'clear')
            return { kind: 'success' }
          },
        }))
        const updates = this.updates
        if (updates !== undefined) agent.ctx.effect(() => commands.register({
          name: 'update', description: this.copy.updateCommand, recordInput: false,
          handler: ({ rawInput, signal }) => rawInput.trim() !== ''
            ? { kind: 'error', text: this.copy.updateUsage }
            : updates.update(this.copy, signal),
        }))
        const preferences = this.preferences
        if (preferences !== undefined) agent.ctx.effect(() => commands.register({
          name: 'settings', description: this.copy.settingsCommand, recordInput: false,
          handler: ({ rawInput, signal }) => {
            const session = controller
            if (rawInput.trim() !== '' || session === undefined) return { kind: 'error', text: this.copy.settingsUsage }
            const routerAccount = session.routerAccount()
            return preferences.panel(this.copy, session.interactions, signal,
              { chooseModel: modelSignal => session.chooseModel(modelSignal), listModels: modelSignal => session.listModels(modelSignal),
                describeRoutes: routeSignal => session.describeRoutes(routeSignal), ...routerAccount === undefined ? {} : { routerAccount } })
          },
        }))
        controller = new SessionController(this.ctx, agent, this.copy, this.login,
          () => { if (this.controller === controller && !this.closed) this.changed() },
          { ...this.options, ...this.preferences === undefined ? {} : { recentModels: this.preferences } }, selection)
        this.candidate = controller
      }, this.login)
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
      this.controller?.notify(this.failure(error, abort.signal))
    }).finally(() => { this.operation = undefined; if (!this.closed) this.changed() })
    this.operation = { abort, done, committed: false }
    this.changed()
  }

  /**
   * Word a navigation failure for the displayed session's notice line.
   * @param error - why preparation stopped.
   * @param signal - the navigation's own cancellation.
   * @returns the localized notice.
   */
  private failure(error: unknown, signal: AbortSignal): string {
    if (signal.aborted) return this.copy.sessionsCancelled
    if (openElsewhere(error)) return this.copy.sessionInUse
    if (needsPreset(error)) return this.copy.sessionNeedsPreset
    return `${this.copy.sessionsError}: ${error instanceof Error ? error.message : String(error)}`
  }

  /** Why the Session cannot be left now, or undefined when it can. */
  private unavailable(agent: Agent): string | undefined {
    if (this.controller?.attachments.pending) return this.copy.attachmentsBeforeNavigation
    if (agent.status !== 'idle') return this.copy.sessionsIdle
    if (agent.inbox.nextStep.length > 0 || agent.inbox.nextTurn.length > 0) return this.copy.sessionsPending
    return undefined
  }

  private assertAvailable(agent: Agent): void {
    const refused = this.unavailable(agent)
    if (refused !== undefined) throw new Error(refused)
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
    const projections = this.ctx.get('sessionProjections')
    if (projections === undefined) throw new Error('tui: sessionProjections is required')
    const offStatus = this.ctx.on('agent/status', payload => { if (payload.agent === agent) recheck() })
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
    // Last use, from the newest logged event, so a session reopened today lists
    // above one merely created later. A log the query could not read falls back
    // to its creation time.
    const used = new Map(titles.flatMap(result => result.status === 'fulfilled'
      && result.value.lastEventAt !== undefined ? [[result.sessionId, result.value.lastEventAt] as const] : []))
    const usedAt = (record: Pick<typeof records[number], 'header'>): number =>
      used.get(record.header.id) ?? record.header.createdAt
    // A session opened and left without a turn has nothing to go back to. A log
    // the query could not read stays listed, since its contents are unknown.
    const unused = new Set(titles.flatMap(result => result.status === 'fulfilled'
      && result.value.startedTurn !== true ? [result.sessionId] : []))
    const saved = records.filter(record => record.header.id !== agent.id && !unused.has(record.header.id))
      .sort((left, right) => usedAt(right) - usedAt(left)
        || left.header.id.localeCompare(right.header.id))
    const current = records.find(record => record.header.id === agent.id) ?? { header: agent.session.header }
    previous.controller.notify(undefined)
    const now = Date.now()
    const age = { now: this.copy.ageNow, minutes: this.copy.ageMinutes, hours: this.copy.ageHours, days: this.copy.ageDays }
    return previous.controller.interactions.choose({
      title: this.copy.chooseSession,
      // The pointer starts on the most recently used other session, so the
      // picker's Enter goes back to it; choosing the open one does nothing.
      initial: saved[0]?.header.id ?? agent.id,
      choices: [
        ...[current, ...saved].map(record => {
          // An untitled session reads as one, not as its id. The short id tells
          // two apart, and the full id stays searchable through the choice's value.
          const when = formatAge(usedAt(record), now, age)
          return {
            value: record.header.id, label: names.get(record.header.id) ?? this.copy.untitledSession,
            description: `${when} · ${shortId(record.header.id)}`,
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
