import { expect, it } from 'vitest'
import { createAssistantMessage, createSystemMessage, createToolResultMessage, createUserMessage, MessageId, ToolCallId } from '@deepseek-ai/dsh-llm'
import { Config, resolveAdapterOptions } from '../../src/config.ts'
import { modelInfo } from '../../src/common/model-info.ts'
import { serialize } from '../../src/protocols/messages/serialize.ts'
import { MODEL, options } from './helpers.ts'

it('serializes native changes after paired tool results and system updates without changing stored messages', () => {
  const callId = ToolCallId('call')
  const result = createToolResultMessage({ callId, content: [{ type: 'text', text: 'done' }], isError: false })
  const messages = [
    createSystemMessage('initial', 'test'),
    createUserMessage({ source: { kind: 'user' }, content: [{ type: 'text', text: 'search' }] }),
    createAssistantMessage({ source: { provider: 'deepseek-official', model: MODEL }, content: [{ type: 'tool-call', id: callId, name: 'search', arguments: '{}' }] }),
    createSystemMessage('updated prompt', 'test'),
    result,
  ]
  const original = JSON.stringify(messages)
  const body = serialize(options({
    messages,
    tools: [{ name: 'fetch', description: 'fetch', parameters: {}, deferLoading: true }],
    toolUpdates: [{ afterMessageId: result.id, additions: ['fetch'], removals: ['search'] }],
  }), resolveAdapterOptions({ models: [{ id: MODEL, systemPromptUpdate: 'in-history', toolUpdate: 'in-history' }] }), messages, new Map(), () => undefined)
  expect(body.messages.slice(-3)).toEqual([
    { role: 'user', content: [{ type: 'tool_result', tool_use_id: 'call', content: [{ type: 'text', text: 'done' }], is_error: false }] },
    { role: 'system', content: [{ type: 'text', text: 'updated prompt' }] },
    { role: 'system', content: [
      { type: 'tool_addition', tool: { type: 'tool_reference', name: 'fetch' } },
      { type: 'tool_removal', tool: { type: 'tool_reference', name: 'search' } },
    ] },
  ])
  expect(body.tools?.[0]?.defer_loading).toBe(true)
  expect(JSON.stringify(messages)).toBe(original)
  expect(() => serialize(options({ toolUpdates: [{ afterMessageId: MessageId('missing'), additions: ['fetch'], removals: [] }] }), resolveAdapterOptions({}), [], new Map(), () => undefined)).toThrow(/whose message is absent/)
})

it.each(['in-history', 'addition-only'] as const)('validates %s capability and advertises it only for Messages', (mode) => {
  const config = Config({ models: [{ id: MODEL, toolUpdate: mode }] })
  const connection = resolveAdapterOptions(config)
  expect(modelInfo(connection, 'test', MODEL).toolUpdate).toBe(mode)
  expect(modelInfo({ ...connection, protocol: 'chat-completions' }, 'test', MODEL)).not.toHaveProperty('toolUpdate')
  expect(modelInfo(connection, 'test', 'unknown')).not.toHaveProperty('toolUpdate')
  expect(() => Config({ models: [{ id: MODEL, toolUpdate: 'invalid' }] })).toThrow()
  expect(() => resolveAdapterOptions({ models: [{ id: MODEL, toolUpdate: 'invalid' as 'in-history' }] })).toThrow(/toolUpdate must be/)
})
