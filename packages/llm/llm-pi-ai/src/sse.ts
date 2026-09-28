/** SSE framing for pi-ai's HTTP JSON protocols before their parsers read event data. */
import { createParser, type EventSourceParser } from 'eventsource-parser'

/** Chooses the event name written for one nonempty event; `undefined` writes no `event:` line. */
type EventName = (event: string | undefined, data: string) => string | undefined

/**
 * Ignore empty SSE events, including a named event that a proxy heartbeat's blank
 * line terminated before its data (`event: x`, `: keep-alive`, blank line).
 * Comments, `id`, and `retry` fields are dropped; nonempty data is re-emitted with
 * the name `eventName` chooses. JSON validation and protocol completion stay with
 * pi-ai, and an unterminated event at EOF is not dispatched.
 *
 * Stream pipes propagate backpressure, read errors, and cancellation to the
 * original body. HTTP failures and non-SSE responses pass through unchanged.
 */
function normalizeSse(response: Response, eventName: EventName): Response {
  if (!response.ok || response.body === null
    || response.headers.get('content-type')?.split(';', 1)[0]?.trim().toLowerCase() !== 'text/event-stream') return response
  const encoder = new TextEncoder()
  let parser: EventSourceParser
  let endsWithCr = false
  const body = response.body
    .pipeThrough(new TextDecoderStream())
    .pipeThrough(new TransformStream<string, Uint8Array>({
      start(controller) {
        parser = createParser({ onEvent({ event, data }) {
          if (data === '') return
          const name = eventName(event, data)
          controller.enqueue(encoder.encode(
            `${name === undefined ? '' : `event: ${name}\n`}${data.split('\n').map(line => `data: ${line}`).join('\n')}\n\n`,
          ))
        } })
      },
      transform(chunk) {
        if (chunk.length === 0) return
        endsWithCr = chunk.endsWith('\r')
        parser.feed(chunk)
      },
      flush() {
        // The parser holds a trailing CR to distinguish CRLF. At EOF that CR
        // is a complete line ending; supplying its LF must not flush other tails.
        if (endsWithCr) parser.feed('\n')
      },
    }))
  const headers = new Headers(response.headers)
  // Fetch has decoded content encodings; normalization also changes byte length.
  headers.delete('content-length')
  headers.delete('content-encoding')
  return new Response(body, { status: response.status, statusText: response.statusText, headers })
}

/**
 * OpenAI Responses and Chat Completions framing: drops empty events and keeps
 * every event name as sent. An empty `response.completed` is not a completion.
 */
export function normalizeOpenAiSse(response: Response): Response {
  return normalizeSse(response, event => event)
}

/**
 * The Anthropic Messages stream event types, each sent as both the SSE event
 * name and the JSON `type`. pi-ai dispatches only a named event: it parses the
 * six message and content-block names, throws on `error`, and skips the rest.
 */
const ANTHROPIC_STREAM_EVENTS: ReadonlySet<string> = new Set([
  'message_start', 'message_delta', 'message_stop',
  'content_block_start', 'content_block_delta', 'content_block_stop',
  'ping', 'error',
])

/**
 * A data-only event, or one named the SSE default `message`, takes its name from
 * a JSON-object `type` that is an Anthropic stream event. A proxy heartbeat that
 * splits `event:` from its `data:` leaves exactly such an event, which pi-ai
 * would otherwise skip, losing that delta. Other events keep their name.
 */
function anthropicEventName(event: string | undefined, data: string): string | undefined {
  if (event !== undefined && event !== 'message') return event
  let value: unknown
  try {
    value = JSON.parse(data)
  } catch (_notJson) {
    return event
  }
  const type = typeof value === 'object' && value !== null && !Array.isArray(value)
    ? (value as { type?: unknown }).type
    : undefined
  return typeof type === 'string' && ANTHROPIC_STREAM_EVENTS.has(type) ? type : event
}

/**
 * Anthropic Messages framing: drops empty events like the OpenAI framing, and
 * names a data-only Anthropic stream event from its JSON `type`. Data is never
 * invented or rewritten; non-JSON and unknown-type data-only events pass unnamed.
 */
export function normalizeAnthropicSse(response: Response): Response {
  return normalizeSse(response, anthropicEventName)
}

/** Request-local SDK fetch hook; never replaces the process-wide fetch function. */
export const fetchOpenAiSse: typeof globalThis.fetch = async (input, init) =>
  normalizeOpenAiSse(await globalThis.fetch(input, init))

/** Request-local Anthropic SDK fetch hook; never replaces the process-wide fetch function. */
export const fetchAnthropicSse: typeof globalThis.fetch = async (input, init) =>
  normalizeAnthropicSse(await globalThis.fetch(input, init))
