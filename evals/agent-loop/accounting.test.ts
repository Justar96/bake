/** Provider counters remain observable even when the stream never reaches final usage. */
import { describe, expect, test } from 'bun:test'
import { normalizeWire, reconcile } from './accounting.ts'

const start = { event: 'message_start', input_tokens: 2, cache_creation_input_tokens: 6175, cache_read_input_tokens: 0, output_tokens: 5 }
const partial = { inputTokens: 2, outputTokens: 5, cacheReadTokens: 0, cacheWriteTokens: 6175, totalTokens: 6182 }
const final = { ...partial, outputTokens: 17, totalTokens: 6194 }
const delta = { event: 'message_delta', output_tokens: 17 }

describe('Anthropic cumulative usage', () => {
  test('an interrupted message_start is partial even when the agent reports identical counters', () => {
    const result = reconcile('anthropic-messages', [{ status: 200, usage: [start], stream: { lastEvent: 'content_block_delta' } }], partial)
    expect(normalizeWire('anthropic-messages', [start])).toMatchObject(partial)
    expect(result).toMatchObject({ complete: false, matches: false, totals: partial })
  })

  test('the last cumulative delta replaces initial output without counting it twice', () => {
    const result = reconcile('anthropic-messages', [{ status: 200, usage: [start, { event: 'message_delta', output_tokens: 9 }, delta], stream: { lastEvent: 'message_stop' } }], final)
    expect(result).toMatchObject({ complete: true, matches: true, totals: final })
  })

  test('a delta without the terminal message_stop remains partial', () => {
    for (const stream of [undefined, { lastEvent: 'message_delta' }, { lastEvent: 'error' }]) {
      const result = reconcile('anthropic-messages', [{ status: 200, usage: [start, delta], stream }], final)
      expect(result).toMatchObject({ complete: false, matches: false, totals: final })
    }
  })

  test('message_stop cannot make missing final output counters complete', () => {
    for (const usage of [[start], [start, { event: 'message_delta' }], [start, { event: 'message_delta', output_tokens: -1 }]]) {
      expect(reconcile('anthropic-messages', [{ status: 200, usage, stream: { lastEvent: 'message_stop' } }], partial).complete).toBe(false)
    }
  })

  test('one partial summary prevents complete accounting for the whole invocation', () => {
    const totals = { inputTokens: 4, outputTokens: 22, cacheReadTokens: 0, cacheWriteTokens: 12350, totalTokens: 12376 }
    const result = reconcile('anthropic-messages', [
      { status: 200, usage: [start, delta], stream: { lastEvent: 'message_stop' } },
      { status: 200, usage: [start], stream: { lastEvent: 'content_block_delta' } },
    ], totals)
    expect(result).toMatchObject({ complete: false, matches: false, totals })
  })
})

describe('other provider accounting', () => {
  test('Chat Completions final usage does not depend on transport EOF', () => {
    const totals = { inputTokens: 12, outputTokens: 3, cacheReadTokens: 8, cacheWriteTokens: 0, totalTokens: 23 }
    const result = reconcile('openai-completions', [{ status: 200, usage: [{ event: 'usage', prompt_tokens: 20, completion_tokens: 3, total_tokens: 23, prompt_tokens_details: { cached_tokens: 8 } }], stream: { lastEvent: null } }], totals)
    expect(result).toMatchObject({ complete: true, matches: true, totals })
  })

  test('Responses final usage retains additional reported reasoning', () => {
    const totals = { inputTokens: 12, outputTokens: 3, cacheReadTokens: 8, cacheWriteTokens: 0, totalTokens: 25 }
    const result = reconcile('openai-responses', [{ status: 200, usage: [{ event: 'response.completed', input_tokens: 20, output_tokens: 3, total_tokens: 25, input_tokens_details: { cached_tokens: 8 }, output_tokens_details: { reasoning_tokens: 2 } }] }], totals)
    expect(result).toMatchObject({ complete: true, matches: true, totals, reasoningTokens: 2, additionalReasoningTokens: 2 })
  })

  test('missing counters, transport errors, and mismatched totals remain distinct failures', () => {
    expect(reconcile('openai-responses', [{ status: 200, usage: [] }], {}).complete).toBe(false)
    expect(reconcile('anthropic-messages', [{ status: 502, usage: [start, delta], stream: { lastEvent: 'message_stop' } }], final).complete).toBe(false)
    expect(reconcile('anthropic-messages', [{ status: 200, usage: [start, delta], stream: { lastEvent: 'message_stop' } }], partial)).toMatchObject({ complete: true, matches: false, totals: final })
  })
})
