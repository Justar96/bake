/** Abortable, ordered human-interaction requests for one terminal owner. */
import type { Context } from '@deepseek-ai/cordis'
import type { Agent } from '@deepseek-ai/dsh-agent'
import type { ApprovalOutcome } from '@deepseek-ai/dsh-user-approval'
import { UserQuestionError } from '@deepseek-ai/dsh-user-questions'
import type { ChoicePrompt } from '@dsh-tui/ui/picker.tsx'
import type { LoginPrompt } from './login.ts'
import type { Interaction, InteractionAnswer } from '@dsh-tui/ui/interaction.tsx'

interface Pending {
  readonly view: Interaction
  answer(value: InteractionAnswer): void
  cancel(): void
}

/** Queue whose request signals and owning fiber bound every pending promise. */
export class Interactions {
  private queue: Pending[] = []
  private nextId = 0
  private closed = false
  private readonly off: (() => void)[] = []

  /**
   * Attach answerers for one exact agent. Other agents keep their own answerers.
   * @param ctx - owning plugin context.
   * @param agent - terminal-owned agent.
   * @param changed - repaint notification.
   */
  constructor(ctx: Context, agent: Agent, private readonly changed: () => void) {
    this.off.push(ctx.on('approval/request', (request, next) => {
      if (request.agent !== agent) return next()
      return this.enqueue<ApprovalOutcome>({
        id: ++this.nextId, kind: 'approval', tool: request.toolName, reason: request.reason ?? '',
        ...request.callId === undefined ? {} : { callId: request.callId },
      }, request.signal, value => {
        if (value !== 'allowed-once' && value !== 'rejected') throw new Error('tui: invalid approval answer')
        return value
      }, () => 'cancelled')
    }))
    this.off.push(ctx.on('user-questions/request', (request, next) => {
      if (request.agent !== agent) return next()
      return this.enqueue({ id: ++this.nextId, kind: 'questions', questions: request.questions }, request.signal,
        value => {
          if (typeof value === 'string' || !('answers' in value)) throw new Error('tui: invalid question answer')
          return value
        }, () => { throw new UserQuestionError('Question cancelled', 'ASK_ABORTED') })
    }))
    agent.ctx.effect(() => () => this.dispose(), 'tui interactions')
  }

  /** The oldest outstanding question. A later request cannot replace it. */
  get current(): Interaction | undefined { return this.queue[0]?.view }

  /**
   * Settle the displayed request. A stale callback cannot answer a later one.
   * @param id - displayed request identity.
   * @param answer - the explicit human response.
   */
  answer(id: number, answer: InteractionAnswer): void {
    if (this.queue[0]?.view.id === id) this.queue[0].answer(answer)
  }

  /** Withdraw the displayed request without granting an approval. */
  cancel(): void { this.queue[0]?.cancel() }

  /** Close all requests and reject subsequent attempts. Safe to call again. */
  dispose(): void {
    if (this.closed) return
    this.closed = true
    for (const off of this.off) off()
    for (const pending of [...this.queue]) pending.cancel()
  }

  /**
   * Ask for sign-in input without storing the answer in session history. A
   * `select` prompt is the picker every other choice uses; text and secrets
   * are the sign-in panel.
   * @param prompt - the field, with whatever the flow knows about its place and last refusal.
   * @param signal - command cancellation lifetime.
   * @returns the answer, or the chosen option's id; rejects on cancellation.
   */
  async prompt(prompt: LoginPrompt, signal: AbortSignal): Promise<string> {
    const scope = prompt.signal === undefined ? signal : AbortSignal.any([signal, prompt.signal])
    if (prompt.kind === 'select') {
      const chosen = await this.choose({
        title: prompt.title === undefined ? prompt.message : `${prompt.title} \u00b7 ${prompt.message}`,
        initial: prompt.options[0]?.id ?? '',
        choices: prompt.options.map(option => ({ value: option.id, label: option.label,
          ...option.description === undefined ? {} : { description: option.description } })),
        ...prompt.error === undefined ? {} : { warning: prompt.error },
      }, scope)
      if (chosen === undefined) throw new Error('Authorization cancelled')
      return chosen
    }
    const { kind, signal: _signal, ...field } = prompt
    return this.enqueue({ ...field, id: ++this.nextId, kind: 'login', secret: kind === 'secret' }, scope,
      value => {
        if (typeof value !== 'string') throw new Error('tui: invalid authorization answer')
        return value
      }, () => { throw new Error('Authorization cancelled') })
  }

  /**
   * Request a terminal-owned choice without writing conversation input.
   * @param prompt - available values, labels, initial selection, and any tabs.
   * @param signal - owning command lifetime.
   * @returns the chosen value or tab, or undefined when dismissed.
   */
  choose(prompt: ChoicePrompt, signal: AbortSignal): Promise<string | undefined> {
    return this.enqueue({ ...prompt, id: ++this.nextId, kind: 'select' }, signal, answer => {
      const value = typeof answer === 'string' ? answer : 'level' in answer ? answer.value : undefined
      if (value === undefined || (!prompt.choices.some(choice => choice.value === value)
        && !(prompt.tabs?.items ?? []).some(tab => tab.value === value))) throw new Error('tui: invalid picker answer')
      return value
    }, () => undefined)
  }

  /**
   * Request a choice with the level its row showed, for a prompt with
   * `levels`. A choice without levels answers no level.
   * @param prompt - available values, their levels, and the initial selection.
   * @param signal - owning command lifetime.
   * @returns the chosen value and level, or undefined when dismissed.
   */
  pick(prompt: ChoicePrompt, signal: AbortSignal): Promise<{ readonly value: string; readonly level?: string } | undefined> {
    return this.enqueue({ ...prompt, id: ++this.nextId, kind: 'select' }, signal, answer => {
      const picked = typeof answer === 'string' ? { value: answer } : 'level' in answer ? answer : undefined
      const choice = prompt.choices.find(candidate => candidate.value === picked?.value)
      if (picked === undefined || choice === undefined || ('level' in picked
        && !(choice.levels?.items ?? []).some(item => item.value === picked.level))) throw new Error('tui: invalid picker answer')
      return picked
    }, () => undefined)
  }

  private enqueue<T>(view: Interaction, signal: AbortSignal | undefined,
    decode: (value: InteractionAnswer) => T, cancelled: () => T): Promise<T> {
    return new Promise<T>((resolve, reject) => {
      let settled = false
      const settle = (value: () => T): void => {
        if (settled) return
        settled = true
        signal?.removeEventListener('abort', pending.cancel)
        this.queue = this.queue.filter(item => item !== pending)
        try { resolve(value()) } catch (error) { reject(error) }
        this.changed()
      }
      const pending: Pending = { view, answer: value => settle(() => decode(value)), cancel: () => settle(cancelled) }
      if (this.closed || signal?.aborted) { pending.cancel(); return }
      this.queue.push(pending)
      signal?.addEventListener('abort', pending.cancel, { once: true })
      this.changed()
    })
  }
}
