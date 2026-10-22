/**
 * The loopback model both arms stream from: an OpenAI Responses endpoint on
 * 127.0.0.1 that answers every request with the same 100 text deltas, one
 * every `paceMs`. The answer's first delta starts with `PERF_STREAM_START`
 * and its last ends with `PERF_STREAM_DONE`, as the 0.3 driver's replay
 * reply does. The event shapes follow `scripts/rust-preview-pty.ts`'s fake
 * model.
 */

export const STREAM_START = 'PERF_STREAM_START'
export const STREAM_DONE = 'PERF_STREAM_DONE'
export const DELTA_COUNT = 100
export const DEFAULT_PACE_MS = 10

/** The deltas of the fixed answer, in order. */
export function answerDeltas(): string[] {
  return Array.from({ length: DELTA_COUNT }, (_, index) => index === 0 ? `${STREAM_START}\n` : index === DELTA_COUNT - 1 ? `\n${STREAM_DONE}` : `Synthetic delta ${index}. `)
}

function event(sequence: number, value: Record<string, unknown>): string {
  return `event: ${String(value.type)}\ndata: ${JSON.stringify({ ...value, sequence_number: sequence })}\n\n`
}

const USAGE = {
  input_tokens: 40, input_tokens_details: { cached_tokens: 0 },
  output_tokens: 100, output_tokens_details: { reasoning_tokens: 0 }, total_tokens: 140,
}

/**
 * The Responses events of one answer: the head that opens the message, one
 * event per delta, and the tail that completes it.
 * @param n - the request number, which names the response and item.
 */
export function answerEvents(n: number, model: string): { head: string[]; deltas: string[]; tail: string[] } {
  const id = `msg_${n}`
  const deltas = answerDeltas()
  const answer = deltas.join('')
  const response = { id: `resp_${n}`, object: 'response', created_at: 1_760_000_000, model, output: [] as unknown[] }
  const item = { id, type: 'message', status: 'completed', role: 'assistant', content: [{ type: 'output_text', annotations: [], text: answer }] }
  let sequence = 0
  const head = [
    { type: 'response.created', response: { ...response, status: 'in_progress' } },
    { type: 'response.output_item.added', output_index: 0, item: { id, type: 'message', status: 'in_progress', role: 'assistant', content: [] } },
    { type: 'response.content_part.added', item_id: id, output_index: 0, content_index: 0, part: { type: 'output_text', annotations: [], text: '' } },
  ].map(value => event(sequence++, value))
  const deltaEvents = deltas.map(delta => event(sequence++, { type: 'response.output_text.delta', item_id: id, output_index: 0, content_index: 0, delta }))
  const tail = [
    { type: 'response.output_text.done', item_id: id, output_index: 0, content_index: 0, text: answer },
    { type: 'response.content_part.done', item_id: id, output_index: 0, content_index: 0, part: { type: 'output_text', annotations: [], text: answer } },
    { type: 'response.output_item.done', output_index: 0, item },
    { type: 'response.completed', response: { ...response, status: 'completed', output: [item], usage: USAGE } },
  ].map(value => event(sequence++, value))
  return { head, deltas: deltaEvents, tail }
}

/** One loopback server; `stop` closes it and every open stream. */
export class PacedModel {
  private readonly server: ReturnType<typeof Bun.serve>
  /** Model requests answered, in order of arrival. */
  requests = 0
  /** Requests on any path but `POST /v1/responses`. */
  readonly unexpected: string[] = []

  constructor(private readonly paceMs = DEFAULT_PACE_MS, private readonly model = 'synthetic') {
    this.server = Bun.serve({ port: 0, hostname: '127.0.0.1', idleTimeout: 0, fetch: request => this.answer(request) })
  }

  get baseUrl(): string { return `http://127.0.0.1:${this.server.port}/v1` }

  async stop(): Promise<void> { await this.server.stop(true) }

  private async answer(request: Request): Promise<Response> {
    const url = new URL(request.url)
    if (request.method !== 'POST' || url.pathname !== '/v1/responses') {
      this.unexpected.push(`${request.method} ${url.pathname}`)
      return new Response(JSON.stringify({ error: { message: 'not served by the perf model', type: 'invalid_request_error' } }), { status: 404, headers: { 'content-type': 'application/json' } })
    }
    // Drain the body, whose size grows with history, before streaming.
    await request.arrayBuffer()
    const { head, deltas, tail } = answerEvents(++this.requests, this.model)
    const encoder = new TextEncoder()
    const pace = this.paceMs
    return new Response(new ReadableStream({
      start: async (controller) => {
        try {
          controller.enqueue(encoder.encode(head.join('')))
          for (const delta of deltas) {
            if (request.signal.aborted) break
            controller.enqueue(encoder.encode(delta))
            await Bun.sleep(pace)
          }
          if (!request.signal.aborted) controller.enqueue(encoder.encode(tail.join('')))
          controller.close()
        } catch {
          // The client went away mid-stream.
        }
      },
    }), { headers: { 'content-type': 'text/event-stream', 'cache-control': 'no-cache' } })
  }
}
