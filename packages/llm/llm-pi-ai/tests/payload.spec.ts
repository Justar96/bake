/**
 * The route-configured Messages body rewrites, one switch at a time and
 * composed with the adaptive-thinking respelling.
 */

import { describe, expect, it } from 'vitest'
import type { PiAiMessagesWire } from '../src/config.ts'
import { DEFERRED_TOOL_PLACEHOLDER, messagesPayloadHook } from '../src/payload.ts'

const cached = { type: 'ephemeral' } as const

const placeholder = {
  name: DEFERRED_TOOL_PLACEHOLDER,
  description: 'Reserved placeholder. Never available. Never call this.',
  input_schema: { type: 'object', properties: {}, required: [] },
  defer_loading: true,
}

/** A body shaped like the one pi-ai builds for an in-history DeepSeek request. */
function piBody(): Record<string, unknown> {
  return {
    model: 'deepseek-flash',
    thinking: { type: 'adaptive', display: 'summarized' },
    output_config: { effort: 'high' },
    system: [{ type: 'text', text: 'prompt', cache_control: cached }],
    tools: [
      { name: 'read', input_schema: { type: 'object', properties: { cache_control: { type: 'string' } } } },
      { name: 'write', input_schema: { type: 'object', properties: {} }, cache_control: cached },
      placeholder,
      { name: 'search', input_schema: { type: 'object', properties: {} }, defer_loading: true },
    ],
    messages: [
      { role: 'user', content: 'look' },
      { role: 'user', content: [{ type: 'text', text: 'context' }] },
      { role: 'assistant', content: [{ type: 'tool_use', id: 'c1', name: 'read', input: {} }] },
      {
        role: 'user',
        content: [{ type: 'tool_result', tool_use_id: 'c1', content: [{ type: 'text', text: 'out', cache_control: cached }] }],
      },
      { role: 'user', content: [{ type: 'text', text: 'more', cache_control: cached }] },
    ],
  }
}

function rewrite(wire: PiAiMessagesWire, body: Record<string, unknown> = piBody(), respell = false): Record<string, unknown> | undefined {
  const hook = messagesPayloadHook({ messagesWire: wire, ...respell ? { adaptiveThinkingType: 'enabled' as const } : {} })
  return hook?.(body) as Record<string, unknown> | undefined
}

describe('messagesWire payload rewrites', () => {
  it('builds no hook when no rewrite is enabled', () => {
    expect(messagesPayloadHook({})).toBeUndefined()
    expect(messagesPayloadHook({ adaptiveThinkingType: 'adaptive', messagesWire: { stripCacheControl: false } })).toBeUndefined()
  })

  it('drops the deferred placeholder and keeps every other tool', () => {
    const body = rewrite({ dropDeferredToolPlaceholder: true })
    expect((body?.['tools'] as { name: string }[]).map(tool => tool.name)).toEqual(['read', 'write', 'search'])
    // The other switches are off: breakpoints and message boundaries stay.
    expect((body?.['messages'] as unknown[]).length).toBe(5)
    expect(body?.['system']).toEqual([{ type: 'text', text: 'prompt', cache_control: cached }])
  })

  it('keeps the placeholder when every remaining tool would be deferred', () => {
    const body = piBody()
    body['tools'] = [placeholder, { name: 'search', input_schema: { type: 'object' }, defer_loading: true }]
    expect(rewrite({ dropDeferredToolPlaceholder: true }, body)).toBeUndefined()
    body['tools'] = [placeholder]
    expect(rewrite({ dropDeferredToolPlaceholder: true }, body)).toBeUndefined()
  })

  it('strips every cache_control breakpoint, nested tool-result blocks included, and nothing else', () => {
    const original = piBody()
    const body = rewrite({ stripCacheControl: true }, original)
    expect(JSON.stringify(body?.['system'])).not.toContain('cache_control')
    expect(JSON.stringify(body?.['messages'])).not.toContain('cache_control')
    const tools = body?.['tools'] as Record<string, unknown>[]
    expect(tools.map(tool => 'cache_control' in tool)).toEqual([false, false, false, false])
    // A tool parameter that happens to carry the name is schema, not a breakpoint.
    expect(tools[0]).toEqual((original['tools'] as unknown[])[0])
    // pi-ai's own body is left as built.
    expect(JSON.stringify(original)).toContain('cache_control":{"type":"ephemeral"')
  })

  it('merges adjacent same-role messages, normalizing string content to a text block', () => {
    const body = rewrite({ mergeAdjacentRoles: true })
    expect(body?.['messages']).toEqual([
      { role: 'user', content: [{ type: 'text', text: 'look' }, { type: 'text', text: 'context' }] },
      { role: 'assistant', content: [{ type: 'tool_use', id: 'c1', name: 'read', input: {} }] },
      {
        role: 'user',
        content: [
          { type: 'tool_result', tool_use_id: 'c1', content: [{ type: 'text', text: 'out', cache_control: cached }] },
          { type: 'text', text: 'more', cache_control: cached },
        ],
      },
    ])
  })

  it('composes every switch with the adaptive-thinking respelling', () => {
    const body = rewrite({ dropDeferredToolPlaceholder: true, stripCacheControl: true, mergeAdjacentRoles: true }, piBody(), true)
    expect(body?.['thinking']).toEqual({ type: 'enabled' })
    expect(body?.['output_config']).toEqual({ effort: 'high' })
    expect(JSON.stringify(body)).not.toContain('"cache_control":{"type":"ephemeral"')
    expect((body?.['tools'] as { name: string }[]).map(tool => tool.name)).toEqual(['read', 'write', 'search'])
    expect((body?.['messages'] as { role: string }[]).map(message => message.role)).toEqual(['user', 'assistant', 'user'])
  })

  it('sends the body unchanged when nothing matches', () => {
    const body = {
      thinking: { type: 'disabled' },
      tools: [{ name: 'read' }],
      messages: [{ role: 'user', content: 'hi' }],
    }
    expect(rewrite({ dropDeferredToolPlaceholder: true, stripCacheControl: true, mergeAdjacentRoles: true }, body, true)).toBeUndefined()
  })
})
