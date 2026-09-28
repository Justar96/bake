import { describe, expect, it } from 'vitest'
import { Context } from '@deepseek-ai/cordis'
import { ToolCallId } from '@deepseek-ai/dsh-llm'
import AgentRegistry, { type Agent } from '@deepseek-ai/dsh-agent'
import SystemPrompt from '@deepseek-ai/dsh-system-prompt'
import ToolRuntime from '@deepseek-ai/dsh-tools'
import UserQuestionService, {
  type AskUserQuestionAnswer,
  type AskUserQuestionRequest,
} from '@deepseek-ai/dsh-user-questions'
import * as toolAskUser from '@deepseek-ai/dsh-tool-ask-user'

const testToolSignal = new AbortController().signal

interface QuestionAnswerer {
  ask(request: AskUserQuestionRequest): Promise<AskUserQuestionAnswer>
}

function registerQuestionAnswerer(ctx: Context, answerer: QuestionAnswerer): () => void {
  return ctx.on('user-questions/request', request => answerer.ask(request))
}

interface OptionSchemaShape {
  properties: {
    questions: {
      items: {
        properties: {
          options: {
            items: {
              properties: Record<string, { type: string }>
            }
          }
        } & Record<string, unknown>
      }
    }
  }
}

interface Described { description: string }

interface DescribedSchemaShape {
  properties: {
    questions: {
      items: {
        properties: {
          question: Described
          header: Described
          options: { items: { properties: { label: Described; description: Described } } }
        }
      }
    }
  }
}

async function setup() {
  const ctx = new Context()
  await ctx.plugin(AgentRegistry)
  await ctx.plugin(SystemPrompt)
  await ctx.plugin(ToolRuntime)
  await ctx.plugin(UserQuestionService)
  await ctx.plugin(toolAskUser)
  return ctx
}

function stubAgent(id: string, delegationDepth = 0): Agent {
  const agentId = id as Agent['id']
  return {
    id: agentId,
    session: { id: agentId, header: { delegationDepth } },
  } as unknown as Agent
}

describe('ask_user_question tool', () => {
  it('registers a model-facing tool schema', async () => {
    const ctx = await setup()
    const schema = ctx.tools.schemas().find(tool => tool.name === 'ask_user_question')

    expect(schema).toMatchObject({
      name: 'ask_user_question',
      parameters: {
        type: 'object',
        properties: {
          questions: { type: 'array' },
        },
        required: ['questions'],
      },
    })
    const parameters = schema?.parameters as unknown as OptionSchemaShape
    expect(parameters.properties.questions.items.properties).toMatchObject({
      id: { type: 'string' },
      question: { type: 'string' },
      header: { type: 'string' },
      options: { type: 'array' },
      multi_select: { type: 'boolean' },
    })
    expect(parameters.properties.questions.items.properties.options.items.properties).toMatchObject({
      label: { type: 'string' },
      description: { type: 'string' },
    })
    expect(parameters.properties.questions.items.properties.options.items.properties).not.toHaveProperty('value')
    expect(parameters.properties.questions.items.properties.options.items.properties).not.toHaveProperty('recommended')
    expect(parameters.properties.questions.items.properties.options.items.properties).not.toHaveProperty('preview')
  })

  it('pins the brevity guidance the model reads for each question field', async () => {
    const ctx = await setup()
    const parameters = ctx.tools.schemas().find(tool => tool.name === 'ask_user_question')?.parameters as unknown as DescribedSchemaShape
    const question = parameters.properties.questions.items.properties
    expect({
      question: question.question.description,
      header: question.header.description,
      label: question.options.items.properties.label.description,
      description: question.options.items.properties.description.description,
    }).toEqual({
      question: 'The question to ask, as one short sentence ending with a question mark.',
      header: 'Optional heading of at most about 12 characters, such as "Confirm" or "Choose Mode".',
      label: 'User-facing option label of 1-5 words.',
      description: 'One short sentence explaining the tradeoff or impact.',
    })
  })

  it('asks the registered user-questions provider and projects structured answers to text', async () => {
    const ctx = await setup()
    const seen: AskUserQuestionRequest[] = []
    registerQuestionAnswerer(ctx, {
      async ask(request) {
        seen.push(request)
        return { answers: [{ id: 'pkg', selected: ['pnpm'] }] }
      },
    })

    const result = await ctx.tools.execute({
      signal: testToolSignal,
      callId: ToolCallId('ask-1'),
      name: 'ask_user_question',
      arguments: {
        questions: [{
          id: 'pkg',
          question: 'Which package manager should I use?',
          options: [{ label: 'pnpm', description: 'Use pnpm workspaces.' }],
        }],
      },
    })

    expect(result).toMatchObject({
      isError: false,
      content: [{ type: 'text', text: '{"answers":[{"id":"pkg","selected":["pnpm"]}]}' }],
    })
    expect(seen).toMatchObject([{
      questions: [{
        id: 'pkg',
        question: 'Which package manager should I use?',
        options: [{ label: 'pnpm', description: 'Use pnpm workspaces.' }],
      }],
    }])
  })

  it('passes recommended option labels through without adding schema fields', async () => {
    const ctx = await setup()
    const seen: AskUserQuestionRequest[] = []
    registerQuestionAnswerer(ctx, {
      async ask(request) {
        seen.push(request)
        return { answers: [{ id: 'pkg', selected: ['pnpm (Recommended)'] }] }
      },
    })

    await ctx.tools.execute({
      signal: testToolSignal,
      callId: ToolCallId('ask-recommended'),
      name: 'ask_user_question',
      arguments: {
        questions: [{
          id: 'pkg',
          question: 'Which package manager should I use?',
          options: [
            { label: 'pnpm (Recommended)' },
            { label: 'npm' },
          ],
        }],
      },
    })

    expect(seen[0]?.questions[0]?.options).toEqual([
      { label: 'pnpm (Recommended)' },
      { label: 'npm' },
    ])
  })

  it('projects custom answers and multi-select choices', async () => {
    const ctx = await setup()
    registerQuestionAnswerer(ctx, {
      async ask() {
        return {
          answers: [
            { id: 'targets', selected: ['tests', 'docs'], custom: 'release notes' },
            { id: 'labels-only', selected: ['tests'] },
            { id: 'notes', selected: [], custom: 'ship today' },
          ],
        }
      },
    })

    const result = await ctx.tools.execute({
      signal: testToolSignal,
      callId: ToolCallId('ask-multi'),
      name: 'ask_user_question',
      arguments: {
        questions: [
          {
            id: 'targets',
            question: 'What should I update?',
            options: [{ label: 'tests' }, { label: 'docs' }],
            multi_select: true,
          },
          {
            id: 'labels-only',
            question: 'Which labels should I keep?',
            options: [{ label: 'tests' }, { label: 'docs' }],
            multi_select: true,
          },
          { id: 'notes', question: 'Any note?' },
        ],
      },
    })

    expect(result.isError).toBe(false)
    if (result.isError) throw new Error('expected ask_user_question success')
    expect(result.value).toEqual({
      answers: [
        { id: 'targets', selected: ['tests', 'docs'], custom: 'release notes' },
        { id: 'labels-only', selected: ['tests'] },
        { id: 'notes', selected: [], custom: 'ship today' },
      ],
    })
    expect(result.content).toEqual([{
      type: 'text',
      text: '{"answers":[{"id":"targets","selected":["tests","docs"],"custom":"release notes"},{"id":"labels-only","selected":["tests"]},{"id":"notes","selected":[],"custom":"ship today"}]}',
    }])
  })

  it('passes the tool abort signal to the user-questions request', async () => {
    const ctx = await setup()
    const seen: AskUserQuestionRequest[] = []
    registerQuestionAnswerer(ctx, {
      async ask(request) {
        seen.push(request)
        return { answers: [{ id: 'continue', selected: ['ok'] }] }
      },
    })
    const controller = new AbortController()

    await ctx.tools.execute({
      callId: ToolCallId('ask-2'),
      name: 'ask_user_question',
      arguments: { questions: [{ id: 'continue', question: 'Continue?' }] },
      signal: controller.signal,
    })

    expect(seen[0]?.signal).toBe(controller.signal)
  })

  it('passes optional header and a resumed runtime root through to the user-questions request', async () => {
    const ctx = await setup()
    const seen: AskUserQuestionRequest[] = []
    registerQuestionAnswerer(ctx, {
      async ask(request) {
        seen.push(request)
        return { answers: [{ id: 'continue', selected: ['ok'] }] }
      },
    })
    const agent = stubAgent('resumed-root', 1)
    ctx.agents.enter(agent, undefined)

    const result = await ctx.tools.execute({
      signal: testToolSignal,
      callId: ToolCallId('ask-3'),
      name: 'ask_user_question',
      arguments: { questions: [{ id: 'continue', header: 'Confirm', question: 'Continue?' }] },
      agent,
    })

    expect(result.content).toEqual([{ type: 'text', text: '{"answers":[{"id":"continue","selected":["ok"]}]}' }])
    expect(seen[0]).toMatchObject({ questions: [{ id: 'continue', header: 'Confirm', question: 'Continue?' }], agent })
  })

  it('returns structured user-questions errors through tool execution', async () => {
    const ctx = await setup()

    const result = await ctx.tools.execute({
      signal: testToolSignal,
      callId: ToolCallId('ask-no-provider'),
      name: 'ask_user_question',
      arguments: { questions: [{ id: 'continue', question: 'Continue?' }] },
    })

    expect(result).toMatchObject({
      isError: true,
      error: { info: { name: 'UserQuestionError', code: 'NO_PROVIDER' } },
    })
  })

  it('rejects a live runtime-owned agent with a structured DELEGATED_CALLER error', async () => {
    const ctx = await setup()
    const seen: AskUserQuestionRequest[] = []
    registerQuestionAnswerer(ctx, {
      async ask(request) {
        seen.push(request)
        return { answers: [{ id: 'continue', selected: ['ok'] }] }
      },
    })
    const root = stubAgent('root', 0)
    const child = stubAgent('child', 0)
    ctx.agents.enter(root, undefined)
    ctx.agents.enter(child, root)

    const result = await ctx.tools.execute({
      signal: testToolSignal,
      callId: ToolCallId('ask-delegated'),
      name: 'ask_user_question',
      arguments: { questions: [{ id: 'continue', question: 'Continue?' }] },
      agent: child,
    })

    expect(result).toMatchObject({
      isError: true,
      error: { info: { name: 'UserQuestionError', code: 'DELEGATED_CALLER' } },
      content: [{
        type: 'text',
        text: "Error: human interaction is unavailable while the calling agent is owned by another live agent; include the unresolved question or decision in the child agent's final result",
      }],
    })
    expect(seen).toHaveLength(0)
  })

  it('returns a structured error for empty question batches', async () => {
    const ctx = await setup()

    const result = await ctx.tools.execute({
      signal: testToolSignal,
      callId: ToolCallId('ask-empty'),
      name: 'ask_user_question',
      arguments: { questions: [] },
    })

    expect(result).toMatchObject({
      isError: true,
      error: { info: { name: 'UserQuestionError', code: 'EMPTY_QUESTIONS' } },
    })
  })

  it('titles a pending call by its headers, or by a question that has none', async () => {
    const tool = (await setup()).tools.get('ask_user_question')!
    const long = 'Which parts of the migration should I include in this pass, given the constraints we discussed?'
    expect(tool.presentCall?.({ questions: [{ id: 'scope', header: 'Choose scope', question: long }] }))
      .toEqual({ card: 'generic', title: 'Ask: Choose scope', kind: 'other' })
    expect(tool.presentCall?.({ questions: [
      { id: 'scope', header: 'Choose scope', question: long, options: [{ label: 'Tooling swaps', description: 'x'.repeat(200) }] },
      { id: 'fixes', header: 'Runtime fixes', question: 'Same commit?', multi_select: true },
    ] })).toEqual({ card: 'generic', title: 'Ask 2 questions: Choose scope, Runtime fixes', kind: 'other' })
    // A blank or missing header yields to the question itself, on one line and cut.
    expect(tool.presentCall?.({ questions: [{ id: 'scope', header: '  ', question: `${long}\nMore detail.` }] }))
      .toEqual({ card: 'generic', title: 'Ask: Which parts of the migration should I include i…', kind: 'other' })
    const twelve = Array.from({ length: 12 }, (_, index) => ({ id: `q${index}`, header: `Header ${index}`, question: '?' }))
    const many = tool.presentCall?.({ questions: twelve })
    expect(many?.title).toMatch(/^Ask 12 questions: Header 0, Header 1, .*…$/u)
    expect(Array.from(many?.title ?? '')).toHaveLength(80)
  })

  it('falls back to generic rendering for obsolete or invalid logged arguments', async () => {
    const tool = (await setup()).tools.get('ask_user_question')!
    for (const args of [
      { questions: [{ question: 'Continue?' }] },
      { questions: [{ id: 'q', prompt: 'Continue?' }] },
      { questions: 'Continue?' },
      { question: 'Continue?' },
      null,
    ]) {
      expect(tool.presentCall?.(args)).toBeUndefined()
      expect(tool.presentResult?.(args, { content: [{ type: 'text', text: '{"answers":[]}' }], isError: false })).toBeUndefined()
    }
  })

  it('shows each answer on its own line without changing the result the model reads', async () => {
    const ctx = await setup()
    registerQuestionAnswerer(ctx, {
      async ask() {
        return { answers: [
          { id: 'scope', selected: ['Tooling swaps', 'Shared versions'], custom: 'skip CI\nfor now' },
          { id: 'fixes', selected: ['Separate commit'] },
          { id: 'notes', selected: [], custom: 'ship today' },
          { id: 'skipped', selected: [] },
        ] }
      },
    })
    const args = { questions: [
      { id: 'scope', header: 'Choose scope', question: 'Which parts?', multi_select: true,
        options: [{ label: 'Tooling swaps' }, { label: 'Shared versions' }] },
      { id: 'fixes', question: 'Same commit?', options: [{ label: 'Same commit' }, { label: 'Separate commit' }] },
      { id: 'notes', question: 'Any note?' },
      { id: 'skipped', question: 'Anything else?' },
    ] }
    const result = await ctx.tools.execute({
      signal: testToolSignal, callId: ToolCallId('ask-view'), name: 'ask_user_question', arguments: args,
    })
    expect(result.content).toEqual([{
      type: 'text',
      text: '{"answers":[{"id":"scope","selected":["Tooling swaps","Shared versions"],"custom":"skip CI\\nfor now"},'
        + '{"id":"fixes","selected":["Separate commit"]},{"id":"notes","selected":[],"custom":"ship today"},'
        + '{"id":"skipped","selected":[]}]}',
    }])
    expect(ctx.tools.get('ask_user_question')?.presentResult?.(args, result)).toEqual({
      card: 'generic',
      content: [{
        type: 'text',
        text: 'scope \u2192 Tooling swaps, Shared versions, "skip CI for now"\n'
          + 'fixes \u2192 Separate commit\nnotes \u2192 "ship today"\nskipped \u2192 \u2014',
      }],
    })
  })

  it('keeps the raw result for a failure or for text that is not the answers JSON', async () => {
    const tool = (await setup()).tools.get('ask_user_question')!
    const args = { questions: [{ id: 'q', question: 'Continue?' }] }
    const text = (value: string) => ({ content: [{ type: 'text' as const, text: value }], isError: false })
    expect(tool.presentResult?.(args, { content: [{ type: 'text', text: 'Error: no provider' }], isError: true })).toBeUndefined()
    for (const value of ['not json', '[]', '{"answers":[]}', '{"answers":[{"id":1,"selected":[]}]}',
      '{"answers":[{"id":"q","selected":[2]}]}', '{"answers":[{"id":"q","selected":[],"custom":3}]}']) {
      expect(tool.presentResult?.(args, text(value))).toBeUndefined()
    }
    // A long free-form answer is cut on its line; the model still reads all of it.
    const custom = 'word '.repeat(60).trim()
    const view = tool.presentResult?.(args, text(JSON.stringify({ answers: [{ id: 'q', selected: [], custom }] })))
    expect(view).toEqual({ card: 'generic', content: [{ type: 'text', text: `q \u2192 "${custom.slice(0, 159).trimEnd()}\u2026"` }] })
  })

  it('unregisters the tool when its plugin fiber is disposed', async () => {
    const ctx = new Context()
    await ctx.plugin(SystemPrompt)
    await ctx.plugin(ToolRuntime)
    await ctx.plugin(UserQuestionService)
    const fiber = await ctx.plugin(toolAskUser)
    expect(ctx.tools.get('ask_user_question')).toBeDefined()

    await fiber.dispose()

    expect(ctx.tools.get('ask_user_question')).toBeUndefined()
  })
})
