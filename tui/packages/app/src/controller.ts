/** Session observers and human actions shared by the renderer and integration tests. */
import type { Context } from '@deepseek-ai/cordis'
import type { Agent, AssistantStreamFrame } from '@deepseek-ai/dsh-agent'
import { BlockAssembler, createUserMessage } from '@deepseek-ai/dsh-llm'
import { assertNever } from '@deepseek-ai/dsh-util-values'
import { parseCommand } from '@deepseek-ai/dsh-commands'
import type { SessionEvent } from '@deepseek-ai/dsh-session'
import type {} from '@deepseek-ai/dsh-session-query'
import type {} from '@deepseek-ai/dsh-compaction'
import type {} from '@deepseek-ai/dsh-session-projection'
import { attachmentSummaries } from '@dsh-tui/ui/rows.ts'
import { appendTranscript, emptyTranscript, project, projector, type Projector, type Row } from '@dsh-tui/ui'
import type { TuiCopy } from '@dsh-tui/ui/copy.ts'
import { AttachmentDraft, type AttachmentOptions } from './attachments.ts'
import { Interactions } from './interactions.ts'
import { InputCatalog } from './catalog.ts'
import { FileReferences } from './references.ts'
import { listTargets, login } from './login.ts'
import { listRoutes, routeOf, resolveRoute, resolveSelection } from './model.ts'
import type { ModelSelectionRef } from '@deepseek-ai/dsh-agent'
// Empty type imports: each declaration-merges a key into the projection map
// (`contextPressure`, `todos`), and those keys are invisible here without them.
import type {} from '@deepseek-ai/dsh-token-meter'
import type {} from '@deepseek-ai/dsh-tool-todo/types'

/** One terminal's presentation over a live Agent and its durable projections. */
export class SessionController {
  readonly interactions: Interactions
  /** Unsubmitted file bytes for this session only. */
  readonly attachments: AttachmentDraft
  private submission: { abort: AbortController; done: Promise<boolean> } | undefined
  private readonly catalog: InputCatalog
  /** Cancellable discovery for the composer’s active workspace-path query. */
  readonly references: FileReferences
  private committed = emptyTranscript
  /** This transcript's words and tool cards; one projector per session. */
  private readonly projector: Projector
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
   * @param attachmentOptions - validated draft byte and count limits.
   * @param selection - harness reference for the active model selection, when available.
   */
  constructor(private readonly ctx: Context, readonly agent: Agent, private readonly copy: TuiCopy,
    private readonly credentialRefs: readonly string[], private readonly changed: () => void,
    attachmentOptions: AttachmentOptions, private readonly selection?: ModelSelectionRef) {
    this.attachments = new AttachmentDraft(agent, attachmentOptions, copy)
    // The tool registry is the lookup: a tool contributed by any plugin
    // presents its own calls here without this surface knowing it exists, and
    // a profile with no tools service renders every call at its raw arguments.
    const tools = agent.ctx.get('tools')
    this.projector = projector(copy, name => tools?.get(name))
    this.interactions = new Interactions(ctx, agent, () => this.repaint())
    const commands = agent.ctx.get('commands')
    if (commands === undefined) throw new Error('tui: commands service is required')
    for (const definition of [
      { name: 'attach', description: copy.attachFile, handler: async ({ rawInput, signal }: { rawInput: string; signal: AbortSignal }) => { await this.attachments.add(rawInput.trim(), signal) } },
      { name: 'remove-attachment', description: copy.removeAttachment, handler: ({ rawInput }: { rawInput: string }) => { this.attachments.remove(rawInput.trim()) } },
      { name: 'clear-attachments', description: copy.clearAttachments, handler: ({ rawInput }: { rawInput: string }) => {
        if (rawInput.trim() !== '') throw new Error(copy.clearAttachmentsUsage)
        this.attachments.clear()
      } },
    ]) this.off.push(agent.ctx.effect(() => commands.register({
      ...definition, recordInput: false,
      handler: async invocation => { await definition.handler(invocation); this.repaint(); return { kind: 'success' } },
    })))
    this.off.push(agent.ctx.effect(() => commands.register({
      name: 'login', description: copy.signIn, recordInput: false,
      handler: async ({ rawInput, signal }) => {
        await this.runLogin(rawInput.trim(), signal)
        return { kind: 'success' }
      },
    })))
    this.off.push(agent.ctx.effect(() => commands.register({
      name: 'model', description: copy.selectModel, recordInput: false,
      handler: async ({ rawInput, signal }) => {
        await this.runModel(rawInput.trim(), signal)
        return { kind: 'success' }
      },
    })))
    this.off.push(agent.ctx.effect(() => commands.register({
      name: 'clear-pending', description: copy.clearPending,
      handler: ({ rawInput }) => {
        if (rawInput.trim() !== '') return { kind: 'error', text: copy.clearPendingUsage }
        const pending = [...agent.inbox.nextStep, ...agent.inbox.nextTurn].filter(message => message.source.kind === 'user')
        for (const message of pending) agent.inbox.remove(message.id)
        return { kind: 'success', text: pending.length === 0 ? copy.noPending : copy.pendingCleared }
      },
    })))
    this.off.push(agent.ctx.effect(() => commands.register({
      name: 'help', description: copy.listCommands, recordInput: false,
      handler: () => {
        // The registry is the list: a command contributed by any plugin appears
        // here without this surface knowing it exists.
        //
        // Returned rather than notified, so the catalog commits to the
        // transcript. The notice region is bounded by the terminal's height and
        // a list of every registered command does not fit it; scrollback has
        // room for the whole thing and can scroll it.
        return {
          kind: 'success',
          text: commands.list(this.agent)
            .map(command => `/${command.name} — ${command.description}`)
            .join('\n'),
        }
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
      if (key === 'inbox' || key === 'contextPressure' || key === 'todos') this.repaint()
    }))
    this.references = new FileReferences(agent, copy, () => this.repaint())
    this.catalog = new InputCatalog(ctx, agent, copy, () => this.repaint())
    this.catalog.refresh()
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
      .map(message => ({ id: message.id, target, text: message.content.flatMap(block => block.type === 'text' ? [block.text] : []).join(''), attachments: attachmentSummaries(message.content) })))
    const pressure = projections?.snapshot(this.agent.session, ['contextPressure']).values.contextPressure
    // The agent's list, not a log of writes to it: `todos` folds every
    // `todo/write` to the latest whole list, which is the only version that
    // is still true.
    const todos = projections?.stateOf(this.agent.session, 'todos')
    const used = pressure?.projectedTokens
    const window = pressure?.contextWindow
    return {
      committed: this.committed, live: this.live, pending, status: this.agent.status,
      stopping: this.stopping, command: this.command?.text, notice: this.notice,
      interaction: this.interactions.current,
      todos: todos === undefined || todos === null ? undefined
        : todos.map(item => ({ text: item.content, status: item.status })),
      completion: this.catalog.view, files: this.references.view, attachments: this.attachments.view,
      model: this.selection?.current === undefined
        ? `${this.agent.options.provider}/${this.agent.options.model}` : `${routeOf(this.selection.current)}${this.selection.current.reasoningEffort === undefined ? '' : ` (${this.selection.current.reasoningEffort})`}`,
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
   * @returns acceptance; asynchronous attachment failure retains the composer draft.
   */
  submit(text: string): boolean | Promise<boolean> {
    if (this.closed || this.submission !== undefined) return false
    this.notice = undefined
    const parsed = parseCommand(text)
    if (parsed === undefined || this.ctx.get('commands')?.find(this.agent, parsed.name) === undefined) {
      if (this.command !== undefined && parseCommand(this.command.text)?.name === 'attach') { this.notify(this.copy.commandBusy); return false }
      if (this.attachments.pending) {
        if (this.command !== undefined) { this.notify(this.copy.commandBusy); return false }
        return this.submitAttachments(text)
      }
      if (text.trim() === '') return false
      const message = createUserMessage({ content: [{ type: 'text', text }], source: { kind: 'user' } })
      if (this.agent.status === 'running') this.agent.steer(message)
      else this.agent.followup(message)
      this.repaint()
      return true
    }
    if (this.attachments.pending && !['attach', 'remove-attachment', 'clear-attachments', 'model', 'login', 'help', 'clear-pending', 'sessions'].includes(parsed.name)) {
      this.notify(this.copy.attachmentCommandsUnsupported); return false
    }
    if (this.command !== undefined) { this.notify(this.copy.commandBusy); return false }
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
    return true
  }

  /** Cancel the nearest interaction or command; otherwise interrupt while retaining visible pending work. */
  cancel(): void {
    if (this.submission !== undefined) { this.submission.abort.abort(); return }
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
    this.catalog.close()
    this.references.close()
    for (const off of this.off) off()
    this.interactions.dispose()
    this.command?.abort.abort()
    this.submission?.abort.abort()
    this.attachments.clear()
  }

  /** @returns after outstanding command and catalog work has settled. */
  async drain(): Promise<void> { await Promise.all([this.submission?.done, this.command?.done, this.catalog.drain(), this.references.drain()]) }

  private submitAttachments(text: string): Promise<boolean> {
    const abort = new AbortController()
    const done = Promise.resolve().then(async () => {
      const content = await this.attachments.admit(this.selection, abort.signal)
      abort.signal.throwIfAborted()
      if (this.ctx.get('agents')?.get(this.agent.id) !== this.agent) throw new Error(this.copy.sessionsCancelled)
      const message = createUserMessage({ content: [...text.trim() === '' ? [] : [{ type: 'text' as const, text }], ...content], source: { kind: 'user' } })
      if (this.agent.status === 'running') this.agent.steer(message)
      else this.agent.followup(message)
      this.attachments.clear()
      return true
    }).catch((error: unknown) => {
      this.notify(abort.signal.aborted ? this.copy.attachmentCancelled : error instanceof Error ? error.message : String(error))
      return false
    }).finally(() => { this.submission = undefined; this.repaint() })
    this.submission = { abort, done }
    this.repaint()
    return done
  }

  private repaint(): void { if (!this.closed) this.changed() }

  private append(event: SessionEvent): void {
    if (event.seq <= this.cursor) return
    this.cursor = event.seq
    this.committed = appendTranscript(this.committed, project(event, this.projector))
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

  /**
   * Choose a model and optional effort, committing only a fully accepted idle selection.
   * @param input - optional provider/model and reasoning-effort arguments.
   * @param commandSignal - owning command lifetime.
   */
  private async runModel(input: string, commandSignal: AbortSignal): Promise<void> {
    const llm = this.agent.ctx.get('llm')
    const selection = this.selection
    if (llm === undefined || selection?.current === undefined) { this.notify(this.copy.noModelSelection); return }
    if (this.agent.status === 'running') { this.notify(this.copy.modelBusy); return }
    const args = input.split(/\s+/).filter(part => part !== '')
    if (args.length > 2) { this.notify(this.copy.modelUsage); return }
    const current = selection.current
    const busy = new AbortController()
    const signal = AbortSignal.any([commandSignal, busy.signal])
    const off = this.ctx.on('agent/status', payload => {
      if (payload.agent === this.agent && payload.status === 'running') busy.abort(new Error(this.copy.modelBusy))
    })
    try {
      let [route, effort] = args
      if (route === undefined) {
        this.notify(this.copy.modelsLoading)
        const catalog = await listRoutes(llm, current, signal)
        signal.throwIfAborted()
        this.notify(undefined)
        route = await this.interactions.choose({
          title: this.copy.chooseModel, initial: routeOf(current),
          choices: catalog.entries.map(entry => ({ value: entry.route, label: entry.route, current: entry.current, description: entry.name })),
          ...catalog.unavailable.length === 0 ? {} : { warning: `${this.copy.modelCatalogError}: ${catalog.unavailable.join(', ')}` },
        }, signal)
        signal.throwIfAborted()
        if (route === undefined) { this.notify(this.copy.modelCancelled); return }
        const info = await resolveRoute(llm, route, signal)
        if (info === undefined) { this.notify(`${this.copy.unknownModel}: ${route}`); return }
        if ((info.reasoning?.efforts.length ?? 0) > 0) {
          const sameRoute = route === routeOf(current)
          const picked = await this.interactions.choose({
            title: `${this.copy.chooseEffort}: ${route}`,
            initial: sameRoute ? current.reasoningEffort ?? '' : '',
            choices: [
              { value: '', label: this.copy.providerDefault, current: sameRoute && current.reasoningEffort === undefined,
                ...info.reasoning?.defaultEffort === undefined ? {} : { description: info.reasoning.defaultEffort } },
              ...info.reasoning!.efforts.map(item => ({ value: item.id, label: item.name,
                current: sameRoute && current.reasoningEffort === item.id,
                ...item.description === undefined ? {} : { description: item.description },
              })),
            ],
          }, signal)
          signal.throwIfAborted()
          if (picked === undefined) { this.notify(this.copy.modelCancelled); return }
          effort = picked === '' ? undefined : picked
        }
      }
      const result = await resolveSelection(llm, route, effort, signal)
      signal.throwIfAborted()
      switch (result.kind) {
        case 'selected':
          selection.current = result.selection
          this.notify(`${this.copy.modelSelected}: ${routeOf(result.selection)}${result.selection.reasoningEffort === undefined ? '' : ` (${result.selection.reasoningEffort})`}`)
          return
        case 'unknown-effort': this.notify(`${this.copy.unknownEffort}: ${result.offered.join(' ')}`); return
        case 'unknown-route': this.notify(`${this.copy.unknownModel}: ${result.route} (${routeOf(current)})`); return
        default: return assertNever(result)
      }
    } finally { off() }
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
