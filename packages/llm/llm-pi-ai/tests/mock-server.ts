import { createServer } from 'node:http'
import type { IncomingMessage, Server, ServerResponse } from 'node:http'

export interface MockServer {
  url: string
  paths: string[]
  requests: unknown[]
  headers: IncomingMessage['headers'][]
  readonly closedResponses: number
  responseClosed: Promise<void>
}

const servers: Server[] = []

/** Close every server opened since the last call; run from each spec's afterEach. */
export async function closeMockServers(): Promise<void> {
  await Promise.all(servers.splice(0).map(server => new Promise<void>((resolve) => {
    server.close(() => resolve())
    server.closeAllConnections()
  })))
}

/** A minimal complete text generation in pi-ai's chat-completions shape. */
export const textEvents = [
  '{"choices":[{"delta":{"role":"assistant","content":""},"index":0,"finish_reason":null}]}',
  '{"choices":[{"delta":{"content":"hello"},"index":0,"finish_reason":null}]}',
  '{"choices":[{"delta":{},"index":0,"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":1}}',
  '[DONE]',
]

/** A minimal complete text generation in Anthropic Messages' named-event SSE shape, for a `wire` behavior. */
export const anthropicTextWire = [
  ['message_start', {
    type: 'message_start',
    message: {
      id: 'msg_1', type: 'message', role: 'assistant', model: 'claude-large', content: [],
      stop_reason: null, stop_sequence: null, usage: { input_tokens: 3, output_tokens: 0 },
    },
  }],
  ['content_block_start', { type: 'content_block_start', index: 0, content_block: { type: 'text', text: '' } }],
  ['content_block_delta', { type: 'content_block_delta', index: 0, delta: { type: 'text_delta', text: 'hello' } }],
  ['content_block_stop', { type: 'content_block_stop', index: 0 }],
  ['message_delta', { type: 'message_delta', delta: { stop_reason: 'end_turn', stop_sequence: null }, usage: { output_tokens: 1 } }],
  ['message_stop', { type: 'message_stop' }],
].map(([event, data]) => `event: ${event as string}\ndata: ${JSON.stringify(data)}\n\n`)

/** Local provider stand-in: replays scripted behaviors per request. */
export async function mockServer(script: {
  status?: number
  events?: string[]
  wire?: readonly string[]
  holdOpen?: boolean
  body?: string
  delayMs?: number
  headers?: Record<string, string>
}[]): Promise<MockServer> {
  const paths: string[] = []
  const requests: unknown[] = []
  const headers: IncomingMessage['headers'][] = []
  let closedResponses = 0
  const responseClosed = Promise.withResolvers<undefined>()
  const server = createServer((request: IncomingMessage, response: ServerResponse) => {
    response.on('close', () => {
      closedResponses += 1
      responseClosed.resolve(undefined)
    })
    let body = ''
    request.on('data', (chunk: Buffer) => { body += chunk.toString('utf8') })
    request.on('end', () => {
      paths.push(request.url ?? '')
      requests.push(body.length === 0 ? undefined : JSON.parse(body))
      headers.push(request.headers)
      const behavior = script.shift() ?? { status: 500, body: 'script exhausted' }
      if (behavior.status !== undefined && behavior.status !== 200) {
        response.writeHead(behavior.status, { 'content-type': 'application/json', ...behavior.headers })
        response.end(behavior.body ?? '{}')
        return
      }
      if (behavior.body !== undefined) {
        response.writeHead(200, { 'content-type': 'application/json', ...behavior.headers })
        response.end(behavior.body)
        return
      }
      response.writeHead(200, { 'content-type': 'text/event-stream', ...behavior.headers })
      let timer: ReturnType<typeof setTimeout> | undefined
      response.on('close', () => { clearTimeout(timer) })
      let index = 0
      const writeNext = (): void => {
        if (response.destroyed) return
        const event = behavior.wire?.[index] ?? behavior.events?.[index]
        index++
        if (event === undefined) { if (!behavior.holdOpen) response.end(); return }
        response.write(behavior.wire === undefined ? `data: ${event}\n\n` : event)
        if (behavior.delayMs === undefined) writeNext()
        else timer = setTimeout(writeNext, behavior.delayMs)
      }
      writeNext()
    })
  })
  servers.push(server)
  await new Promise<void>(resolve => server.listen(0, '127.0.0.1', resolve))
  const address = server.address()
  if (address === null || typeof address === 'string') throw new Error('no port')
  return {
    url: `http://127.0.0.1:${address.port}`,
    paths,
    requests,
    headers,
    responseClosed: responseClosed.promise,
    get closedResponses() { return closedResponses },
  }
}
