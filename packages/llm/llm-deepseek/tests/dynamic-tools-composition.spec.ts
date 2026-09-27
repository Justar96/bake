/** Loader → agent loop → persisted history → provider wire, including restart and tool disposal. */
import { mkdtemp, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { pathToFileURL } from 'node:url'
import { afterEach, expect, it, vi } from 'vitest'
import { Context } from '@deepseek-ai/cordis'
import Loader from '@deepseek-ai/cordis-plugin-loader'
import Include from '@deepseek-ai/cordis-plugin-include'
import AgentRegistry, { type Agent } from '@deepseek-ai/dsh-agent'
import AgentLoop from '@deepseek-ai/dsh-agent-loop'
import LlmRuntime, { createUserMessage } from '@deepseek-ai/dsh-llm'
import SessionStore, { SessionId } from '@deepseek-ai/dsh-session'
import SessionProjectionRegistry from '@deepseek-ai/dsh-session-projection'
import JsonlSessionPersistence from '@deepseek-ai/dsh-session-persistence-jsonl'
import SystemPrompt from '@deepseek-ai/dsh-system-prompt'
import ToolRuntime, { defineContentToolFixture } from '@deepseek-ai/dsh-tools'
import * as LlmDeepSeek from '../src/index.ts'
import { MODEL, server, sse, textEvents } from './messages/helpers.ts'

const contexts: Context[] = []
const roots: string[] = []
const closers: (() => Promise<void>)[] = []
afterEach(async () => {
  const failures: unknown[] = []
  try {
    for (const dispose of [
      ...contexts.splice(0).map(ctx => () => ctx.fiber.dispose()),
      ...closers.splice(0),
      ...roots.splice(0).map(root => () => rm(root, { recursive: true, force: true })),
    ]) {
      try { await dispose() } catch (error) { failures.push(error) }
    }
  } finally {
    vi.unstubAllEnvs()
  }
  if (failures.length > 0) throw new AggregateError(failures, 'dynamic tool fixture cleanup failed')
})

async function boot(root: string, baseURL: string, protocol: 'messages' | 'chat-completions') {
  const modules = new Map<string, unknown>([
    ['@deepseek-ai/dsh-llm', LlmRuntime],
    ['@deepseek-ai/dsh-session', SessionStore],
    ['@deepseek-ai/dsh-session-projection', SessionProjectionRegistry],
    ['@deepseek-ai/dsh-session-persistence-jsonl', JsonlSessionPersistence],
    ['@deepseek-ai/dsh-system-prompt', SystemPrompt],
    ['@deepseek-ai/dsh-tools', ToolRuntime],
    ['@deepseek-ai/dsh-agent', AgentRegistry],
    ['@deepseek-ai/dsh-agent-loop', AgentLoop],
    ['@deepseek-ai/dsh-llm-deepseek', LlmDeepSeek],
  ])
  const configs: Record<string, unknown> = {
    '@deepseek-ai/dsh-session-persistence-jsonl': { root: join(root, 'sessions'), compression: 'none' },
    '@deepseek-ai/dsh-system-prompt': { personaPrefix: '', personaSuffix: '' },
    '@deepseek-ai/dsh-agent-loop': { agents: [] },
    '@deepseek-ai/dsh-llm-deepseek': { protocol, baseURL, models: [{ id: MODEL, toolUpdate: 'in-history' }] },
  }
  const path = join(root, 'cordis.yml')
  await writeFile(path, [...modules.keys()].map((name, index) =>
    `- id: plugin-${index}\n  name: ${JSON.stringify(name)}\n  config: ${JSON.stringify(configs[name] ?? {})}\n`).join(''))
  const ctx = new Context()
  contexts.push(ctx)
  ctx.baseUrl = pathToFileURL(root).href + '/'
  await ctx.plugin(Loader)
  ctx.loader.builtins.include = Include
  ctx.loader.internal = {
    version: 'v2',
    async import(specifier: string) {
      if (!modules.has(specifier)) throw new Error(`unexpected plugin: ${specifier}`)
      return modules.get(specifier)
    },
  } as unknown as NonNullable<typeof ctx.loader.internal>
  await ctx.loader.create({ name: 'cordis:include', config: { path: pathToFileURL(path).href } })
  await ctx.loader.await()
  return ctx
}

async function register(ctx: Context, name: string) {
  return ctx.plugin({
    name: `test-${name}`, inject: ['tools'],
    apply(owner: Context) {
      owner.tools.register(defineContentToolFixture({
        name, description: name, parameters: {}, execute: async () => [{ type: 'text', text: 'done' }],
      }))
    },
  })
}
async function send(agent: Agent, text: string) {
  agent.followup(createUserMessage({ source: { kind: 'user' }, content: [{ type: 'text', text }] }))
  await agent.whenIdle()
  expect(agent.session.snapshotEvents().findLast(event => event.type === 'turn/end')?.data.reason).toEqual({ kind: 'completed' })
}

it.each(['messages', 'chat-completions'] as const)('reconstructs dynamic tools through %s Loader composition and disk resume', async (protocol) => {
  const root = await mkdtemp(join(tmpdir(), 'bake-dynamic-tools-'))
  roots.push(root)
  vi.stubEnv('DSH_HOME', root)
  vi.stubEnv('DEEPSEEK_API_KEY', 'test-key')
  const chat = 'data: {"choices":[{"delta":{"content":"done"}}]}\n\n'
    + 'data: {"choices":[{"delta":{},"finish_reason":"stop"}]}\n\ndata: [DONE]\n\n'
  const http = await server(response => response.end(protocol === 'messages' ? sse(textEvents) : chat))
  closers.push(() => http.close())
  const ctx = await boot(root, http.url, protocol)
  await register(ctx, 'search')
  const agent = await ctx.agentLoop.create(SessionId('dynamic'), { provider: 'deepseek-official', model: MODEL })
  await send(agent, 'first')
  const fetchPlugin = await register(ctx, 'fetch')
  await send(agent, 'add')
  await fetchPlugin.dispose()
  expect(ctx.tools.schemas().map(tool => tool.name)).not.toContain('fetch')
  await send(agent, 'remove')
  const history = agent.session.toolHistory()
  expect(history.updates).toHaveLength(2)
  await ctx.sessions.flush(agent.session)
  await ctx.fiber.dispose()
  contexts.splice(contexts.indexOf(ctx), 1)

  const resumed = await boot(root, http.url, protocol)
  await register(resumed, 'search')
  const handle = await resumed.agents.resume({
    resumeSessionId: SessionId('dynamic'), agentOptions: { provider: 'deepseek-official', model: MODEL },
  })
  expect(handle.agent.session.toolHistory()).toEqual(history)
  await send(handle.agent, 'resume unchanged')
  await register(resumed, 'fetch')
  await send(handle.agent, 're-add')
  expect(handle.agent.session.toolHistory().updates).toHaveLength(3)
  expect(http.requests).toHaveLength(5)
  const final = http.requests[4]!
  if (protocol === 'messages') {
    expect(http.requests[0]?.headers['anthropic-beta']).toBeUndefined()
    expect(final.headers['anthropic-beta']).toBe('mid-conversation-tool-changes-2026-07-01')
    expect(final.body.tools).toEqual([
      { name: 'search', description: 'search', input_schema: expect.anything() as object },
      { name: 'fetch', description: 'fetch', input_schema: expect.anything() as object, defer_loading: true },
    ])
    const changes = (final.body.messages as { content: { type: string }[] }[]).flatMap(message => message.content)
      .filter(block => block.type === 'tool_addition' || block.type === 'tool_removal')
    expect(changes).toEqual([
      { type: 'tool_addition', tool: { type: 'tool_reference', name: 'fetch' } },
      { type: 'tool_removal', tool: { type: 'tool_reference', name: 'fetch' } },
      { type: 'tool_addition', tool: { type: 'tool_reference', name: 'fetch' } },
    ])
    expect(http.requests[2]?.body.tools).toEqual(http.requests[3]?.body.tools)
  } else {
    expect(final.headers['anthropic-beta']).toBeUndefined()
    expect(JSON.stringify(final.body)).not.toMatch(/tool_addition|tool_removal|defer_loading/)
    expect((http.requests[2]?.body.tools as { function: { name: string } }[]).map(tool => tool.function.name)).toEqual(['search'])
  }
})
