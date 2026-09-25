/** Session observers and human actions shared by the renderer and integration tests. */
import type { Context } from '@deepseek-ai/cordis'
import type { Agent, AssistantStreamFrame } from '@deepseek-ai/dsh-agent'
import { createUserMessage } from '@deepseek-ai/dsh-llm'
import type { LlmModelReasoningInfo } from '@deepseek-ai/dsh-llm'
import { assertNever } from '@deepseek-ai/dsh-util-values'
import { parseCommand } from '@deepseek-ai/dsh-commands'
import type { SessionEvent } from '@deepseek-ai/dsh-session'
import type {} from '@deepseek-ai/dsh-session-query'
import type {} from '@deepseek-ai/dsh-compaction'
import type {} from '@deepseek-ai/dsh-session-projection'
import { attachmentSummaries } from '@dsh-tui/ui/rows.ts'
import { Actions, announcedCalls, appendTranscript, emptyTranscript, project, projector, SETTLES, type Projector, type Row } from '@dsh-tui/ui'
import type { TuiCopy } from '@dsh-tui/ui/copy.ts'
import { AttachmentDraft, type AttachmentOptions } from './attachments.ts'
import { LiveBlocks } from './live.ts'
import { Printed } from './printed.ts'
import { Interactions } from './interactions.ts'
import { InputCatalog } from './catalog.ts'
import { SubagentCatalog, subagentEntries } from './subagents.ts'
import { subagentStatus } from '@dsh-tui/ui/subagents.tsx'
import { SubagentInspection } from './inspection.ts'
import { FileReferences } from './references.ts'
import { listTargets, login } from './login.ts'
import { bakeVersion, changelogFor } from './release.ts'
import { contextFor, goalFor, usageFor } from './status.ts'
import { listRoutes, namesRoute, routeOf, resolveRoute, resolveSelection } from './model.ts'
import type { ModelSelectionRef } from '@deepseek-ai/dsh-agent'
// Empty type imports. Each declaration-merges a key into the projection map
// (`contextPressure`, `todos`), and those keys are invisible here without them.
import type {} from '@deepseek-ai/dsh-token-meter'
import type {} from '@deepseek-ai/dsh-tool-todo/types'
import type {} from '@deepseek-ai/dsh-plan-mode/types'
import type {} from '@deepseek-ai/dsh-permission-presets/types'
import type {} from '@deepseek-ai/dsh-goal'

/** One terminal's presentation over a live Agent and its durable projections. */
export class SessionController {
  readonly interactions: Interactions
  /** Unsubmitted file bytes for this session only. */
  readonly attachments: AttachmentDraft
  private submission: { abort: AbortController; done: Promise<boolean> } | undefined
  private readonly catalog: InputCatalog
  private readonly subagents: SubagentCatalog
  private inspection: SubagentInspection | undefined
  /** Cancellable discovery for the composer’s active workspace-path query. */
  readonly references: FileReferences
  private committed = emptyTranscript
  /** This transcript's words and tool cards; one projector per session. */
  private readonly projector: Projector
  private buffered: SessionEvent[] | undefined = []
  private cursor = -1
  private reasoning: { route: string; info: LlmModelReasoningInfo | undefined } | undefined
  private reasoningRevision = 0
  private readonly reasoningAbort = new AbortController()
  private reasoningLoad: Promise<void> | undefined
  /** Rows for the attempt currently streaming that have not printed yet. */
  private blocks: readonly Row[] = []
  /**
   * Actions whose block has not printed, in call order. An action is running,
   * or finished behind one that is.
   *
   * A call and its result print together, once, as one block. An action never
   * appears as a call with its outcome stacked a few rows below it. Order is
   * preserved, so parallel calls appear in the order the model made them,
   * however they finish.
   */
  private readonly actions = new Actions()
  private stream: { attemptId: string; blocks: LiveBlocks; printed: Printed } | undefined
  private streamRevision = -1
  private stopping = false
  private command: {
    text: string; abort: AbortController; done: Promise<void>
    commandId?: string; compactPhase?: 'preparing' | 'summarizing' | 'saving'
  } | undefined
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
    // The tool registry is the lookup. A tool contributed by any plugin
    // presents its own calls here without this surface knowing it exists. A
    // profile with no tools service renders every call at its raw arguments.
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
      name: 'agents', description: copy.listSubagents, recordInput: false,
      handler: async ({ rawInput, signal }) => {
        if (rawInput.trim() !== '') return { kind: 'error', text: copy.agentsUsage }
        const entries = await this.subagents.list(signal)
        if (entries === undefined) return { kind: 'error', text: copy.subagentsUnavailable }
        const children = subagentEntries(this.subagents.view, ctx, copy)
        if (children.length === 0) return { kind: 'success', text: copy.noSubagents }
        const selected = await this.interactions.choose({
          title: copy.subagentsTitle, initial: children.find(child => child.inspectable)?.id ?? children[0]!.id,
          choices: children.map(child => ({ value: child.id, label: child.label,
            description: `${child.detail} · ${child.id}`,
            status: { text: subagentStatus(child, copy),
              ...child.state === 'working' ? { tone: 'waiting' as const }
                : child.state === 'issue' || child.outcome === 'failed' ? { tone: 'failed' as const }
                : child.outcome === 'completed' ? { tone: 'done' as const }
                : child.outcome === 'stopped' ? { tone: 'waiting' as const } : {} },
          })),
          warning: copy.subagentChoose,
        }, signal)
        signal.throwIfAborted()
        if (selected === undefined) return { kind: 'success' }
        const child = children.find(entry => entry.id === selected)!
        if (!child.inspectable) return { kind: 'error', text: `${child.label}: ${child.detail} · ${copy.subagentNoTranscript}` }
        const entry = entries.find(entry => entry.id === selected)!
        const inspection = new SubagentInspection(ctx, entry.id, child.label, copy, () => this.repaint())
        try {
          await inspection.replay(signal)
          signal.throwIfAborted()
          if (this.closed) { inspection.close(); return { kind: 'success' } }
          this.inspection?.close()
          this.inspection = inspection
          this.repaint()
        } catch (error) { inspection.close(); throw error }
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
        // The registry is the list. A command contributed by any plugin
        // appears here without this surface knowing it exists.
        //
        // Returned, not notified. The catalog commits to the transcript. The
        // notice region is bounded by the terminal's height, and a list of
        // every registered command does not fit it. Scrollback has room for
        // the whole list and can scroll it.
        return {
          kind: 'success',
          text: commands.list(this.agent)
            .map(command => `/${command.name} — ${command.description}`)
            .join('\n'),
        }
      },
    })))
    this.off.push(agent.ctx.effect(() => commands.register({
      name: 'changelog', description: copy.changelogCommand, recordInput: false,
      handler: async ({ rawInput, signal }) => {
        if (rawInput.trim() !== '') return { kind: 'error', text: copy.changelogUsage }
        // Returned, like `/help`, so the entry commits to scrollback instead of
        // being cut to the notice region's height.
        const entry = await changelogFor(bakeVersion(), signal)
        return entry === undefined ? { kind: 'error', text: copy.changelogUnavailable } : { kind: 'success', text: entry }
      },
    })))
    this.off.push(ctx.on('session/event', (session, event) => {
      if (session !== agent.session) return
      const command = this.command
      if (command?.compactPhase !== undefined) {
        if (event.type === 'command/run' && event.data.name === 'compact' && command.commandId === undefined) {
          command.commandId = event.data.commandId
        } else if (event.type === 'compaction/start' && event.data.sourceCommandId === command.commandId) {
          command.compactPhase = 'summarizing'
        } else if (event.type === 'compaction/end' && event.data.sourceCommandId === command.commandId) {
          command.compactPhase = 'saving'
        }
      }
      if (this.buffered !== undefined) this.buffered.push(event)
      else this.append(event)
      // The rows the stream stood in for have committed. `turn/end` covers an
      // interrupted turn, which reaches neither `assistant/message` nor
      // `assistant/attempt`.
      if (event.type === 'assistant/message' || event.type === 'assistant/attempt' || event.type === 'turn/end') {
        this.blocks = []
      }
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
      if (key === 'inbox' || key === 'contextPressure' || key === 'todos' || key === 'plan' || key === 'permissions' || key === 'goal') this.repaint()
    }))
    // Activation is process-local and is not written to the goal projection.
    // Create and resume arm the goal; pause disarms it. Those transitions
    // arrive only on this event, so the header would stay stale without it.
    this.off.push(ctx.on('goal/activation-changed', ({ sessionId }) => {
      if (sessionId === agent.session.id) this.repaint()
    }))
    this.references = new FileReferences(agent, copy, () => this.repaint())
    this.catalog = new InputCatalog(ctx, agent, copy, () => this.repaint())
    this.catalog.refresh()
    this.subagents = new SubagentCatalog(ctx, agent, () => this.repaint())
    this.reasoningLoad = this.loadReasoning()
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
    const surface = projections?.snapshot(this.agent.session, ['contextPressure', 'plan', 'tokenUsage', 'permissions']).values
    const pressure = surface?.contextPressure
    // The agent's current list, not a log of writes to it. `todos` folds every
    // `todo/write` to the latest whole list, which is the only version that
    // is still current.
    const todos = projections?.stateOf(this.agent.session, 'todos')
    const plan = surface?.plan
    const goal = this.goal()
    const children = this.subagents.view
    const subagents = subagentEntries(children, this.ctx, this.copy)
    const selected = this.selection?.current
    const model = selected === undefined ? `${this.agent.options.provider}/${this.agent.options.model}` : routeOf(selected)
    const usage = usageFor(surface?.tokenUsage)
    const reasoning = selected === undefined || this.reasoning?.route !== routeOf(selected) ? undefined : this.reasoning.info
    const thinkingLevel = selected?.reasoningEffort ?? reasoning?.defaultEffort
      ?? (reasoning === undefined ? undefined : this.copy.providerDefault)
    return {
      committed: this.committed, live: this.actions.live(this.blocks), pending, status: this.agent.status,
      stopping: this.stopping, command: this.command?.text,
      ...(this.command?.compactPhase === undefined ? {} : { compactPhase: this.command.compactPhase }),
      notice: this.notice,
      interaction: this.interactions.current,
      todos: todos === undefined || todos === null ? undefined
        : todos.map(item => ({ text: item.content, status: item.status })),
      subagents,
      inspection: this.inspection?.view,
      ...plan === undefined ? {} : { plan },
      ...goal === undefined ? {} : { goal },
      ...surface?.permissions === undefined ? {} : { permission: surface.permissions.currentValue },
      ...thinkingLevel === undefined ? {} : { thinkingLevel },
      completion: this.catalog.view, files: this.references.view, attachments: this.attachments.view,
      model,
      context: contextFor(pressure, model),
      ...usage === undefined ? {} : { usage },
    }
  }

  /**
   * Live goal for this agent, including process-local activation.
   *
   * Returns undefined when the goal service is absent, and when this agent is
   * no longer the live instance. A session switch can still observe an agent
   * that is being retired, and the service rejects that read.
   */
  private goal() {
    const goals = this.ctx.get('goals')
    if (goals === undefined || this.ctx.get('agents')?.get(this.agent.id) !== this.agent) return undefined
    return goalFor(goals.get(this.agent))
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
    if (this.closed || this.submission !== undefined || this.inspection !== undefined) return false
    this.notice = undefined
    const parsed = parseCommand(text)
    if (parsed === undefined || this.ctx.get('commands')?.find(this.agent, parsed.name) === undefined) {
      if (this.command?.compactPhase !== undefined) { this.notify(this.copy.compactBusy); return false }
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
    if (this.attachments.pending && !['attach', 'remove-attachment', 'clear-attachments', 'model', 'login', 'help', 'changelog', 'agents', 'clear-pending', 'sessions', 'resume', 'new', 'clear'].includes(parsed.name)) {
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
    this.command = { text, abort, done, ...(parsed.name === 'compact' ? { compactPhase: 'preparing' as const } : {}) }
    this.repaint()
    return true
  }

  /** Cancel the nearest interaction or command; otherwise interrupt while retaining visible pending work. */
  cancel(): void {
    if (this.inspection !== undefined) {
      this.inspection.close(); this.inspection = undefined; this.repaint(); return
    }
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
    this.reasoningAbort.abort()
    this.inspection?.close()
    this.catalog.close()
    this.subagents.close()
    this.references.close()
    for (const off of this.off) off()
    this.interactions.dispose()
    this.command?.abort.abort()
    this.submission?.abort.abort()
    this.attachments.clear()
  }

  /**
   * Step the selected model's reasoning effort to the next it offers, after
   * the last wrapping to the provider default: the Shift-Tab toggle.
   *
   * The efforts are the route's own, already loaded for the status line's
   * `Think` badge, so the step is synchronous and needs no picker. A route
   * whose reasoning is still loading, or that offers no efforts, says so
   * instead. The selection is read each time a step enters prompt assembly,
   * so a change during a turn takes effect from its next step.
   */
  cycleThinking(): void {
    const current = this.selection?.current
    if (current === undefined) { this.notify(this.copy.noModelSelection); return }
    const route = routeOf(current)
    const efforts = this.reasoning?.route === route ? this.reasoning.info?.efforts ?? [] : undefined
    if (efforts === undefined) { this.notify(this.copy.thinkingLoading); return }
    if (efforts.length === 0) { this.notify(`${this.copy.thinkingUnsupported}: ${route}`); return }
    // Provider default first, then each effort in the adapter's order.
    const steps = [undefined, ...efforts.map(effort => effort.id)]
    const next = steps[(steps.indexOf(current.reasoningEffort) + 1) % steps.length]
    this.selection!.current = { provider: current.provider, model: current.model, ...next === undefined ? {} : { reasoningEffort: next } }
    const label = next === undefined ? this.copy.providerDefault : efforts.find(effort => effort.id === next)?.name ?? next
    this.notify(`${this.copy.thinking}: ${label}${this.agent.status === 'running' ? ` \u00b7 ${this.copy.thinkingNextStep}` : ''}`)
  }

  /** @returns after outstanding command and catalog work has settled. */
  async drain(): Promise<void> { await Promise.all([this.submission?.done, this.command?.done, this.reasoningLoad, this.catalog.drain(), this.subagents.drain(), this.references.drain()]) }

  /** Read only the selected route's advertised default; explicit efforts need no lookup. */
  private async loadReasoning(): Promise<void> {
    const selected = this.selection?.current
    const llm = this.agent.ctx.get('llm')
    if (selected === undefined || llm === undefined) return
    const route = routeOf(selected)
    const revision = this.reasoningRevision
    try {
      const info = await resolveRoute(llm, route, this.reasoningAbort.signal)
      if (this.closed || revision !== this.reasoningRevision || route !== routeOf(this.selection?.current ?? selected)) return
      this.reasoning = { route, info: info?.reasoning }
      this.repaint()
    } catch (error) {
      if (!this.reasoningAbort.signal.aborted) throw error
    }
  }

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

  private repaint(): void {
    // Parent approvals must stay reachable while a child is being inspected.
    if (this.inspection !== undefined && this.interactions.current !== undefined) {
      this.inspection.close()
      this.inspection = undefined
    }
    if (!this.closed) this.changed()
  }

  /**
   * Commit one event's rows, skipping an event the transcript already holds.
   * @param event - the session event to project.
   * @returns the rows it contributed, empty when it was a duplicate.
   */
  private append(event: SessionEvent): readonly Row[] {
    if (event.seq <= this.cursor) return []
    this.cursor = event.seq
    let rows = project(event, this.projector)
    // The streaming attempt already printed its finished lines. The commit
    // adds only what it has not.
    if (event.type === 'assistant/message' && this.stream !== undefined) {
      rows = this.stream.printed.reconcile(rows)
      this.stream.printed = new Printed()
    }
    // A step end follows every action it made, finished or not.
    const out = this.actions.fold(rows, SETTLES.has(event.type))
    // After the fold. The message settles the step before it, so its calls
    // keep their places until the loop dispatches each one.
    if (event.type === 'assistant/message') this.actions.announce(announcedCalls(event))
    this.committed = appendTranscript(this.committed, out)
    return out
  }

  /**
   * Close an attempt whose printed lines no commit will replace.
   *
   * Scrollback cannot be unwritten, so the lines stay. The notice says they
   * were discarded. Without it, a retry would look like the answer twice.
   */
  private discard(): void {
    if (this.stream?.printed.any !== true) return
    this.stream.printed = new Printed()
    this.committed = appendTranscript(this.committed, [{ kind: 'notice', tone: 'info', text: this.copy.attemptDiscarded }])
  }

  /** Keep process-local frames in revision order; the log remains the commit source. */
  private streamFrame(frame: AssistantStreamFrame): void {
    if (frame.type === 'start') {
      // Revisions are monotone across attempts. A delayed start cannot replace
      // the current reply or restore an attempt whose end already arrived.
      if (frame.revision <= this.streamRevision) return
      this.discard()
      this.stream = { attemptId: frame.attemptId, blocks: new LiveBlocks(), printed: new Printed() }
    }
    if (this.stream === undefined || this.stream.attemptId !== frame.attemptId) return
    if (frame.type !== 'start' && frame.revision <= this.streamRevision) return
    this.streamRevision = frame.revision
    if (frame.type === 'chunk') {
      this.stream.blocks.push(frame.chunk)
      const { print, live } = this.stream.printed.split(this.stream.blocks.keyed())
      if (print.length > 0) this.committed = appendTranscript(this.committed, print)
      this.blocks = live
    } else if (frame.type === 'end') {
      // `end` is published once the assistant message has committed, so the
      // rows these stood in for are already in the transcript. Holding them a
      // frame longer would show every block twice. An attempt that committed
      // no message printed lines nothing will replace.
      if (frame.outcome.kind === 'abandoned' || frame.outcome.eventType !== 'assistant/message') this.discard()
      this.stream = undefined
      this.blocks = []
    }
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
          choices: catalog.entries.map(entry => ({ value: entry.route, label: entry.route, current: entry.current,
            ...namesRoute(entry.name, entry.route) ? {} : { description: entry.name } })),
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
          this.reasoningRevision += 1
          this.reasoning = { route: routeOf(result.selection), info: result.reasoning }
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
    const chosen = id === '' ? await this.interactions.choose({
      title: this.copy.chooseLogin,
      initial: targets.find(target => target.id === 'cliproxyapi' && !target.configured)?.id ?? targets[0]!.id,
      choices: targets.map(target => ({
        value: target.id, label: target.label,
        status: target.configured ? { text: this.copy.configured, tone: 'done' as const } : { text: this.copy.notSet },
        description: [target.writable ? undefined : this.copy.readOnly,
          target.kind === 'cliproxyapi' ? this.copy.cliProxySetupHint : undefined,
        ].filter(part => part !== undefined).join(' · '),
      })),
    }, signal) : id
    signal.throwIfAborted()
    if (chosen === undefined) { this.notify(this.copy.loginCancelled); return }
    const result = await login(this.ctx, targets, chosen, {
      notify: notice => this.notify([notice.message, notice.url, notice.code].filter(value => value !== undefined).join(' ')),
      prompt: prompt => this.interactions.prompt(prompt, signal),
    }, signal, this.copy)
    this.notify(result.kind === 'stored' ? result.models === undefined
      ? `${result.target}: ${this.copy.stored}` : `${result.target}: ${result.models} ${result.models === 1 ? this.copy.cliProxyReadyOne : this.copy.cliProxyReady}`
      : result.kind === 'cancelled' ? this.copy.loginCancelled : `${this.copy.unknownTarget}: ${result.id}`)
  }
}
