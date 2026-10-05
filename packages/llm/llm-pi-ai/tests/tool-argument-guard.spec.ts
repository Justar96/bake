import { afterEach, describe, expect, it, vi } from 'vitest'
import { Context } from '@deepseek-ai/cordis'
import LlmRuntime, { isRetryableFailureCode, LlmError, resolveRetryPolicy } from 'bake-llm'
import type { StreamChunk } from 'bake-llm'
import * as LlmPiAi from 'bake-llm-pi-ai'
import type { AssistantMessage, AssistantMessageEvent } from '@earendil-works/pi-ai'
import { Config, DEFAULT_MAX_TOOL_ARGUMENT_WHITESPACE, resolveProfiles } from '../src/config.ts'
import { toStreamChunks } from '../src/stream.ts'
import { assemble } from './assemble.ts'
import { closeMockServers, mockServer } from './mock-server.ts'

afterEach(async () => {
  vi.unstubAllEnvs()
  await closeMockServers()
})

function assistant(overrides: Partial<AssistantMessage> = {}): AssistantMessage {
  return {
    role: 'assistant',
    content: [{ type: 'toolCall', id: 'call-1', name: 'edit', arguments: {} }],
    api: 'openai-responses',
    provider: 'gw',
    model: 'm',
    usage: {
      input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0,
      cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
    },
    stopReason: 'toolUse',
    timestamp: 0,
    ...overrides,
  }
}

/** One tool call streamed as the given argument deltas, then completed. */
function toolCallEvents(deltas: readonly string[]): AssistantMessageEvent[] {
  const partial = assistant()
  const text = deltas.join('')
  return [
    { type: 'toolcall_start', contentIndex: 0, partial },
    ...deltas.map((delta): AssistantMessageEvent => ({ type: 'toolcall_delta', contentIndex: 0, delta, partial })),
    {
      type: 'toolcall_end',
      contentIndex: 0,
      toolCall: { type: 'toolCall', id: 'call-1', name: 'edit', arguments: JSON.parse(text) as Record<string, unknown> },
      partial,
    },
    { type: 'done', reason: 'toolUse', message: partial },
  ]
}

/** A scripted pi-ai stream that records whether its consumer stopped reading. */
function scripted(events: readonly AssistantMessageEvent[]): { stream: AsyncGenerator<AssistantMessageEvent>; closed: () => boolean } {
  let closed = false
  async function* stream(): AsyncGenerator<AssistantMessageEvent> {
    try {
      for (const event of events) yield event
    } finally {
      closed = true
    }
  }
  return { stream: stream(), closed: () => closed }
}

async function collect(stream: AsyncIterable<StreamChunk>): Promise<StreamChunk[]> {
  const out: StreamChunk[] = []
  for await (const chunk of stream) out.push(chunk)
  return out
}

describe('runaway tool-argument whitespace guard', () => {
  it('abandons a tool call whose arguments end in a whitespace run past the bound, with a retryable failure', async () => {
    const padding = Array.from({ length: 30 }, () => ' \n\t\r ')
    const partial = assistant()
    const events: AssistantMessageEvent[] = [
      { type: 'toolcall_start', contentIndex: 0, partial },
      { type: 'toolcall_delta', contentIndex: 0, delta: '{"file_path":"src/money.js","old_string":"a","edits":[]', partial },
      ...padding.map((delta): AssistantMessageEvent => ({ type: 'toolcall_delta', contentIndex: 0, delta, partial })),
    ]
    const source = scripted(events)
    const seen: StreamChunk[] = []
    const failure = await (async () => {
      try {
        for await (const chunk of toStreamChunks(source.stream, undefined, undefined, 'm', 100)) seen.push(chunk)
      } catch (error: unknown) {
        return error
      }
      return undefined
    })()

    expect(failure).toBeInstanceOf(LlmError)
    expect(failure).toMatchObject({ code: 'TRANSPORT' })
    expect((failure as Error).message).toMatch(/model "m" streamed 105 whitespace characters after the arguments of tool call "edit"/)
    // The bound is checked per delta: the 21st five-character delta crossed it and was not forwarded.
    expect(seen.filter(chunk => chunk.type === 'tool-call-delta')).toHaveLength(21)
    expect(seen.some(chunk => chunk.type === 'block-end' || chunk.type === 'finish')).toBe(false)
    expect(source.closed()).toBe(true)
    // A route's default retry policy retries the failure.
    expect(isRetryableFailureCode(resolveRetryPolicy(undefined, 'test'), (failure as LlmError).code)).toBe(true)
  })

  it('counts whitespace across delta boundaries and defaults to the documented bound', async () => {
    const run = ' '.repeat(DEFAULT_MAX_TOOL_ARGUMENT_WHITESPACE)
    await expect(collect(toStreamChunks(scripted(toolCallEvents(['{"a":1', run.slice(0, 700), run.slice(700), '}'])).stream)))
      .resolves.toHaveLength(7)
    await expect(collect(toStreamChunks(scripted([
      { type: 'toolcall_start', contentIndex: 0, partial: assistant() },
      { type: 'toolcall_delta', contentIndex: 0, delta: `{"a":1${run}`, partial: assistant() },
      { type: 'toolcall_delta', contentIndex: 0, delta: ' ', partial: assistant() },
    ]).stream))).rejects.toMatchObject({ code: 'TRANSPORT' })
  })

  it('ignores whitespace inside string values, including after escaped quotes and across deltas', async () => {
    const indentation = ' '.repeat(5000)
    const chunks = await collect(toStreamChunks(scripted(toolCallEvents([
      '{"new_string":"\\"',
      `${indentation}\\`,
      `"${indentation}\\\\`,
      `", "b":"${indentation}"}`,
    ])).stream, undefined, undefined, 'm', 100))
    expect(chunks.at(-1)).toMatchObject({ type: 'finish', reason: { kind: 'tool-calls' } })
    const end = chunks.find(chunk => chunk.type === 'block-end')
    expect(end).toMatchObject({ block: { type: 'tool-call', name: 'edit' } })
  })

  it('resets the run on any character outside a string', async () => {
    const run = ' '.repeat(90)
    const chunks = await collect(toStreamChunks(scripted(toolCallEvents([
      `{${run}"a"${run}:${run}[${run}1${run},${run}2${run}]${run}`,
      `,${run}"b":{${run}}${run}}`,
    ])).stream, undefined, undefined, 'm', 100))
    expect(chunks.at(-1)).toMatchObject({ type: 'finish', reason: { kind: 'tool-calls' } })
  })

  it('tracks each concurrent tool call separately', async () => {
    const first = assistant()
    const run = ' '.repeat(60)
    const chunks = await collect(toStreamChunks(scripted([
      { type: 'toolcall_start', contentIndex: 0, partial: first },
      { type: 'toolcall_start', contentIndex: 1, partial: first },
      { type: 'toolcall_delta', contentIndex: 0, delta: `{"a":1${run}`, partial: first },
      { type: 'toolcall_delta', contentIndex: 1, delta: `{"b":2${run}`, partial: first },
      { type: 'toolcall_delta', contentIndex: 0, delta: '}', partial: first },
      { type: 'toolcall_delta', contentIndex: 1, delta: '}', partial: first },
      { type: 'done', reason: 'toolUse', message: first },
    ]).stream, undefined, undefined, 'm', 100))
    expect(chunks.at(-1)).toMatchObject({ type: 'finish', reason: { kind: 'tool-calls' } })
  })

  it('validates the bound at the provider-profile boundary', () => {
    expect(resolveProfiles({ gw: { api: 'openai-responses', baseURL: 'http://127.0.0.1:1', models: [{ id: 'm' }] } })
      .get('gw')?.maxToolArgumentWhitespace).toBe(DEFAULT_MAX_TOOL_ARGUMENT_WHITESPACE)
    for (const value of [0, -1, 1.5]) {
      expect(() => resolveProfiles({ gw: { api: 'openai-responses', baseURL: 'http://127.0.0.1:1', models: [{ id: 'm' }], maxToolArgumentWhitespace: value } }))
        .toThrow(/maxToolArgumentWhitespace must be a positive safe integer/)
      expect(() => Config({ providers: { gw: { maxToolArgumentWhitespace: value } } })).toThrow()
    }
  })

  it('aborts the upstream Responses request and surfaces a transient failure', async () => {
    const whitespace = JSON.stringify({ type: 'response.function_call_arguments.delta', output_index: 0, item_id: 'fc_1', delta: ' \n'.repeat(10) })
    const server = await mockServer([{
      events: [
        JSON.stringify({ type: 'response.created', response: { id: 'resp_1', status: 'in_progress', output: [] } }),
        JSON.stringify({
          type: 'response.output_item.added', output_index: 0,
          item: { type: 'function_call', id: 'fc_1', call_id: 'call_1', name: 'edit', arguments: '' },
        }),
        JSON.stringify({
          type: 'response.function_call_arguments.delta', output_index: 0, item_id: 'fc_1',
          delta: '{"file_path":"src/money.js","replace_all":false,"edits":[]',
        }),
        ...Array.from({ length: 50 }, () => whitespace),
      ],
      delayMs: 1,
      holdOpen: true,
    }])
    vi.stubEnv('PI_TEST_KEY', 'test-key')
    const ctx = new Context()
    await ctx.plugin(LlmRuntime)
    await ctx.plugin(LlmPiAi, {
      providers: {
        gw: {
          apiKeyEnv: 'PI_TEST_KEY', api: 'openai-responses', baseURL: server.url,
          models: [{ id: 'm' }], maxToolArgumentWhitespace: 100,
        },
      },
    })

    const result = await assemble(ctx, { provider: 'gw', model: 'm', messages: [] })
    expect(result.finish).toMatchObject({ kind: 'error', failure: { code: 'TRANSPORT' } })
    await Promise.race([
      server.responseClosed,
      new Promise<never>((_resolve, reject) => {
        setTimeout(() => { reject(new Error('upstream request stayed open after the guard fired')) }, 2_000)
      }),
    ])
    expect(server.closedResponses).toBe(1)
    expect(server.paths).toEqual(['/responses'])
  })
})
