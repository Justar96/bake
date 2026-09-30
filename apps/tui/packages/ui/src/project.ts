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
import stringWidth from 'string-width'
import { ToolCards, type ToolLookup } from './cards.ts'
import type { TuiCopy } from './copy.ts'
import { PENDING_ARGUMENTS } from './present.ts'
import { attachmentSummaries, type Row, type ToolCallRow } from './rows.ts'
import { clipCells, toolText } from './tool-output.ts'

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

/** Argument fields that name what a call acts on, in the order one is chosen as its headline. */
const PRIMARY_ARGUMENTS = ['command', 'cmd', 'file_path', 'path', 'pattern', 'url', 'query'] as const

/**
 * Cells a headline read from raw arguments may take, beside the tool's name.
 *
 * About one row of a wide terminal and two of a narrow one. Arguments longer
 * than that are a body, not a name: the head is the line a reader scans the
 * transcript by, and a tool that wants its input read declares a card.
 */
export const HEADLINE_CELLS = 96

/**
 * Cells one field may take in a `key: value` headline, so a long first field
 * leaves the next ones room to be named.
 */
const FIELD_CELLS = 40

/**
 * A call's headline from its raw arguments, for a tool that declared no card.
 *
 * The field that names what the call acts on, when it has one. Otherwise its
 * fields, as `key: value`, each value on one line: text with its whitespace
 * folded, a short list of plain values as JSON, and any other list or record
 * by what it holds first, `questions: [scope, +1]`, never as a JSON dump.
 * Text that is not a JSON object is the model's own malformed output, and it
 * stays as sent. Whichever it is, its first line is the headline, cut at
 * {@link HEADLINE_CELLS} with an ellipsis; any further lines of a primary
 * field or of malformed text stay under the head, bounded as output is.
 * Escape sequences are stripped and control characters shown, as in output.
 *
 * @param args - the raw arguments string from `tool/call`.
 * @returns the headline, bounded in cells on its first line.
 */
export function argumentsTitle(args: string): string {
  let value: unknown
  try { value = JSON.parse(args) } catch { return headline(args) }
  if (typeof value !== 'object' || value === null || Array.isArray(value)) return headline(args)
  const fields = value as Record<string, unknown>
  for (const key of PRIMARY_ARGUMENTS) {
    const primary = fields[key]
    if (typeof primary === 'string' && primary.trim() !== '') return headline(primary)
  }
  let title = ''
  for (const [key, field] of Object.entries(fields)) {
    // Past the bound, the remaining fields could only be cut away.
    if (stringWidth(title) > HEADLINE_CELLS) break
    title += `${title === '' ? '' : ', '}${oneLine(key)}: ${summary(field, FIELD_CELLS)}`
  }
  return clipCells(title, HEADLINE_CELLS)
}

/**
 * Text as a headline: its first line bounded, any further lines kept for the
 * rows under the head.
 * @param text - a primary field or unparsed arguments.
 * @returns the text with a first line of at most {@link HEADLINE_CELLS} cells.
 */
function headline(text: string): string {
  const shown = toolText(text)
  const end = shown.indexOf('\n')
  return end === -1 ? clipCells(shown, HEADLINE_CELLS) : `${clipCells(shown.slice(0, end), HEADLINE_CELLS)}${shown.slice(end)}`
}

/**
 * Text on one line: escape sequences stripped, control characters shown, and
 * each run of whitespace, line breaks included, folded to one space.
 */
const oneLine = (text: string): string => toolText(text).replace(/\s+/g, ' ').trim()

/** Whether a value has no fields of its own. */
const plain = (value: unknown): boolean => value === null || typeof value !== 'object'

/**
 * One argument value, in at most `cells` cells.
 *
 * Text is itself on one line, an empty string `""`. A list of plain values
 * that fits is its JSON, `[1,2]`. Any other list is its first item and a
 * count of the rest, `[scope, +1]`, an item that is a record standing for the
 * first text it holds. A record is its first field, `{name: build, …}`, or all
 * of them when they are plain and fit.
 *
 * @param value - a parsed JSON value.
 * @param cells - the widest the summary may be.
 * @returns the summary, never wider than `cells`.
 */
function summary(value: unknown, cells: number): string {
  if (cells < 4) return '\u2026'
  if (typeof value === 'string') return value.trim() === '' ? '""' : clipCells(oneLine(value), cells)
  if (plain(value)) return String(value)
  if (Array.isArray(value)) {
    if (value.length === 0) return '[]'
    // Each item takes a cell and a comma at least, so a longer list cannot fit.
    if (value.length * 2 + 1 <= cells && value.every(plain)) {
      const json = oneLine(JSON.stringify(value))
      if (stringWidth(json) <= cells) return json
    }
    const rest = value.length === 1 ? '' : `, +${value.length - 1}`
    return `[${summary(firstText(value[0]), cells - rest.length - 2)}${rest}]`
  }
  const entries = Object.entries(value as Record<string, unknown>)
  if (entries.length === 0) return '{}'
  if (entries.every(([, field]) => plain(field))) {
    const all = `{${entries.map(([key, field]) => `${oneLine(key)}: ${typeof field === 'string' ? oneLine(field) : String(field)}`)
      .join(', ')}}`
    if (stringWidth(all) <= cells) return all
  }
  const [key, field] = entries[0]!
  const more = entries.length === 1 ? '' : ', \u2026'
  const name = `${clipCells(oneLine(key), Math.floor(cells / 2))}: `
  return `{${name}${summary(field, cells - stringWidth(name) - more.length - 2)}${more}}`
}

/**
 * What stands for a list's first item: a record by the first text it holds,
 * since that is usually its name, id, or path; anything else as itself.
 * @param item - the list's first item.
 * @returns the value to summarize in the item's place.
 */
function firstText(item: unknown): unknown {
  if (plain(item) || Array.isArray(item)) return item
  const text = Object.values(item as Record<string, unknown>).find(field => typeof field === 'string' && field.trim() !== '')
  return text ?? item
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
      // The final answer's speed. The transcript draws nothing for it; the
      // ended turn's summary reads it from the turn's rows.
      const rate = rows.some(row => row.kind === 'assistant') ? outputRate(event.data) : undefined
      if (rate !== undefined) rows.push({ kind: 'rate', tokens: rate.tokens, ms: rate.ms })
      return rows
    }

    case 'command/run':
      return [{ kind: 'command', name: event.data.name, args: event.data.args ?? '',
        ...event.data.args === undefined ? { inputOmitted: true } : {} }]

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
      // A tool can complete normally while the work it ran fails. Shell tools
      // deliberately keep non-zero exits out of `isError` so the model gets a
      // usable result and can decide whether to retry; their terminal card
      // carries the structured `summary: 'failure'` instead. Keep the model
      // contract intact, but make the UI outcome reflect that failed work.
      const failedWork = card?.detail.some(line => line.summary === 'failure') === true
      return [{
        kind: 'tool-result',
        callId,
        ok: !isError && !failedWork,
        // A card reformats the result for a reader; showing the model-facing
        // text under it would print the same outcome twice. A card that
        // reformats nothing, such as a generic result with no `content`,
        // keeps the text, which the preview cuts as it cuts any raw result.
        text: card === undefined || card.raw === true ? text : '',
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
        case 'error': {
          // A turn that failed for want of a usable key says how to get one, under the reason.
          const hint = reason.error.code === 'MISSING_CREDENTIAL' ? copy.missingCredentialHint
            : reason.error.code === 'AUTH' || reason.error.code === 'INVALID_CREDENTIAL' ? copy.authFailedHint : undefined
          return [{ kind: 'notice', placement: 'turn-end', tone: 'error',
            text: `${reason.error.code}: ${reason.error.message}${hint === undefined ? '' : `\n${hint}`}` }]
        }
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
      return [{ kind: 'notice', tone: 'info', text: projector.copy.compacted, compaction: true }]

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
