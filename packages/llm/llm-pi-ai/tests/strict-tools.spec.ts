import { afterEach, describe, expect, it, vi } from 'vitest'
import { Context } from '@deepseek-ai/cordis'
import LlmRuntime from '@deepseek-ai/dsh-llm'
import type { ToolSchema } from '@deepseek-ai/dsh-llm'
import * as LlmPiAi from '@deepseek-ai/dsh-llm-pi-ai'
import { resolveProfiles } from '../src/config.ts'
import type { PiAiProviderProfile } from '../src/config.ts'
import { assemble } from './assemble.ts'
import { closeMockServers, mockServer } from './mock-server.ts'

afterEach(async () => {
  vi.unstubAllEnvs()
  await closeMockServers()
})

/** A tool with optional properties at the root and inside array items. */
const READ: ToolSchema = {
  name: 'read',
  description: 'Read a file.',
  parameters: {
    type: 'object',
    properties: {
      file_path: { type: 'string' },
      offset: { type: 'integer' },
      label: { type: ['string', 'null'] },
      edits: {
        type: 'array',
        items: {
          type: 'object',
          properties: { old_string: { type: 'string' }, note: { type: 'string' } },
          required: ['old_string'],
        },
      },
    },
    required: ['file_path'],
  },
}

/** A tool whose open-ended map has no strict form, so pi-ai keeps it non-strict. */
const WORKFLOW: ToolSchema = {
  name: 'workflow',
  description: 'Run a workflow.',
  parameters: {
    type: 'object',
    properties: { inputs: { type: 'object', additionalProperties: { type: 'string' } }, retries: { type: 'integer' } },
    required: ['inputs'],
  },
}

const READ_ARGUMENTS = '{"file_path":"a.js","offset":null,"label":null,"edits":[{"old_string":"x","note":null}]}'
const WORKFLOW_ARGUMENTS = '{"inputs":{"k":"v"},"retries":null}'

/** One Responses turn calling both tools with the nulls strict sampling writes for omitted properties. */
function responsesToolCalls(): string[] {
  const calls = [['read', READ_ARGUMENTS], ['workflow', WORKFLOW_ARGUMENTS]] as const
  return [
    JSON.stringify({ type: 'response.created', response: { id: 'resp_1', status: 'in_progress', output: [] } }),
    ...calls.flatMap(([name, args], index) => {
      const item = { type: 'function_call', id: `fc_${index}`, call_id: `call_${index}`, name }
      return [
        JSON.stringify({ type: 'response.output_item.added', output_index: index, item: { ...item, arguments: '' } }),
        JSON.stringify({ type: 'response.function_call_arguments.delta', output_index: index, item_id: item.id, delta: args }),
        JSON.stringify({ type: 'response.output_item.done', output_index: index, item: { ...item, arguments: args, status: 'completed' } }),
      ]
    }),
    JSON.stringify({
      type: 'response.completed',
      response: { id: 'resp_1', status: 'completed', output: [], usage: { input_tokens: 3, output_tokens: 1, total_tokens: 4 } },
    }),
  ]
}

async function routed(server: { url: string }, route: PiAiProviderProfile): Promise<Context> {
  vi.stubEnv('PI_TEST_KEY', 'test-key')
  const ctx = new Context()
  await ctx.plugin(LlmRuntime)
  await ctx.plugin(LlmPiAi, {
    providers: { gw: { apiKeyEnv: 'PI_TEST_KEY', baseURL: server.url, models: [{ id: 'm' }], ...route } },
  })
  return ctx
}

interface WireTool {
  type: string
  name?: string
  strict?: boolean
  parameters?: unknown
  function?: { name: string; strict?: boolean; parameters: unknown }
}

function wireTools(request: unknown): WireTool[] {
  return (request as { tools: WireTool[] }).tools
}

describe('strictTools', () => {
  it('sends strict Responses declarations, keeps a schema with no strict form as written, and drops introduced nulls', async () => {
    const server = await mockServer([{ events: responsesToolCalls() }])
    const ctx = await routed(server, { api: 'openai-responses', compat: { supportsStrictMode: true }, strictTools: true })

    const result = await assemble(ctx, { provider: 'gw', model: 'm', messages: [], tools: [READ, WORKFLOW] })

    const [read, workflow] = wireTools(server.requests[0])
    expect(read).toMatchObject({ type: 'function', name: 'read', strict: true })
    expect(read?.parameters).toEqual({
      type: 'object',
      properties: {
        file_path: { type: 'string' },
        offset: { anyOf: [{ type: 'integer' }, { type: 'null' }] },
        label: { type: ['string', 'null'] },
        edits: {
          anyOf: [{
            type: 'array',
            items: {
              type: 'object',
              properties: { old_string: { type: 'string' }, note: { anyOf: [{ type: 'string' }, { type: 'null' }] } },
              required: ['old_string', 'note'],
              additionalProperties: false,
            },
          }, { type: 'null' }],
        },
      },
      required: ['file_path', 'offset', 'label', 'edits'],
      additionalProperties: false,
    })
    expect(workflow).toMatchObject({ type: 'function', name: 'workflow', strict: false, parameters: WORKFLOW.parameters })

    expect(result.finish.kind).toBe('tool-calls')
    const calls = result.message.content.filter(block => block.type === 'tool-call')
    expect(calls.map(call => [call.name, JSON.parse(call.arguments)])).toEqual([
      // A null the original schema admits stays; ones strict conversion introduced go.
      ['read', { file_path: 'a.js', label: null, edits: [{ old_string: 'x' }] }],
      // The non-strict tool keeps its arguments exactly as streamed.
      ['workflow', { inputs: { k: 'v' }, retries: null }],
    ])
  })

  it('leaves the request unchanged by default, with or without a strict-capable endpoint', async () => {
    const server = await mockServer([{ events: responsesToolCalls() }, { events: responsesToolCalls() }])
    const capable = await routed(server, { api: 'openai-responses', compat: { supportsStrictMode: true } })
    const result = await assemble(capable, { provider: 'gw', model: 'm', messages: [], tools: [READ, WORKFLOW] })
    expect(wireTools(server.requests[0])).toEqual([
      { type: 'function', name: 'read', description: READ.description, parameters: READ.parameters, strict: false },
      { type: 'function', name: 'workflow', description: WORKFLOW.description, parameters: WORKFLOW.parameters, strict: false },
    ])
    expect(result.message.content.filter(block => block.type === 'tool-call').map(call => call.arguments))
      .toEqual([READ_ARGUMENTS, WORKFLOW_ARGUMENTS])

    const plain = await routed(server, { api: 'openai-responses' })
    await assemble(plain, { provider: 'gw', model: 'm', messages: [], tools: [READ] })
    expect(wireTools(server.requests[1])).toEqual([
      { type: 'function', name: 'read', description: READ.description, parameters: READ.parameters },
    ])
  })

  it('sends strict Chat Completions declarations', async () => {
    const server = await mockServer([{ status: 401, body: JSON.stringify({ error: { message: 'expected mock failure' } }) }])
    const ctx = await routed(server, { api: 'openai-completions', compat: { supportsStrictMode: true }, strictTools: true })
    await assemble(ctx, { provider: 'gw', model: 'm', messages: [], tools: [READ] })
    const [read] = wireTools(server.requests[0])
    expect(read?.function).toMatchObject({ name: 'read', strict: true, parameters: { additionalProperties: false } })
  })

  it('leaves other protocols on a mixed route unchanged', async () => {
    const server = await mockServer([{ status: 401, body: JSON.stringify({ error: { message: 'expected mock failure' } }) }])
    const ctx = await routed(server, {
      api: 'openai-responses',
      compat: { supportsStrictMode: true },
      strictTools: true,
      models: [{ id: 'm' }, { id: 'claude', api: 'anthropic-messages' }],
    })
    await assemble(ctx, { provider: 'gw', model: 'claude', messages: [], tools: [READ] })
    expect(JSON.stringify(server.requests[0])).not.toContain('"strict"')
  })

  it('is refused on a route where no model would send strict declarations', () => {
    const base = { baseURL: 'http://127.0.0.1:1', models: [{ id: 'm' }], strictTools: true }
    expect(() => resolveProfiles({ gw: { ...base, api: 'openai-responses' } }))
      .toThrow(/sets strictTools, but no model on the route speaks openai-responses or openai-completions with compat supportsStrictMode/)
    expect(() => resolveProfiles({ gw: { ...base, api: 'anthropic-messages' } }))
      .toThrow(/sets strictTools/)
    expect(resolveProfiles({ gw: { ...base, api: 'openai-responses', compat: { supportsStrictMode: true } } }).get('gw')?.strictTools)
      .toBe(true)
  })
})
