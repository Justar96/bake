import { describe, expect, it } from 'vitest'
import { Context } from '@deepseek-ai/cordis'
import LlmRuntime, { createUserMessage, LlmAdapter, MessageId, projectToolUpdates } from '../src/index.ts'
import type { GenerateOptions, LlmResolvedModelInfo, StreamChunk, ToolHistory, ToolSchema, ToolUpdate } from '../src/index.ts'

const tool = (name: string): ToolSchema => ({ name, description: name, parameters: {} })
const first = createUserMessage({ source: { kind: 'user' }, content: [{ type: 'text', text: 'first' }] })
const second = createUserMessage({ source: { kind: 'user' }, content: [{ type: 'text', text: 'second' }] })
const third = createUserMessage({ source: { kind: 'user' }, content: [{ type: 'text', text: 'third' }] })
const history: ToolHistory = {
  tools: [tool('search')],
  updates: [
    { afterMessageId: first.id, additions: [tool('fetch')], removals: [] },
    { afterMessageId: second.id, additions: [], removals: ['search', 'fetch'] },
    { afterMessageId: third.id, additions: [tool('fetch')], removals: [] },
  ],
}

describe('route tool projection', () => {
  it('binds update capability to the prepared adapter generation and strips forged notices on unsupported routes', async () => {
    class Adapter extends LlmAdapter {
      mode: string | undefined = 'in-history'
      requests: GenerateOptions[] = []
      override async resolveModel(provider: string, id: string): Promise<LlmResolvedModelInfo> {
        return { provider, id, name: id, ...this.mode === undefined ? {} : { toolUpdate: this.mode as ToolUpdate } }
      }
      override async *stream(options: GenerateOptions): AsyncIterable<StreamChunk> {
        this.requests.push(options)
        yield { type: 'finish', reason: { kind: 'stop' } }
      }
    }
    const ctx = new Context()
    try {
      await ctx.plugin(LlmRuntime)
      const adapter = new Adapter()
      ctx.llm.registerAdapter(['test'], adapter)
      const prepared = await ctx.llm.prepareCall({ provider: 'test', model: 'test' })
      expect(prepared.toolUpdate).toBe('in-history')
      adapter.mode = undefined
      const request: GenerateOptions = { provider: 'test', model: 'test', messages: [first, second, third], tools: [tool('fetch')], toolHistory: history }
      for await (const _chunk of prepared.stream(request)) { /* Drain the prepared call. */ }
      expect(adapter.requests[0]?.toolUpdates).toHaveLength(3)
      for await (const _chunk of ctx.llm.stream({ ...request, toolUpdates: adapter.requests[0]?.toolUpdates ?? [] })) {
        /* Drain the unsupported route. */
      }
      expect(adapter.requests[1]?.toolUpdates).toBeUndefined()
      expect(adapter.requests[1]?.tools).toEqual([tool('fetch')])
      adapter.mode = 'invalid'
      await expect(ctx.llm.prepareCall({ provider: 'test', model: 'test' })).rejects.toMatchObject({ code: 'INVALID_MODEL_INFO' })
    } finally {
      await ctx.fiber.dispose()
    }
  })
  it('retains declarations and re-offers unchanged definitions through ordered updates', () => {
    const projected = projectToolUpdates([first, second, third], [tool('fetch')], 'in-history', history)
    expect(projected.tools).toEqual([tool('search'), { ...tool('fetch'), deferLoading: true }])
    expect(projected.toolUpdates).toEqual([
      { afterMessageId: first.id, additions: ['fetch'], removals: [] },
      { afterMessageId: second.id, additions: [], removals: ['search', 'fetch'] },
      { afterMessageId: third.id, additions: ['fetch'], removals: [] },
    ])
    expect(history.tools).toEqual([tool('search')])
  })

  it('omits removals and declarations for inactive tools on addition-only routes', () => {
    expect(projectToolUpdates([first, second, third], [tool('fetch')], 'addition-only', history)).toEqual({
      tools: [{ ...tool('fetch'), deferLoading: true }],
      toolUpdates: [{ afterMessageId: first.id, additions: ['fetch'], removals: [] }],
    })
  })

  it('uses complete current declarations for unsupported routes and incomplete auxiliary prefixes', () => {
    const tools = [tool('fetch')]
    const fallback = { tools, toolUpdates: undefined }
    expect(projectToolUpdates([first, second, third], tools, undefined, history)).toEqual(fallback)
    expect(projectToolUpdates([first, second], tools, 'in-history', history)).toEqual(fallback)
    expect(projectToolUpdates([first, second, third], tools, 'in-history')).toEqual(fallback)
    expect(projectToolUpdates([third, second, first], tools, 'in-history', history)).toEqual(fallback)
    expect(projectToolUpdates([first], tools, 'in-history', {
      ...history, updates: [{ afterMessageId: MessageId('missing'), additions: tools, removals: [] }],
    })).toEqual(fallback)
    expect(projectToolUpdates([first, second, third], [tool('unknown')], 'in-history', history)).toEqual({ tools: [tool('unknown')], toolUpdates: undefined })
  })

  it('strips deferred flags from unsupported requests and rejects a changed historical definition', () => {
    const deferred = { ...tool('fetch'), deferLoading: true as const }
    expect(projectToolUpdates([], [deferred], undefined)).toEqual({ tools: [tool('fetch')], toolUpdates: undefined })
    const changed = { ...tool('search'), description: 'different' }
    expect(projectToolUpdates([first], [changed], 'in-history', {
      tools: [tool('search')], updates: [{ afterMessageId: first.id, additions: [changed], removals: [] }],
    })).toEqual({ tools: [changed], toolUpdates: undefined })
    expect(projectToolUpdates([], undefined, undefined)).toEqual({ tools: undefined, toolUpdates: undefined })
  })
})
