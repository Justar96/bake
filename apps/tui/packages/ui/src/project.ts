/**
 * Session events to view rows. Pure. One event and the surface's own
 * vocabulary in, zero or more rows out, with no clock, no I/O, and no Cordis.
 * The same function serves live events, replayed history, and recorded harness
 * fixtures. That is what lets the component loop run under Bun with no
 * harness at all.
 *
 * Every event with a terminal presentation is projected here. A caller that
 * worded its own rows for a few event types would keep those types out of the
 * fixtures and out of the Bun harness. The localized labels are a parameter
 * for that reason, not a reason to project elsewhere.
 *
 * @module @dsh-tui/ui/project
 */

import type { SessionEvent } from '@deepseek-ai/dsh-session'
// Empty type imports. Each declaration-merges the events projected below into
// `SessionEventMap`, and those arms are invisible here without them.
import type {} from '@deepseek-ai/dsh-commands'
import type {} from '@deepseek-ai/dsh-compaction'
import { ToolCards, type ToolLookup } from './cards.ts'
import type { TuiCopy } from './copy.ts'
import { PENDING_ARGUMENTS } from './present.ts'
import { formatTokens } from './format.ts'
import { attachmentSummaries, type Row, type ToolCallRow } from './rows.ts'

/** Rows for one event; empty when the event has no terminal presentation. */
export type Projection = readonly Row[]

/**
 * Everything the projection needs beyond the event itself. This surface's
 * words, and its view of the tools whose calls it is rendering.
 */
export interface Projector {
  /** Localized labels for the rows this surface writes instead of quoting. */
  readonly copy: TuiCopy
  /** Tool cards for one session; state, so one projector serves one transcript. */
  readonly cards: ToolCards
}

const NONE: Projection = []

/** The logged payload of one committed assistant message. */
type AssistantMessageData = Extract<SessionEvent, { type: 'assistant/message' }>['data']

/**
 * Generation speed of a final answer. Reported output tokens divided by the
 * time from the first streamed token to the finish.
 *
 * The interval starts at the first token, not the request, so the rate is
 * generation speed, not time-to-first-token. Only an answer that
 * finished with `stop` has a rate. A step that ends in tool calls is not the
 * response, and an interrupted or failed answer has no complete count.
 *
 * @param data - the committed message, including usage and the timed stream.
 * @returns output tokens and elapsed milliseconds, or undefined when the log cannot support a rate.
 */
export function outputRate(data: AssistantMessageData): { readonly tokens: number, readonly ms: number } | undefined {
  const tokens = data.usage?.outputTokens
  if (tokens === undefined || tokens <= 0 || data.interrupted === true) return undefined
  let first: number | undefined
  let finish: number | undefined
  for (const record of data.stream) {
    if (record.type !== 'chunk') first = Math.min(first ?? record.time0, record.time0)
    else if (record.chunk.type === 'finish' && record.chunk.reason.kind === 'stop') finish = record.time
  }
  if (first === undefined || finish === undefined || finish <= first) return undefined
  return { tokens, ms: finish - first }
}

/**
 * Format an answer's rate for the dim footer. `856 tokens · 42.3 tok/s`.
 * @param rate - output tokens and the milliseconds they took.
 * @param copy - locale-owned unit labels.
 * @returns the footer text.
 */
export function formatRate(rate: { readonly tokens: number, readonly ms: number }, copy: TuiCopy): string {
  const perSecond = rate.tokens / (rate.ms / 1000)
  return `${formatTokens(rate.tokens)} ${copy.rateTokens} \u00b7 ${perSecond < 100 ? perSecond.toFixed(1) : Math.round(perSecond)} ${copy.rateUnit}`
}

/** Argument fields that name what a call acts on, in the order one is chosen as its headline. */
const PRIMARY_ARGUMENTS = ['command', 'cmd', 'file_path', 'path', 'pattern', 'url', 'query'] as const

/**
 * A call's headline from its raw arguments, for a tool that declared no card.
 *
 * The field that names what the call acts on, when it has one. Otherwise its
 * fields, as `key: value`. Text that is not a JSON object is the model's
 * own malformed output, and it stays exactly as sent.
 *
 * @param args - the raw arguments string from `tool/call`.
 * @returns one readable line of the arguments.
 */
export function argumentsTitle(args: string): string {
  let value: unknown
  try { value = JSON.parse(args) } catch { return args }
  if (typeof value !== 'object' || value === null || Array.isArray(value)) return args
  const fields = value as Record<string, unknown>
  for (const key of PRIMARY_ARGUMENTS) {
    const primary = fields[key]
    if (typeof primary === 'string' && primary.trim() !== '') return primary
  }
  return Object.entries(fields)
    .map(([key, field]) => `${key}: ${typeof field === 'string' ? field : JSON.stringify(field)}`).join(', ')
}

/**
 * Build a projector for one transcript.
 *
 * @param copy - localized labels for this terminal's locale.
 * @param lookup - resolves a recorded tool name to its presenters; a lookup
 *   that finds nothing renders every tool at its raw arguments and result.
 * @returns the projector to pass to {@link project} for this session.
 */
export function projector(copy: TuiCopy, lookup: ToolLookup): Projector {
  return { copy, cards: new ToolCards(lookup, copy) }
}

/**
 * The calls an assistant message makes, as they stand before each is dispatched.
 *
 * The message commits with every call it makes, but each call's own event
 * follows only as the loop reaches it, one after another. Until then the
 * surface knows a call is coming and what tool it is for, and nothing else,
 * so it draws it as a call still streaming. Named, arguments pending. Not part
 * of {@link project}, because these rows stand in for rows the call's event
 * will commit, and must never be committed themselves.
 *
 * @param event - the committed session event.
 * @returns the message's calls in order, empty for any other event.
 */
export function announcedCalls(event: SessionEvent): readonly ToolCallRow[] {
  if (event.type !== 'assistant/message') return NONE_CALLS
  return event.data.message.content.flatMap((block): ToolCallRow[] => block.type === 'tool-call'
    ? [{ kind: 'tool-call', callId: String(block.id), tool: block.name, input: PENDING_ARGUMENTS }]
    : [])
}

const NONE_CALLS: readonly ToolCallRow[] = []

/**
 * Project one session event into transcript rows.
 *
 * `SessionEventMap` is merge-extensible, so an unrecognized event type is not
 * an error. A build that does not know a type renders nothing for it instead
 * of refusing the transcript.
 *
 * @param event - the committed session event.
 * @param projector - this transcript's labels and tool cards.
 * @returns the rows this event contributes, in display order.
 */
export function project(event: SessionEvent, projector: Projector): Projection {
  if ('surfaceOp' in event && event.surfaceOp !== 'append') return NONE
  switch (event.type) {
    case 'user/message': {
      // `source` separates a human prompt from synthetic context the loop
      // injects (file-change notices, skill content, goal continuations).
      // Only the human's own words belong in the transcript as a user row.
      if (event.data.source.kind !== 'user') return NONE
      const text = textOf(event.data.content)
      const attachments = attachmentSummaries(event.data.content)
      return text === '' && attachments.length === 0 ? NONE : [{ kind: 'user', text, ...attachments.length === 0 ? {} : { attachments } }]
    }

    case 'assistant/message': {
      const rows: Row[] = []
      for (const block of event.data.message.content) {
        if (block.type === 'reasoning' && block.text !== '') rows.push({ kind: 'reasoning', text: block.text })
        else if (block.type === 'text' && block.text !== '') rows.push({ kind: 'assistant', text: block.text })
      }
      // The final answer's speed, under the answer it measures.
      const rate = rows.some(row => row.kind === 'assistant') ? outputRate(event.data) : undefined
      if (rate !== undefined) rows.push({ kind: 'rate', text: formatRate(rate, projector.copy) })
      return rows
    }

    case 'command/run':
      return [{ kind: 'command', name: event.data.name, args: event.data.args ?? '' }]

    case 'command/done':
      return event.data.text === undefined ? NONE
        : [{ kind: 'notice', placement: 'command', tone: event.data.kind === 'error' ? 'error' : 'info', text: event.data.text }]

    case 'tool/call': {
      const callId = String(event.data.callId)
      const card = projector.cards.call(callId, event.data.name, event.data.arguments)
      return [{
        kind: 'tool-call',
        callId,
        tool: event.data.name,
        // Without a card the arguments are the headline, shown as text and not
        // as JSON. `Bash(echo ok)`, not `Bash({"command": "echo ok"})`.
        input: card?.title ?? argumentsTitle(event.data.arguments),
        ...card === undefined || card.detail.length === 0 ? {} : { detail: card.detail },
      }]
    }

    case 'tool/result': {
      const block = event.data.message.content[0]
      if (block === undefined) return NONE
      const callId = String(block.toolCallId)
      const isError = block.isError === true
      const text = resultText(block.content)
      const card = projector.cards.result(callId, {
        content: [{ type: 'text', text }],
        isError,
        ...event.data.meta === undefined ? {} : { meta: event.data.meta },
      })
      return [{
        kind: 'tool-result',
        callId,
        ok: !isError,
        // A card reformats the result for a reader; showing the model-facing
        // text under it would print the same outcome twice.
        text: card === undefined ? text : '',
        // The title stays out of `detail` so a bound that reports the body as a
        // count keeps it. `Edit packages/ui/src/app.tsx` is the part of an
        // applied diff a reader needs after the hunks have scrolled away.
        ...card === undefined || card.title === '' ? {} : { title: card.title },
        ...card === undefined || card.detail.length === 0 ? {} : { detail: card.detail },
      }]
    }

    case 'turn/end': {
      const { reason } = event.data
      const { copy } = projector
      const kind: string = reason.kind
      switch (reason.kind) {
        case 'completed':
          return [{ kind: 'notice', placement: 'turn-end', tone: 'info', text: copy.turnCompleted }]
        case 'error':
          return [{ kind: 'notice', placement: 'turn-end', tone: 'error', text: `${reason.error.code}: ${reason.error.message}` }]
        case 'aborted':
        case 'interrupted':
          return [{ kind: 'notice', placement: 'turn-end', tone: 'warn', text: copy.cancelled }]
        case 'blocked':
          return [{ kind: 'notice', placement: 'turn-end', tone: 'warn', text: copy.turnBlocked }]
        case 'max-tokens':
          return [{ kind: 'notice', placement: 'turn-end', tone: 'warn', text: copy.turnMaxTokens }]
        default:
          // Plugins may add end reasons; show the recorded kind, never success.
          return [{ kind: 'notice', placement: 'turn-end', tone: 'warn', text: kind }]
      }
    }

    case 'compaction/summary':
      // The summary itself is context for the model, not for the reader. What
      // the reader needs to know is that history behind this point was folded.
      return [{ kind: 'notice', tone: 'info', text: projector.copy.compacted }]

    default:
      // Events without terminal presentation contribute no rows.
      return NONE
  }
}

/**
 * Concatenate the text blocks of a message's content.
 *
 * @param content - the message content blocks.
 * @returns the joined text, empty when the message carries none.
 */
function textOf(content: readonly { type: string, text?: string }[]): string {
  return content.filter(block => block.type === 'text').map(block => block.text ?? '').join('')
}

/**
 * Render a tool result's content as terminal text.
 *
 * @param content - the result content, already normalized by the tool surface.
 * @returns the text to display.
 */
function resultText(content: unknown): string {
  if (typeof content === 'string') return content
  if (!Array.isArray(content)) return ''
  return content
    .map((block: unknown) =>
      typeof block === 'object' && block !== null && 'text' in block && typeof block.text === 'string'
        ? block.text
        : '')
    .join('')
}
