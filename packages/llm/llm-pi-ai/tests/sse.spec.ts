/** Proxy framing through the installed parser, SDK, and Harness adapter. */
import { afterEach, describe, expect, it } from 'vitest'
import { createUserMessage, type StreamChunk } from '@deepseek-ai/dsh-llm'
import { normalizeOpenAiSse } from '../src/sse.ts'
import { PiAiAdapter } from '../src/adapter.ts'
import { resolveProfiles } from '../src/config.ts'
import { memoryAuth } from './auth-double.ts'
import { closeMockServers, mockServer, textEvents } from './mock-server.ts'

afterEach(closeMockServers)
const emptyEvent = 'event: response.completed\n: keep-alive\n\n'
const data = (value: unknown) => `data: ${JSON.stringify(value)}\n\n`
const item = { id: 'msg_test', type: 'message', role: 'assistant', content: [{ type: 'output_text', text: 'hello' }] }
const responseEvents = [
  { type: 'response.created', response: { id: 'resp_test', status: 'in_progress' } },
  { type: 'response.output_item.added', output_index: 0, item: { ...item, content: [] } },
  { type: 'response.output_text.delta', output_index: 0, content_index: 0, delta: 'hello' },
  { type: 'response.output_item.done', output_index: 0, item },
  { type: 'response.completed', response: { id: 'resp_test', status: 'completed', output: [item],
    usage: { input_tokens: 3, output_tokens: 1, total_tokens: 4 } } },
]
const responseWire = responseEvents.map(data)
const request = { provider: 'fixture', model: 'model', messages: [createUserMessage({
  content: [{ type: 'text', text: 'hello' }], source: { kind: 'user' },
})] }
function adapter(url: string, api = 'openai-responses', streamIdleTimeoutMs = 5000) {
  const profiles = resolveProfiles({ fixture: { api, baseURL: url, streamIdleTimeoutMs,
    models: [{ id: 'model', contextWindow: 8192, maxTokens: 128 }] } })
  return new PiAiAdapter({ profiles: () => profiles, resolveApiKey: async () => 'fixture-key', auth: memoryAuth() })
}
async function collect(stream: AsyncIterable<StreamChunk>) {
  const chunks: StreamChunk[] = []
  for await (const chunk of stream) chunks.push(chunk)
  return chunks
}
const finish = (chunks: readonly StreamChunk[]) => chunks.findLast(chunk => chunk.type === 'finish')

function response(chunks: readonly Uint8Array[]) {
  let index = 0
  return new Response(new ReadableStream({ pull(controller) {
    if (index < chunks.length) controller.enqueue(chunks[index++]!)
    else controller.close()
  } }), { headers: { 'content-type': 'Text/Event-Stream; charset=utf-8', 'x-request-id': 'kept', 'content-length': '999' } })
}

describe('SSE framing', () => {
  it.each(['\n', '\r\n', '\r'])('handles byte splits, UTF-8, BOM, multiline data, and %j endings', async (ending) => {
    const wire = '\uFEFF' + (emptyEvent + 'id: 1\nretry: 1000\n: heartbeat\n\n'
      + 'event: response.created\ndata: {"text":\ndata: "你好🌱"}\n\n'
      + 'event: empty\ndata:\n\n' + 'data: [DONE]\n\n').replaceAll('\n', ending)
    const bytes = new TextEncoder().encode(wire)
    const result = normalizeOpenAiSse(response(Array.from(bytes, byte => Uint8Array.of(byte))))
    expect(await result.text()).toBe('event: response.created\ndata: {"text":\ndata: "你好🌱"}\n\ndata: [DONE]\n\n')
    expect(result.headers.get('x-request-id')).toBe('kept')
    expect(result.headers.has('content-length')).toBe(false)
  })

  it('does not repair nonempty invalid JSON or dispatch an unterminated tail', async () => {
    const result = normalizeOpenAiSse(response([new TextEncoder().encode('data: {broken}\n\ndata: {"type":"response.completed"}\n')]))
    expect(await result.text()).toBe('data: {broken}\n\n')
  })

  it.each(['\n', '\r\n', '\r'])('requires a blank line before EOF with %j endings', async (ending) => {
    const result = normalizeOpenAiSse(response([new TextEncoder().encode(`data: {"value":1}${ending}`)]))
    expect(await result.text()).toBe('')
  })

  it.each([
    new Response('denied', { status: 401, headers: { 'content-type': 'text/event-stream' } }),
    Response.json({ message: 'not an SSE response' }),
    new Response(null, { status: 204 }),
  ])('preserves HTTP failures and non-SSE response objects', (original) => {
    expect(normalizeOpenAiSse(original)).toBe(original)
  })

  it('propagates cancellation through all pipes to the original reader', async () => {
    const cancelled = Promise.withResolvers<unknown>()
    const source = new Response(new ReadableStream({
      start(controller) { controller.enqueue(new TextEncoder().encode('data: {"hello":1}\n\n')) },
      cancel(reason) { cancelled.resolve(reason) },
    }), { headers: { 'content-type': 'text/event-stream' } })
    const reader = normalizeOpenAiSse(source).body!.getReader()
    await reader.read()
    await reader.cancel('consumer stopped')
    await expect(cancelled.promise).resolves.toBe('consumer stopped')
  })

  it('propagates an upstream stream failure', async () => {
    const source = new Response(new ReadableStream({ pull(controller) { controller.error(new Error('socket lost')) } }),
      { headers: { 'content-type': 'text/event-stream' } })
    await expect(normalizeOpenAiSse(source).text()).rejects.toThrow('socket lost')
  })
})

describe('OpenAI SDK protocol outcomes', () => {
  it.each(['openai-responses', 'openai-completions'])('ignores empty frames without losing %s output or usage', async (api) => {
    const wire = api === 'openai-responses' ? responseWire : textEvents.map(event => `data: ${event}\n\n`)
    const server = await mockServer([{ wire: [emptyEvent, ...wire.flatMap(event => [event, emptyEvent])] }])
    const chunks = await collect(adapter(server.url, api).stream(request))
    expect(finish(chunks)).toMatchObject({ type: 'finish', reason: { kind: 'stop' } })
    expect(chunks).toContainEqual({ type: 'text-delta', index: 0, text: 'hello' })
    expect(chunks).toContainEqual({ type: 'usage', usage: { inputTokens: 3, outputTokens: 1, totalTokens: 4 } })
    expect(server.requests).toHaveLength(1)
  })

  it.each([
    [emptyEvent],
    [...responseWire.slice(0, -1), emptyEvent],
    [...responseWire.slice(0, -1), 'data: [DONE]\n\n'],
    [...responseWire.slice(0, -1), responseWire.at(-1)!.trimEnd()],
  ].map(wire => ({ wire })))('requires an actual terminal event despite EOF or [DONE]', async ({ wire }) => {
    const server = await mockServer([{ wire }])
    expect(finish(await collect(adapter(server.url).stream(request)))).toMatchObject({
      type: 'finish', reason: { kind: 'error', failure: { code: 'TRANSPORT' } },
    })
  })

  it('keeps malformed nonempty JSON a failure instead of silently completing', async () => {
    const server = await mockServer([{ wire: [...responseWire.slice(0, -1), 'data: {broken}\n\n'] }])
    const chunks = await collect(adapter(server.url).stream(request))
    expect(finish(chunks)).toMatchObject({ type: 'finish', reason: { kind: 'error', failure: { code: 'PI_AI_ERROR' } } })
  })

  it.each([
    [{ type: 'response.failed', response: { status: 'failed', error: { code: 'server_error', message: 'provider failed' } } }, 'error'],
    [{ type: 'response.incomplete', response: { status: 'incomplete', incomplete_details: { reason: 'max_output_tokens' }, output: [item] } }, 'max-tokens'],
  ] as const)('retains provider failure and token-limit terminal semantics', async (terminal, kind) => {
    const server = await mockServer([{ wire: [...responseWire.slice(0, -1), emptyEvent, data(terminal)] }])
    expect(finish(await collect(adapter(server.url).stream(request)))).toMatchObject({ type: 'finish', reason: { kind } })
  })

  it('does not let heartbeats turn a stalled generation into an active one', async () => {
    const server = await mockServer([{ wire: Array.from({ length: 100 }, () => emptyEvent), delayMs: 5, holdOpen: true }])
    await expect(collect(adapter(server.url, 'openai-responses', 40).stream(request))).rejects.toMatchObject({ code: 'TIMEOUT' })
    await server.responseClosed
  })

  it('preserves caller cancellation after partial output and closes the socket', async () => {
    const server = await mockServer([{ wire: responseWire.slice(0, 3), holdOpen: true }])
    const abort = new AbortController()
    const stream = adapter(server.url).stream({ ...request, signal: abort.signal })
    const chunks: StreamChunk[] = []
    for await (const chunk of stream) {
      chunks.push(chunk)
      if (chunk.type === 'text-delta') abort.abort()
    }
    expect(finish(chunks)).toMatchObject({ type: 'finish', reason: { kind: 'aborted' } })
    await server.responseClosed
  })

  it('closes the socket when the consumer leaves mid-response', async () => {
    const server = await mockServer([{ wire: responseWire.slice(0, 3), holdOpen: true }])
    for await (const chunk of adapter(server.url).stream(request)) {
      if (chunk.type === 'text-delta') break
    }
    await server.responseClosed
    expect(server.closedResponses).toBe(1)
  })
})
