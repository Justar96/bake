import { afterEach, describe, expect, it } from 'vitest'
import { Context } from '@deepseek-ai/cordis'
import LlmRuntime, {
  createAssistantMessage,
  createSystemMessage,
  createUserMessage,
  ReasoningEffortId,
  ToolCallId,
} from '@deepseek-ai/dsh-llm'
import type { Message, ToolSchema } from '@deepseek-ai/dsh-llm'
import * as LlmPiAi from '@deepseek-ai/dsh-llm-pi-ai'
import { assemble, type AssembledResult } from './assemble.ts'
import { deepseekProfile } from './deepseek-profile.ts'

/**
 * Real-API e2e for the pi-ai-backed adapter against DeepSeek: the shipped
 * `deepseek-official` route over the Anthropic-format Messages endpoint
 * (V4.1 Flash defaults, off/high/max reasoning, a replayed tool follow-up,
 * and the in-history prompt and tool changes it declares), plus a structural
 * cross-check against pi-ai's own `deepseek` catalog route over Chat
 * Completions. Key-gated.
 */

const FLASH = 'deepseek-flash'
const contexts: Context[] = []

async function harness(): Promise<Context> {
  const ctx = new Context()
  contexts.push(ctx)
  await ctx.plugin(LlmRuntime)
  await ctx.plugin(LlmPiAi, {
    providers: {
      'deepseek-official': deepseekProfile(),
      deepseek: { apiKeyEnv: 'DEEPSEEK_API_KEY' },
    },
  })
  return ctx
}

afterEach(async () => {
  await Promise.all(contexts.splice(0).map(ctx => ctx.fiber.dispose()))
})

function ask(text: string): Message[] {
  return [createUserMessage({
    content: [{ type: 'text', text }],
    source: { kind: 'plugin', plugin: 'test' },
  })]
}

function textOf(result: AssembledResult): string {
  return result.message.content
    .filter(block => block.type === 'text')
    .map(block => block.text)
    .join('')
}

function blockKinds(result: AssembledResult): string[] {
  return result.message.content.map(block => block.type)
}

const weatherTool: ToolSchema = {
  name: 'get_weather',
  description: 'Get the current weather for a city.',
  parameters: {
    type: 'object',
    properties: { city: { type: 'string', description: 'City name' } },
    required: ['city'],
  },
}

const timeTool: ToolSchema = {
  name: 'get_time',
  description: 'Get the current local time in a city.',
  parameters: {
    type: 'object',
    properties: { city: { type: 'string', description: 'City name' } },
    required: ['city'],
  },
}

describe.skipIf(!process.env.DEEPSEEK_API_KEY)('llm-pi-ai e2e (real DeepSeek API)', () => {
  it(`${FLASH} + route-default reasoning: plain text generation`, async () => {
    const ctx = await harness()
    const result = await assemble(ctx, {
      provider: 'deepseek-official',
      model: FLASH,
      messages: ask('Reply with exactly the word: pong'),
      maxTokens: 2000,
    })
    expect(result.finish.kind).toBe('stop')
    expect(textOf(result).toLowerCase()).toContain('pong')
  })

  it('flash + reasoning off: plain text without reasoning blocks', async () => {
    const ctx = await harness()
    const result = await assemble(ctx, {
      provider: 'deepseek-official',
      model: FLASH,
      reasoningEffort: ReasoningEffortId('off'),
      messages: ask('Reply with exactly the word: pong'),
      maxTokens: 50,
    })
    expect(result.finish.kind).toBe('stop')
    expect(result.message.content.some(block => block.type === 'reasoning')).toBe(false)
    expect(textOf(result).toLowerCase()).toContain('pong')
  })

  it(`${FLASH} + reasoning high: reasoning blocks present`, async () => {
    const ctx = await harness()
    const result = await assemble(ctx, {
      provider: 'deepseek-official',
      model: FLASH,
      reasoningEffort: ReasoningEffortId('high'),
      messages: ask('Which is larger, 9.11 or 9.8? Answer with just the number.'),
      maxTokens: 2000,
    })
    expect(result.finish.kind).toBe('stop')
    expect(result.message.content.some(block => block.type === 'reasoning')).toBe(true)
    expect(textOf(result)).toContain('9.8')
  })

  it('flash + reasoning max: tool-call round trip', async () => {
    const ctx = await harness()
    const question = ask('What is the weather in Paris right now? Use the get_weather tool.')

    const first = await assemble(ctx, {
      provider: 'deepseek-official',
      model: FLASH,
      reasoningEffort: ReasoningEffortId('max'),
      messages: question,
      tools: [weatherTool],
      maxTokens: 2000,
    })
    expect(first.finish.kind, `tool-call turn finished as ${JSON.stringify(first.finish)}`).toBe('tool-calls')
    const call = first.message.content.find(block => block.type === 'tool-call')
    expect(call).toBeDefined()
    expect(call!.name).toBe('get_weather')
    expect(JSON.parse(call!.arguments)).toMatchObject({ city: expect.stringMatching(/paris/i) as string })

    const second = await assemble(ctx, {
      provider: 'deepseek-official',
      model: FLASH,
      reasoningEffort: ReasoningEffortId('max'),
      messages: [
        ...question,
        first.message,
        createUserMessage({
          content: [{
            type: 'tool-result',
            toolCallId: ToolCallId(call!.id),
            content: [{ type: 'text', text: 'Sunny, 22°C' }],
          }],
          source: { kind: 'plugin', plugin: 'test' },
        }),
      ],
      tools: [weatherTool],
      maxTokens: 2000,
    })
    expect(second.finish.kind, `tool-result turn finished as ${JSON.stringify(second.finish)}`).toBe('stop')
    expect(textOf(second).toLowerCase()).toMatch(/sunny|22/)
  })

  it('a later in-history system prompt governs the next reply', async () => {
    const ctx = await harness()
    const result = await assemble(ctx, {
      provider: 'deepseek-official',
      model: FLASH,
      reasoningEffort: ReasoningEffortId('off'),
      messages: [
        createSystemMessage('You are a helpful assistant.', 'test'),
        ...ask('Say hello.'),
        createAssistantMessage({ content: [{ type: 'text', text: 'Hello!' }], source: { provider: 'deepseek-official', model: FLASH } }),
        createSystemMessage('Whatever the user asks, reply with exactly the word: banana', 'test'),
        ...ask('What is the capital of France?'),
      ],
      maxTokens: 50,
    })
    expect(result.finish.kind).toBe('stop')
    expect(textOf(result).toLowerCase()).toContain('banana')
  })

  it('a tool added mid-conversation is callable after its addition', async () => {
    const ctx = await harness()
    const opening = ask('Remember that I am in Paris.')
    const reply = createAssistantMessage({ content: [{ type: 'text', text: 'Noted.' }], source: { provider: 'deepseek-official', model: FLASH } })
    const anchor = createUserMessage({
      content: [{ type: 'text', text: 'What time is it here? Use the get_time tool.' }],
      source: { kind: 'plugin', plugin: 'test' },
    })
    const result = await assemble(ctx, {
      provider: 'deepseek-official',
      model: FLASH,
      reasoningEffort: ReasoningEffortId('off'),
      messages: [...opening, reply, anchor],
      tools: [weatherTool, timeTool],
      toolHistory: { tools: [weatherTool], updates: [{ afterMessageId: anchor.id, additions: [timeTool], removals: [] }] },
      maxTokens: 200,
    })
    expect(result.finish.kind, `tool-change turn finished as ${JSON.stringify(result.finish)}`).toBe('tool-calls')
    expect(result.message.content.find(block => block.type === 'tool-call')?.name).toBe('get_time')
  })

  it('the Messages route and the Chat Completions catalog route produce the same block structure', async () => {
    // Loose structural equivalence between the two wire protocols DeepSeek
    // serves: same block KINDS in the same order for a deterministic prompt.
    const ctx = await harness()
    const prompt = ask('Reply with exactly the word: pong')
    const [messages, completions] = await Promise.all([
      assemble(ctx, { provider: 'deepseek-official', model: FLASH, reasoningEffort: ReasoningEffortId('off'), messages: prompt, maxTokens: 50 }),
      assemble(ctx, { provider: 'deepseek', model: FLASH, reasoningEffort: ReasoningEffortId('off'), messages: prompt, maxTokens: 50 }),
    ])
    expect(blockKinds(messages)).toEqual(blockKinds(completions))
    expect(messages.finish.kind).toBe(completions.finish.kind)
  })
})
