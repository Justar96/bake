/**
 * Route-declared transcript handling and the Anthropic-compatible thinking
 * dialect, through the DeepSeek profile the base bundle ships: the declared
 * capabilities reach the seam's model info, the wire carries later system
 * prompts and tool changes in place, and the thinking block takes DeepSeek's
 * spelling.
 */

import { afterEach, describe, expect, it } from 'vitest'
import { Context } from '@deepseek-ai/cordis'
import LlmRuntime, {
  createAssistantMessage,
  createSystemMessage,
  createToolResultMessage,
  createUserMessage,
  LlmError,
  ReasoningEffortId,
  ToolCallId,
} from 'bake-llm'
import type { GenerateOptions, Message, ToolDeclaration } from 'bake-llm'
import { PiAiAdapter } from 'bake-llm-pi-ai'
import type { PiAiModelProfile } from 'bake-llm-pi-ai'
import { PiAiCatalogError, resolveRouteModels } from '../src/catalog.ts'
import { resolveProfiles } from '../src/config.ts'
import { toPiContext } from '../src/context.ts'
import { DEFERRED_TOOL_PLACEHOLDER } from '../src/payload.ts'
import { memoryAuth } from './auth-double.ts'
import { DEEPSEEK_PRO_DESCRIPTION, deepseekProfile } from './deepseek-profile.ts'
import { anthropicTextWire, closeMockServers, mockServer } from './mock-server.ts'

afterEach(async () => {
  await closeMockServers()
})

function deepseekAdapter(baseURL: string): PiAiAdapter {
  return new PiAiAdapter({
    profiles: () => resolveProfiles({ 'deepseek-official': deepseekProfile(baseURL) }),
    resolveApiKey: () => Promise.resolve('sk-test'),
    auth: memoryAuth(),
  })
}

async function deepseekRuntime(baseURL: string): Promise<Context> {
  const ctx = new Context()
  await ctx.plugin(LlmRuntime)
  ctx.llm.registerAdapter(['deepseek-official'], deepseekAdapter(baseURL))
  return ctx
}

async function drain(ctx: Context, options: Omit<GenerateOptions, 'provider'>): Promise<void> {
  for await (const chunk of ctx.llm.stream({ provider: 'deepseek-official', ...options })) {
    if (chunk.type === 'finish' && chunk.reason.kind === 'error') throw new Error(JSON.stringify(chunk.reason))
  }
}

const source = { kind: 'plugin', plugin: 'test' } as const

function user(text: string): Message {
  return createUserMessage({ content: [{ type: 'text', text }], source })
}

function assistant(text: string): Message {
  return createAssistantMessage({ content: [{ type: 'text', text }], source: { provider: 'deepseek-official', model: 'deepseek-flash' } })
}

interface WireMessage {
  role: string
  content: string | { type: string; text?: string; tool?: { name: string }; tool_use_id?: string }[]
}

interface WireBody {
  system?: { text: string }[]
  messages: WireMessage[]
  tools?: { name: string; defer_loading?: boolean }[]
  thinking?: Record<string, unknown>
  output_config?: Record<string, unknown>
}

/** Collapse one wire message to its role and the block types or texts it carries. */
function shape(message: WireMessage): string {
  const blocks = typeof message.content === 'string'
    ? [message.content]
    : message.content.map(block => block.text ?? (block.tool === undefined ? block.type : `${block.type}:${block.tool.name}`))
  return `${message.role}[${blocks.join('|')}]`
}

describe('route-declared transcript updates', () => {
  it('turns each declaration into the pi-ai switch that carries it and keeps the installed compat', () => {
    const catalog = resolveRouteModels({
      provider: 'gateway',
      api: 'anthropic-messages',
      baseURL: 'https://gateway.example',
      compat: { allowEmptySignature: true },
      models: [
        { id: 'both', systemPromptUpdate: 'in-history', toolUpdate: 'in-history', description: 'Guidance.' },
        { id: 'prompt-only', systemPromptUpdate: 'in-history' },
        { id: 'plain' },
      ],
      defaultInput: ['text'],
      defaultContextWindow: 1000,
      defaultMaxTokens: 100,
    })
    const compat = Object.fromEntries(catalog.models.map(model => [model.id, model.compat]))
    expect(compat['both']).toEqual({
      allowEmptySignature: true,
      supportsMidConvoSystemMessages: true,
      supportsMidConvoToolChanges: true,
    })
    expect(compat['prompt-only']).toEqual({ allowEmptySignature: true, supportsMidConvoSystemMessages: true })
    expect(compat['plain']).toEqual({ allowEmptySignature: true })
    expect([...catalog.harnessInfo]).toEqual([
      ['both', { description: 'Guidance.', systemPromptUpdate: 'in-history', toolUpdate: 'in-history' }],
      ['prompt-only', { systemPromptUpdate: 'in-history' }],
    ])
  })

  it('refuses a declaration its protocol cannot carry, an unknown spelling, and an empty description', () => {
    const route = (entry: PiAiModelProfile, api = 'anthropic-messages') => () => resolveRouteModels({
      provider: 'gateway',
      api,
      baseURL: 'https://gateway.example',
      models: [entry],
      defaultInput: ['text'],
      defaultContextWindow: 1000,
      defaultMaxTokens: 100,
    })
    expect(route({ id: 'm', toolUpdate: 'in-history' }, 'openai-completions')).toThrow(PiAiCatalogError)
    expect(route({ id: 'm', toolUpdate: 'in-history' }, 'openai-completions'))
      .toThrow('model "m" declares toolUpdate, but its api is "openai-completions"')
    expect(route({ id: 'm', systemPromptUpdate: 'replace' as 'in-history' }))
      .toThrow('model "m" systemPromptUpdate must be "in-history" when present')
    expect(route({ id: 'm', description: '' })).toThrow('model "m" has an empty description')
  })

  it('reports the declarations and description through model info and the listing', async () => {
    const ctx = await deepseekRuntime('https://api.deepseek.example/anthropic')
    const flash = await ctx.llm.resolveModelInfo('deepseek-official', 'deepseek-flash')
    expect(flash).toMatchObject({
      name: 'DeepSeek-V41-Flash',
      inputModalities: ['text', 'image'],
      context: { contextWindow: 1_000_000 },
      defaultMaxTokens: 256_000,
      systemPromptUpdate: 'in-history',
      toolUpdate: 'in-history',
      reasoning: { defaultEffort: 'high' },
    })
    expect(flash.reasoning?.efforts.map(effort => effort.id)).toEqual(['off', 'low', 'high', 'max'])
    const pro = await ctx.llm.resolveModelInfo('deepseek-official', 'deepseek-v4-pro')
    expect(pro.description).toBe(DEEPSEEK_PRO_DESCRIPTION)
    expect(pro.systemPromptUpdate).toBeUndefined()
    expect(pro.toolUpdate).toBeUndefined()
    expect((await ctx.llm.listModels('deepseek-official')).map(model => [model.id, model.description]))
      .toEqual([['deepseek-flash', undefined], ['deepseek-v4-pro', DEEPSEEK_PRO_DESCRIPTION]])
  })
})

describe('adaptive thinking spelled for an Anthropic-compatible endpoint', () => {
  it('sends thinking enabled beside the effort, and disabled without one', async () => {
    const server = await mockServer([{ wire: anthropicTextWire }, { wire: anthropicTextWire }, { wire: anthropicTextWire }])
    const ctx = await deepseekRuntime(server.url)

    await drain(ctx, { model: 'deepseek-flash', messages: [user('hi')] })
    await drain(ctx, { model: 'deepseek-flash', messages: [user('hi')], reasoningEffort: ReasoningEffortId('max') })
    await drain(ctx, { model: 'deepseek-flash', messages: [user('hi')], reasoningEffort: ReasoningEffortId('off') })

    expect(server.paths.map(path => path.split('?')[0])).toEqual(['/v1/messages', '/v1/messages', '/v1/messages'])
    expect(server.headers[0]?.['x-api-key']).toBe('sk-test')
    const [byDefault, max, off] = server.requests as WireBody[]
    expect(byDefault).toMatchObject({ model: 'deepseek-flash', max_tokens: 256_000 })
    expect(byDefault?.thinking).toEqual({ type: 'enabled' })
    expect(byDefault?.output_config).toEqual({ effort: 'high' })
    expect(max?.thinking).toEqual({ type: 'enabled' })
    expect(max?.output_config).toEqual({ effort: 'max' })
    expect(off?.thinking).toEqual({ type: 'disabled' })
    expect(off?.output_config).toBeUndefined()
  })

  it('keeps Anthropic\'s adaptive spelling unless the route names the DeepSeek one', async () => {
    const server = await mockServer([{ wire: anthropicTextWire }])
    const profile = deepseekProfile(server.url)
    delete profile.adaptiveThinkingType
    const ctx = new Context()
    await ctx.plugin(LlmRuntime)
    ctx.llm.registerAdapter(['deepseek-official'], new PiAiAdapter({
      profiles: () => resolveProfiles({ 'deepseek-official': profile }),
      resolveApiKey: () => Promise.resolve('sk-test'),
      auth: memoryAuth(),
    }))
    await drain(ctx, { model: 'deepseek-flash', messages: [user('hi')] })
    expect((server.requests[0] as WireBody).thinking).toEqual({ type: 'adaptive', display: 'summarized' })
  })

  it('refuses the respelling on a route where no request would carry it', () => {
    expect(() => resolveProfiles({
      gateway: {
        api: 'anthropic-messages',
        baseURL: 'https://gateway.example',
        adaptiveThinkingType: 'enabled',
        models: [{ id: 'budgeted', reasoningEfforts: { high: 'high' } }],
      },
    })).toThrow('sets adaptiveThinkingType "enabled", but no model on the route speaks anthropic-messages with compat forceAdaptiveThinking')
    expect(() => resolveProfiles({
      gateway: {
        api: 'anthropic-messages',
        baseURL: 'https://gateway.example',
        adaptiveThinkingType: 'on' as 'enabled',
        models: [{ id: 'm' }],
      },
    })).toThrow('adaptiveThinkingType must be one of adaptive, enabled')
  })
})

describe('in-history system prompts on the wire', () => {
  it('sends a later prompt as a system message before the reply it governs', async () => {
    const server = await mockServer([{ wire: anthropicTextWire }])
    const ctx = await deepseekRuntime(server.url)
    await drain(ctx, {
      model: 'deepseek-flash',
      messages: [
        createSystemMessage('first prompt', 'test'),
        user('one'),
        assistant('reply one'),
        createSystemMessage('second prompt', 'test'),
        user('two'),
      ],
    })
    const body = server.requests[0] as WireBody
    expect(body.system?.map(block => block.text)).toEqual(['first prompt'])
    expect(body.messages.map(shape)).toEqual([
      'user[one]',
      'assistant[reply one]',
      'user[two]',
      'system[second prompt]',
    ])
  })

  it('takes the last of a leading run as the prompt, and refuses an empty later prompt', () => {
    const transcript = { systemPromptUpdate: 'in-history' } as const
    const leading = toPiContext({
      provider: 'p',
      model: 'm',
      messages: [createSystemMessage('stale', 'test'), createSystemMessage('current', 'test'), user('hi')],
    }, undefined, undefined, transcript)
    expect(leading.systemPrompt).toBe('current')
    expect(leading.messages.map(message => message.role)).toEqual(['user'])

    expect(() => toPiContext({
      provider: 'p',
      model: 'm',
      messages: [createSystemMessage('first', 'test'), user('hi'), assistant('ok'), createSystemMessage('', 'test')],
    }, undefined, undefined, transcript)).toThrow(LlmError)
  })

  it('folds a later prompt into a user message on a route that did not declare in-history', async () => {
    const server = await mockServer([{ wire: anthropicTextWire }])
    const ctx = await deepseekRuntime(server.url)
    await drain(ctx, {
      model: 'deepseek-v4-pro',
      messages: [createSystemMessage('first prompt', 'test'), user('one'), assistant('reply one'), createSystemMessage('second prompt', 'test'), user('two')],
    })
    const body = server.requests[0] as WireBody
    expect(body.messages.map(shape)).toEqual(['user[one]', 'assistant[reply one]', 'user[second prompt|two]'])
  })
})

describe('native tool changes on the wire', () => {
  const read: ToolDeclaration = { name: 'read', description: 'Read a file.', parameters: { type: 'object', properties: {} } }
  const searchSchema: ToolDeclaration = { name: 'search', description: 'Search.', parameters: { type: 'object', properties: {} } }
  const search: ToolDeclaration = { ...searchSchema, deferLoading: true }
  const callId = ToolCallId('call-1')

  function toolHistory(): { messages: Message[]; anchor: Message } {
    const call = createAssistantMessage({
      content: [{ type: 'tool-call', id: callId, name: 'read', arguments: '{}' }],
      source: { provider: 'deepseek-official', model: 'deepseek-flash' },
    })
    const result = createToolResultMessage({ callId, content: [{ type: 'text', text: 'contents' }], isError: false })
    return { messages: [createSystemMessage('prompt', 'test'), user('look'), call, result], anchor: result }
  }

  it('declares a later tool deferred and surfaces it after the turn that added it', async () => {
    const server = await mockServer([{ wire: anthropicTextWire }])
    const ctx = await deepseekRuntime(server.url)
    const { messages, anchor } = toolHistory()
    // The Session's tool history, which the seam projects for this route.
    await drain(ctx, {
      model: 'deepseek-flash',
      messages,
      tools: [searchSchema],
      toolHistory: { tools: [read], updates: [{ afterMessageId: anchor.id, additions: [searchSchema], removals: ['read'] }] },
    })

    const body = server.requests[0] as WireBody
    expect(body.tools?.map(tool => [tool.name, tool.defer_loading === true])).toEqual([
      ['read', false],
      ['search', true],
    ])
    expect(body.messages.map(shape)).toEqual([
      'user[look]',
      'assistant[tool_use]',
      'user[tool_result]',
      'system[tool_removal:read|tool_addition:search]',
    ])
  })

  it('keeps pi-ai\'s deferred placeholder on a route without messagesWire', async () => {
    const server = await mockServer([{ wire: anthropicTextWire }])
    const profile = deepseekProfile(server.url)
    delete profile.messagesWire
    const { messages, anchor } = toolHistory()
    for await (const chunk of new PiAiAdapter({
      profiles: () => resolveProfiles({ 'deepseek-official': profile }),
      resolveApiKey: () => Promise.resolve('sk-test'),
      auth: memoryAuth(),
    }).stream({
      provider: 'deepseek-official',
      model: 'deepseek-flash',
      messages,
      tools: [read, search],
      toolUpdates: [{ afterMessageId: anchor.id, additions: ['search'], removals: [] }],
    })) void chunk

    const body = server.requests[0] as WireBody
    expect(body.tools?.map(tool => [tool.name, tool.defer_loading === true])).toEqual([
      ['read', false],
      [DEFERRED_TOOL_PLACEHOLDER, true],
      ['search', true],
    ])
    expect(JSON.stringify(body)).toContain('"cache_control":')
  })

  it('refuses a tool change whose anchor or definition the request lacks', () => {
    const { messages } = toolHistory()
    const options = (update: { afterMessageId: Message['id']; additions: string[]; removals: string[] }): GenerateOptions => ({
      provider: 'p', model: 'm', messages, tools: [read, search], toolUpdates: [update],
    })
    const absent = createUserMessage({ content: [{ type: 'text', text: 'elsewhere' }], source })
    expect(() => toPiContext(options({ afterMessageId: absent.id, additions: ['search'], removals: [] })))
      .toThrow('pi-ai tool update follows a message absent from the request')
    expect(() => toPiContext(options({ afterMessageId: messages[2]!.id, additions: ['search'], removals: [] })))
      .toThrow('only a user turn can anchor one')
    expect(() => toPiContext(options({ afterMessageId: messages[3]!.id, additions: ['missing'], removals: [] })))
      .toThrow('pi-ai tool update adds "missing", which the request does not declare')
  })

  it('sends the folded current tools when changes reach a model that cannot take them natively', async () => {
    const server = await mockServer([{ wire: anthropicTextWire }])
    const { messages, anchor } = toolHistory()
    // Straight to the adapter: the seam never projects changes for this model,
    // so this is the adapter's own answer to a caller that bypasses it.
    for await (const chunk of deepseekAdapter(server.url).stream({
      provider: 'deepseek-official',
      model: 'deepseek-v4-pro',
      messages,
      tools: [read, search],
      toolUpdates: [{ afterMessageId: anchor.id, additions: ['search'], removals: ['read'] }],
    })) void chunk

    const body = server.requests[0] as WireBody
    expect(body.tools?.map(tool => [tool.name, tool.defer_loading === true])).toEqual([['search', false]])
    expect(body.messages.map(shape)).toEqual(['user[look]', 'assistant[tool_use]', 'user[tool_result]'])
  })
})

describe('the DeepSeek Messages wire', () => {
  it('sends no placeholder, no cache breakpoints, and one leading user message', async () => {
    const server = await mockServer([{ wire: anthropicTextWire }])
    const ctx = await deepseekRuntime(server.url)
    const read: ToolDeclaration = { name: 'read', description: 'Read a file.', parameters: { type: 'object', properties: {} } }
    await drain(ctx, {
      model: 'deepseek-flash',
      messages: [createSystemMessage('prompt', 'test'), user('fix the bug'), user('runtime context')],
      tools: [read],
      toolHistory: { tools: [read], updates: [] },
    })

    const body = server.requests[0] as WireBody
    expect(body.system?.map(block => block.text)).toEqual(['prompt'])
    expect(body.tools?.map(tool => tool.name)).toEqual(['read'])
    expect(JSON.stringify(body)).not.toContain('cache_control')
    expect(JSON.stringify(body)).not.toContain(DEFERRED_TOOL_PLACEHOLDER)
    expect(body.messages.map(shape)).toEqual(['user[fix the bug|runtime context]'])
    expect(body.thinking).toEqual({ type: 'enabled' })
  })

  it('refuses messagesWire on a route where no request would carry it', () => {
    expect(() => resolveProfiles({
      gateway: {
        api: 'openai-completions',
        baseURL: 'https://gateway.example',
        messagesWire: { stripCacheControl: true },
        models: [{ id: 'm' }],
      },
    })).toThrow('sets messagesWire, but no model on the route speaks anthropic-messages')
    expect(() => resolveProfiles({
      gateway: {
        api: 'openai-completions',
        baseURL: 'https://gateway.example',
        messagesWire: { stripCacheControl: false },
        models: [{ id: 'm' }],
      },
    })).not.toThrow()
  })
})
