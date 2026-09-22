/** Abortable, ordered human-interaction requests for one terminal owner. */
import type { Context } from '@deepseek-ai/cordis'
import type { Agent } from '@deepseek-ai/dsh-agent'
import type { ApprovalOutcome } from '@deepseek-ai/dsh-user-approval'
import { UserQuestionError } from '@deepseek-ai/dsh-user-questions'
import type { AuthorizationPrompt } from '@deepseek-ai/dsh-authorization/types'
import type { ChoicePrompt } from '@dsh-tui/ui/picker.tsx'
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
   * Attach answerers for one exact agent; other agents retain their own answerers.
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
          if (typeof value === 'string') throw new Error('tui: invalid question answer')
          return value
        }, () => { throw new UserQuestionError('Question cancelled', 'ASK_ABORTED') })
    }))
    agent.ctx.effect(() => () => this.dispose(), 'tui interactions')
  }

  /** The oldest outstanding question; later requests cannot replace it. */
  get current(): Interaction | undefined { return this.queue[0]?.view }

  /**
   * Settle the displayed request; stale callbacks cannot answer a later one.
   * @param id - displayed request identity.
   * @param answer - the explicit human response.
   */
  answer(id: number, answer: InteractionAnswer): void {
    if (this.queue[0]?.view.id === id) this.queue[0].answer(answer)
  }

  /** Withdraw the displayed request without granting an approval. */
  cancel(): void { this.queue[0]?.cancel() }

  /** Close all requests and reject subsequent attempts. Safe to repeat. */
  dispose(): void {
    if (this.closed) return
    this.closed = true
    for (const off of this.off) off()
    for (const pending of [...this.queue]) pending.cancel()
  }

  /**
   * Ask for authorization input without storing the answer in session history.
   * @param prompt - authorization-owned text and secrecy mode.
   * @param signal - command cancellation lifetime.
   * @returns the answer, or rejects on cancellation.
   */
  prompt(prompt: AuthorizationPrompt, signal: AbortSignal): Promise<string> {
    const message = prompt.kind === 'select'
      ? `${prompt.message} (${prompt.options.map(option => `${option.id}: ${option.label}`).join(', ')})`
      : prompt.message
    return this.enqueue({ id: ++this.nextId, kind: 'login', message, secret: prompt.kind === 'secret' }, signal,
      value => {
        if (typeof value !== 'string') throw new Error('tui: invalid authorization answer')
        return value
      }, () => { throw new Error('Authorization cancelled') })
  }

  /**
   * Request a terminal-owned choice without writing conversation input.
   * @param prompt - available values, labels, and initial selection.
   * @param signal - owning command lifetime.
   * @returns the chosen value, or undefined when dismissed.
   */
  choose(prompt: ChoicePrompt, signal: AbortSignal): Promise<string | undefined> {
    return this.enqueue({ ...prompt, id: ++this.nextId, kind: 'select' }, signal, value => {
      if (typeof value !== 'string' || !prompt.choices.some(choice => choice.value === value)) throw new Error('tui: invalid picker answer')
      return value
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
