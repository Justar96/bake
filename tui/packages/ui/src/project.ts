/**
 * Session events to view rows. Pure: one event and the surface's own
 * vocabulary in, zero or more rows out, with no clock, no I/O, and no Cordis.
 * The same function serves live events, replayed history, and recorded harness
 * fixtures, which is what lets the component loop run under Bun with no
 * harness at all.
 *
 * Every event with a terminal presentation is projected here. A caller that
 * worded its own rows for a few event types would keep those types out of the
 * fixtures and out of the Bun harness, which is why the localized labels are a
 * parameter rather than a reason to project elsewhere.
 *
 * @module @dsh-tui/ui/project
 */

import type { SessionEvent } from '@deepseek-ai/dsh-session'
// Empty type imports: each declaration-merges the events projected below into
// `SessionEventMap`, and those arms are invisible here without them.
import type {} from '@deepseek-ai/dsh-commands'
import type {} from '@deepseek-ai/dsh-compaction'
import { ToolCards, type ToolLookup } from './cards.ts'
import type { TuiCopy } from './copy.ts'
import { attachmentSummaries, type Row } from './rows.ts'

/** Rows for one event; empty when the event has no terminal presentation. */
export type Projection = readonly Row[]

/**
 * Everything the projection needs beyond the event itself: this surface's
 * words, and its view of the tools whose calls it is rendering.
 */
export interface Projector {
  /** Localized labels for the rows this surface words rather than quotes. */
  readonly copy: TuiCopy
  /** Tool cards for one session; state, so one projector serves one transcript. */
  readonly cards: ToolCards
}

const NONE: Projection = []

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
 * Project one session event into transcript rows.
 *
 * `SessionEventMap` is merge-extensible, so an unrecognized event type is not
 * an error: a build that does not know a type renders nothing for it rather
 * than refusing the transcript.
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
      return rows
    }

    case 'command/run':
      return [{ kind: 'command', name: event.data.name, args: event.data.args ?? '' }]

    case 'command/done':
      return event.data.text === undefined ? NONE
        : [{ kind: 'notice', tone: event.data.kind === 'error' ? 'error' : 'info', text: event.data.text }]

    case 'tool/call': {
      const callId = String(event.data.callId)
      const card = projector.cards.call(callId, event.data.name, event.data.arguments)
      return [{
        kind: 'tool-call',
        callId,
        tool: event.data.name,
        // Without a card the raw arguments are the headline, which is the
        // presentation every tool had before it could declare one.
        input: card?.title ?? event.data.arguments,
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
        ...card === undefined ? {} : {
          detail: card.title === '' ? card.detail : [{ text: card.title }, ...card.detail],
        },
      }]
    }

    case 'turn/end': {
      const reason = event.data.reason
      if (reason.kind === 'error') {
        return [{ kind: 'notice', tone: 'error', text: `${reason.error.code}: ${reason.error.message}` }]
      }
      if (reason.kind === 'aborted' || reason.kind === 'interrupted') {
        return [{ kind: 'notice', tone: 'warn', text: projector.copy.cancelled }]
      }
      return NONE
    }

    case 'compaction/summary':
      // The summary itself is context for the model, not for the reader: what
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
