/** SSE framing for the OpenAI JSON protocols before their SDK parses event data. */
import { createParser, type EventSourceParser } from 'eventsource-parser'

/**
 * Ignore empty SSE events, including named events interrupted by proxy heartbeats.
 * Nonempty data and event names retain their SSE meaning. JSON validation and
 * protocol completion stay with pi-ai: an empty `response.completed` is not a
 * completion, and an unterminated event at EOF is not dispatched.
 *
 * Stream pipes propagate backpressure, read errors, and cancellation to the
 * original body. HTTP failures and non-SSE responses pass through unchanged.
 */
export function normalizeOpenAiSse(response: Response): Response {
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
          controller.enqueue(encoder.encode(
            `${event === undefined ? '' : `event: ${event}\n`}${data.split('\n').map(line => `data: ${line}`).join('\n')}\n\n`,
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

/** Request-local SDK fetch hook; never replaces the process-wide fetch function. */
export const fetchOpenAiSse: typeof globalThis.fetch = async (input, init) =>
  normalizeOpenAiSse(await globalThis.fetch(input, init))
