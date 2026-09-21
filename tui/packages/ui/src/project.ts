/**
 * Session events to view rows. Pure: one event in, zero or more rows out, with
 * no clock, no I/O, and no Cordis. The same function serves live events,
 * replayed history, and recorded harness fixtures, which is what lets the
 * component loop run under Bun with no harness at all.
 *
 * @module @dsh-tui/ui/project
 */

import type { SessionEvent } from '@deepseek-ai/dsh-session'
import { attachmentSummaries, type Row } from './rows.ts'

/** Rows for one event; empty when the event has no terminal presentation. */
export type Projection = readonly Row[]

const NONE: Projection = []

/**
 * Project one session event into transcript rows.
 *
 * `SessionEventMap` is merge-extensible, so an unrecognized event type is not
 * an error: a build that does not know a type renders nothing for it rather
 * than refusing the transcript.
 *
 * @param event - the committed session event.
 * @returns the rows this event contributes, in display order.
 */
export function project(event: SessionEvent): Projection {
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

    case 'tool/call':
      return [{
        kind: 'tool-call',
        callId: String(event.data.callId),
        tool: event.data.name,
        input: event.data.arguments,
      }]

    case 'tool/result': {
      const block = event.data.message.content[0]
      if (block === undefined) return NONE
      return [{
        kind: 'tool-result',
        callId: String(block.toolCallId),
        ok: block.isError !== true,
        text: resultText(block.content),
      }]
    }

    case 'turn/end': {
      const reason = event.data.reason
      if (reason.kind === 'error') {
        return [{ kind: 'notice', tone: 'error', text: `${reason.error.code}: ${reason.error.message}` }]
      }
      return NONE
    }

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
