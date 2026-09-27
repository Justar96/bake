/** Dynamic tool records and route projection through the real loop. */
import { afterEach, describe, expect, it } from 'vitest'
import { Context } from '@deepseek-ai/cordis'
import AgentRegistry, { type Agent } from '@deepseek-ai/dsh-agent'
import AgentLoop from '@deepseek-ai/dsh-agent-loop'
import LlmRuntime, { createUserMessage } from '@deepseek-ai/dsh-llm'
import SessionStore, { SessionId } from '@deepseek-ai/dsh-session'
import SessionProjectionRegistry from '@deepseek-ai/dsh-session-projection'
import SystemPrompt from '@deepseek-ai/dsh-system-prompt'
import ToolRuntime, { defineContentToolFixture } from '@deepseek-ai/dsh-tools'
import { MockAdapter, textResponse, toolCallResponse } from './mock-adapter.ts'

const contexts: Context[] = []
afterEach(async () => {
  for (const ctx of contexts.splice(0)) await ctx.fiber.dispose()
})
async function harness(adapter: MockAdapter) {
  const ctx = new Context()
  contexts.push(ctx)
  await ctx.plugin(LlmRuntime)
  await ctx.plugin(SessionStore)
  await ctx.plugin(SessionProjectionRegistry)
  await ctx.plugin(SystemPrompt, { personaPrefix: '', personaSuffix: '' })
  await ctx.plugin(ToolRuntime)
  await ctx.plugin(AgentRegistry)
  await ctx.plugin(AgentLoop, { agents: [] })
  ctx.llm.registerAdapter(['mock'], adapter)
  const agent = await ctx.agentLoop.create(SessionId('tool-updates'), { provider: 'mock', model: 'model' })
  return { ctx, agent }
}
async function send(agent: Agent, text: string) {
  agent.followup(createUserMessage({ source: { kind: 'user' }, content: [{ type: 'text', text }] }))
  await agent.whenIdle()
}
function tool(ctx: Context, name: string, description = name) {
  return ctx.tools.register(defineContentToolFixture({
    name, description, parameters: {}, execute: async () => [{ type: 'text', text: 'done' }],
  }))
}

describe('dynamic tool loop', () => {
  it('records changes within a tool turn when a later system update follows its result', async () => {
    const adapter = new MockAdapter([toolCallResponse('install-call', 'install', {}), textResponse('ready')])
    adapter.toolUpdate = 'in-history'
    adapter.systemPromptUpdate = 'in-history'
    const { ctx, agent } = await harness(adapter)
    ctx.systemPrompt.section({ name: 'test:tools', order: 0, text: () => ctx.tools.schemas().map(schema => schema.name).join(', ') })
    ctx.tools.register(defineContentToolFixture({
      name: 'install', description: 'install fetch', parameters: {},
      execute: async () => {
        tool(ctx, 'fetch')
        return [{ type: 'text', text: 'installed' }]
      },
    }))
    await send(agent, 'install fetch')
    expect(adapter.requests).toHaveLength(2)
    const second = adapter.requests[1]!
    expect(second.messages.at(-1)?.role).toBe('system')
    const result = second.messages.find(message => message.source.kind === 'tool')!
    expect(second.toolUpdates).toEqual([{ afterMessageId: result.id, additions: ['fetch'], removals: [] }])
    expect(agent.session.snapshotEvents().findLast(event => event.type === 'turn/end')?.data.reason).toEqual({ kind: 'completed' })
  })

  it.each([undefined, 'in-history', 'addition-only'] as const)('logs required changes with route mode %s', async (mode) => {
    const adapter = new MockAdapter(Array.from({ length: 5 }, () => textResponse('ok')))
    if (mode !== undefined) adapter.toolUpdate = mode
    const { ctx, agent } = await harness(adapter)
    tool(ctx, 'search')
    await send(agent, 'first')
    const disposeFetch = tool(ctx, 'fetch')
    await send(agent, 'second')
    disposeFetch()
    await send(agent, 'third')
    tool(ctx, 'fetch')
    await send(agent, 'fourth')
    await send(agent, 'unchanged')
    const updates = agent.session.snapshotEvents().filter(event => event.type === 'request/tool-update')
    expect(updates.map(event => [event.data.additions, event.data.removals])).toEqual([
      [['fetch'], []], [[], ['fetch']], [['fetch'], []],
    ])
    for (const event of updates) {
      expect(event).not.toHaveProperty('ignorable')
      expect(event).not.toHaveProperty('surfaceOp')
    }
    const last = adapter.requests.at(-1)!
    expect(last.toolHistory).toEqual(agent.session.toolHistory())
    expect(Object.isFrozen(last.toolHistory?.updates)).toBe(true)
    if (mode === undefined) {
      expect(last.toolUpdates).toBeUndefined()
      expect(last.tools?.some(schema => schema.deferLoading)).toBe(false)
    } else {
      expect(last.tools?.map(schema => [schema.name, schema.deferLoading])).toEqual([['search', undefined], ['fetch', true]])
      expect(last.toolUpdates).toHaveLength(mode === 'in-history' ? 3 : 1)
    }
    expect(agent.session.requestHeader()?.tools?.every(schema => !('deferLoading' in schema))).toBe(true)
  })

  it('starts new declarations when a removed tool returns with a changed definition', async () => {
    const adapter = new MockAdapter(Array.from({ length: 4 }, () => textResponse('ok')))
    adapter.toolUpdate = 'in-history'
    const { ctx, agent } = await harness(adapter)
    tool(ctx, 'search')
    await send(agent, 'first')
    const dispose = tool(ctx, 'fetch')
    await send(agent, 'add')
    dispose()
    await send(agent, 'remove')
    tool(ctx, 'fetch', 'new definition')
    await send(agent, 'changed')
    expect(adapter.requests.at(-1)?.toolUpdates).toBeUndefined()
    expect(adapter.requests.at(-1)?.tools?.find(schema => schema.name === 'fetch')).toMatchObject({ description: 'new definition' })
    expect(adapter.requests.at(-1)?.tools?.some(schema => schema.deferLoading)).toBe(false)
    expect(agent.session.toolHistory().updates).toEqual([])
  })
})
