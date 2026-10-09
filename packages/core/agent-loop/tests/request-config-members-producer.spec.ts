/** Structurally typed request middleware keeps its JSON members in the logged config. */
import { describe, expect, it } from 'vitest'
import { Context } from '@deepseek-ai/cordis'
import LlmRuntime, { createUserMessage } from 'bake-llm'
import type { LlmCallConfig, ToolSchema } from 'bake-llm'
import SessionStore, { SessionId } from 'bake-session'
import SessionProjectionRegistry from 'bake-session-projection'
import SystemPrompt from 'bake-system-prompt'
import ToolRuntime, { defineContentToolFixture } from 'bake-tools'
import AgentRegistry from 'bake-agent'
import AgentLoop from 'bake-agent-loop'
import { MockAdapter, textResponse } from './mock-adapter.ts'
import { encodeLog, replayRequests } from './runtime-fixture.ts'

interface PluginConfig extends LlmCallConfig {
  purpose: string
  extension: { nested: (string | { enabled: boolean })[] }
  messages: string[]
  toolHistory: { config: boolean }
  sessionId: string
  tools: ToolSchema[]
  signal: { aborted: boolean; reason: { kind: string } }
}

const configuredTools: ToolSchema[] = [{
  name: 'configured', description: 'from config', parameters: { type: 'object' },
}]

describe('request config producer', () => {
  for (const hasHeaderTools of [false, true]) {
    it(`logs middleware members with header tools ${hasHeaderTools ? 'present' : 'absent'}`, async () => {
      const ctx = new Context()
      const adapter = new MockAdapter([textResponse('done')])
      try {
        await ctx.plugin(LlmRuntime)
        await ctx.plugin(SessionStore)
        await ctx.plugin(SessionProjectionRegistry)
        await ctx.plugin(SystemPrompt, { personaPrefix: 'stable base', includeHarnessIdentity: false })
        await ctx.plugin(ToolRuntime)
        await ctx.plugin(AgentRegistry)
        await ctx.plugin(AgentLoop, { agents: [] })
        ctx.effect(() => ctx.llm.registerAdapter(['mock'], adapter))
        if (hasHeaderTools) ctx.effect(() => ctx.tools.register(defineContentToolFixture({
          name: 'echo', description: 'echo back', parameters: { text: { type: 'string' } },
          async execute() { return [{ type: 'text', text: 'unused' }] },
        })))
        let turnSignal: AbortSignal | undefined
        let replacement: PluginConfig | undefined
        // Returning a typed variable establishes ordinary structural assignability, without a cast.
        ctx.on('agent/request', async ({ signal }, next): Promise<LlmCallConfig> => {
          turnSignal = signal
          replacement = {
            ...await next(), purpose: 'plugin-purpose', extension: { nested: ['kept', { enabled: true }] },
            messages: ['config-message'], toolHistory: { config: true }, sessionId: 'config-session',
            tools: configuredTools, signal: { aborted: false, reason: { kind: 'logged-signal' } },
          }
          return replacement
        })
        const errors: unknown[] = []
        ctx.on('agent/error', ({ error }) => { errors.push(error) })
        const id = SessionId(hasHeaderTools ? 'config-header-tools' : 'config-no-header-tools')
        const agent = await ctx.agentLoop.create(id, { provider: 'mock', model: 'mock' })
        agent.followup(createUserMessage({ content: [{ type: 'text', text: 'go' }], source: { kind: 'user' } }))
        await agent.whenIdle()
        expect(errors).toEqual([])
        expect(adapter.requests).toHaveLength(1)
        const request = adapter.requests[0]!
        const events = agent.session.snapshotEvents()
        const headerEvents = events.filter(event => event.type === 'request/header')
        expect(headerEvents).toHaveLength(1)
        const header = headerEvents[0]!.data.header
        expect(header.config).toStrictEqual(replacement)
        expect(header.config).not.toBe(replacement)
        expect(agent.session.requestHeader()?.config).toStrictEqual(replacement)
        expect(request).toMatchObject({ purpose: 'plugin-purpose', extension: replacement!.extension })
        expect(request.messages.map(message => message.content)).toStrictEqual([
          [{ type: 'text', text: 'stable base' }], [{ type: 'text', text: 'go' }],
        ])
        expect(request.sessionId).toBe(id)
        expect(request.signal).toBe(turnSignal)
        expect(request.signal).toBeInstanceOf(AbortSignal)
        const schemas = header.tools ?? []
        expect(schemas.map(tool => tool.name)).toStrictEqual(hasHeaderTools ? ['echo'] : [])
        expect(request.toolHistory).toStrictEqual({ tools: schemas, updates: [] })
        expect(request.tools).toStrictEqual(hasHeaderTools ? schemas : configuredTools)

        const log = Buffer.from(encodeLog(agent.session.header, events))
        const physical = log.toString('utf8').trimEnd().split('\n').map(line => JSON.parse(line) as { type: string; data?: { header?: { config?: unknown } } })
        expect(physical.find(row => row.type === 'request/header')?.data?.header?.config).toStrictEqual(replacement)
        const replayed = replayRequests(log)
        expect(replayed).toHaveLength(1)
        // Offline reconstruction has no process-local signal to overwrite the logged JSON member.
        expect(replayed[0]!.signal).toStrictEqual(replacement!.signal)
        expect(replayed[0]!.extension).toStrictEqual(replacement!.extension)
        expect(replayed[0]!.tools).toStrictEqual(request.tools)
      } finally {
        await ctx.fiber.dispose()
      }
    })
  }
})
