/// <reference types="node" />
/// <reference lib="es2024.promise" />
/** Real Loader composition: route admission, durable decisions, and cancellation. */

import { createServer } from 'node:http'
import type { Server, ServerResponse } from 'node:http'
import { mkdtemp, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { pathToFileURL } from 'node:url'
import { afterEach, describe, expect, it } from 'vitest'
import { Context } from '@deepseek-ai/cordis'
import Loader from '@deepseek-ai/cordis-plugin-loader'
import Include from '@deepseek-ai/cordis-plugin-include'
import AgentRegistry from '@deepseek-ai/dsh-agent'
import type { Agent } from '@deepseek-ai/dsh-agent'
import AgentLoop from '@deepseek-ai/dsh-agent-loop'
import LlmRuntime, { createUserMessage, ReasoningEffortId } from '@deepseek-ai/dsh-llm'
import { bindScopeParent, createScope, scopeOf } from '@deepseek-ai/dsh-scope'
import SessionStore, { SessionId } from '@deepseek-ai/dsh-session'
import SessionProjectionRegistry from '@deepseek-ai/dsh-session-projection'
import SystemPrompt from '@deepseek-ai/dsh-system-prompt'
import ToolRuntime from '@deepseek-ai/dsh-tools'
import SubagentRuntime from '@deepseek-ai/dsh-subagent'
import * as Spawn from '@deepseek-ai/dsh-subagent-spawn-in-process'
import * as ToolSubagent from '../src/index.ts'
import SubagentModelSelection from '../src/model-selection-settings.ts'
import { MockAdapter, textResponse, toolCallResponse } from '../../../core/agent-loop/tests/mock-adapter.ts'

const ALLOWED = [{ provider: 'route-test', model: 'parent' }, { provider: 'route-test', model: 'selected' }]
const REASONING = { efforts: ['low', 'high'].map(id => ({ id: ReasoningEffortId(id), name: id })) }
const ANSWER = {
  provider: 'route-test', model: 'selected', reasoning_effort: 'high', reason: 'Integration checks need stronger reasoning.',
  fallback: false, routing: { policy: '2026-10-01', status: 'normal', difficulty: 0.74, reasons: [] },
}

let owner: Context | undefined
let directory: string | undefined
let server: Server | undefined

afterEach(async () => {
  await owner?.fiber.dispose()
  owner = undefined
  if (server !== undefined) {
    server.closeAllConnections()
    await new Promise<void>((resolve) => { server!.close(() => { resolve() }) })
    server = undefined
  }
  if (directory !== undefined) await rm(directory, { recursive: true, force: true })
  directory = undefined
})

async function localRouter(reply: (response: ServerResponse) => void) {
  const received = Promise.withResolvers<Record<string, unknown>>()
  server = createServer((request, response) => {
    let body = ''
    request.on('data', (chunk: Buffer) => { body += chunk.toString() })
    request.on('end', () => {
      expect(request.url).toBe('/v1/bake/select')
      received.resolve(JSON.parse(body) as Record<string, unknown>)
      reply(response)
    })
  })
  await new Promise<void>((resolve) => { server!.listen(0, '127.0.0.1', resolve) })
  const address = server.address()
  if (address === null || typeof address === 'string') throw new Error('router did not bind a TCP port')
  return { url: `http://127.0.0.1:${address.port}`, received: received.promise }
}

/** The YAML mounts every shipping runtime component; only model and HTTP answers are scripted. */
async function load(url: string, cancel = false) {
  directory = await mkdtemp(join(tmpdir(), 'bake-routing-loader-'))
  const path = join(directory, 'cordis.yml')
  const modules = new Map<string, unknown>([
    ['@deepseek-ai/dsh-llm', LlmRuntime],
    ['@deepseek-ai/dsh-session', SessionStore],
    ['@deepseek-ai/dsh-session-projection', SessionProjectionRegistry],
    ['@deepseek-ai/dsh-system-prompt', SystemPrompt],
    ['@deepseek-ai/dsh-tools', ToolRuntime],
    ['@deepseek-ai/dsh-agent', AgentRegistry],
    ['@deepseek-ai/dsh-agent-loop', AgentLoop],
    ['@deepseek-ai/dsh-subagent', SubagentRuntime],
    ['@deepseek-ai/dsh-subagent-spawn-in-process', Spawn],
    ['@deepseek-ai/dsh-tool-subagent/model-selection-settings', SubagentModelSelection],
    ['@deepseek-ai/dsh-tool-subagent', ToolSubagent],
  ])
  const entries = [...modules.keys()].map(name => ({ name, ...name === '@deepseek-ai/dsh-tool-subagent' ? {
    config: { provider: 'spawn', modelSelectionSettings: true, enableRunInBackground: false },
  } : name === '@deepseek-ai/dsh-tool-subagent/model-selection-settings' ? {
    config: { enabled: true, allowedModels: ALLOWED, router: { enabled: true, url, timeoutMs: 5000 } },
  } : name === '@deepseek-ai/dsh-agent-loop' ? { config: { agents: [] } } : {} }))
  // JSON is valid YAML and leaves the test-owned endpoint and paths unambiguous.
  await writeFile(path, JSON.stringify(entries))
  owner = new Context()
  owner.baseUrl = `${pathToFileURL(directory).href}/`
  const composition = createScope(owner, { name: 'routing-composition' })
  const ctx = composition.ctx
  await ctx.plugin(Loader)
  const loader = ctx.get('loader')!
  loader.builtins.include = Include
  loader.internal = {
    version: 'v2',
    async import(specifier: string) {
      if (!modules.has(specifier)) throw new Error(`unexpected Loader import: ${specifier}`)
      return modules.get(specifier)
    },
  } as unknown as NonNullable<typeof loader.internal>
  await loader.create({ name: 'cordis:include', config: { path: pathToFileURL(path).href } })
  await loader.await()
  expect([...loader.entries()].filter(entry => entry.fiber === undefined && !entry.disabled)
    .map(entry => entry.options.name)).toEqual([])
  const adapter = new MockAdapter([
    toolCallResponse('route-call', 'subagent', { description: 'Check routing', prompt: 'Return CHILD_OK.' }),
    ...cancel ? [] : [textResponse('CHILD_OK'), textResponse('PARENT_OK')],
  ], REASONING)
  ctx.get('llm')!.registerAdapter(['route-test'], adapter)
  const children: Agent[] = []
  owner.on('agent/created', ({ agent }) => { if (agent.session.header.origin === 'subagent') children.push(agent) })
  const handle = await ctx.get('agents')!.create({
    sessionId: SessionId('routing-parent'), agentOptions: { provider: 'route-test', model: 'parent' },
    setup(agentCtx) { bindScopeParent(scopeOf(agentCtx)!, scopeOf(composition.ctx)!) },
  })
  return { ctx, parent: handle.agent, adapter, children }
}

function begin(parent: Agent): void {
  parent.followup(createUserMessage({ source: { kind: 'user' }, content: [{ type: 'text', text: 'Delegate the check.' }] }))
}

describe('automatic routing through a real Loader composition', () => {
  it.each(['normal', 'cautious', 'needs_context'] as const)('records %s without changing model-visible output', async (status) => {
    const fallback = status === 'needs_context'
    const reasons = status === 'cautious' ? ['limited benchmark support']
      : fallback ? ['missing conversation context'] : []
    const router = await localRouter((response) => {
      response.writeHead(200, { 'content-type': 'application/json' }).end(JSON.stringify({
        ...ANSWER, fallback, routing: { ...ANSWER.routing, status, reasons },
      }))
    })
    const { parent, adapter, children } = await load(router.url)
    begin(parent)
    await parent.whenIdle()
    const sent = await router.received
    expect((sent['allowed_models'] as object[]).map((route) => {
      const { provider, model } = route as { provider: string; model: string }
      return { provider, model }
    })).toEqual(ALLOWED)
    expect(children).toHaveLength(1)
    expect(adapter.requests[1]).toMatchObject({ provider: 'route-test', model: fallback ? 'parent' : 'selected' })
    if (!fallback) expect(adapter.requests[1]?.reasoningEffort).toBe('high')
    const decisions = parent.session.snapshotEvents().filter(event => event.type === 'subagent/routing-decision')
    expect(decisions).toHaveLength(1)
    expect(decisions[0]?.data).toMatchObject({
      childId: children[0]?.id, callId: 'route-call', source: fallback ? 'fallback' : 'auto',
      route: { provider: 'route-test', model: fallback ? 'parent' : 'selected' },
      router: { fallback, assessment: { status, difficulty: 0.74, reasons } },
    })
    if (!fallback) expect(decisions[0]?.data.route?.reasoningEffort).toBe('high')
    const messages = parent.session.deriveMessages()
    expect(JSON.stringify(messages)).toContain('CHILD_OK')
    expect(JSON.stringify(messages)).not.toContain(ANSWER.reason)
    expect(JSON.stringify(messages)).not.toContain('subagent/routing-decision')
    expect(messages.at(-1)?.content).toEqual([{ type: 'text', text: 'PARENT_OK' }])
  })

  it('cancellation settles before a delayed response can admit a child or log a decision', async () => {
    let reply: ServerResponse | undefined
    const router = await localRouter((response) => { reply = response })
    const { parent, adapter, children } = await load(router.url, true)
    begin(parent)
    await router.received
    parent.cancel({ kind: 'user' })
    await parent.whenIdle()
    reply!.writeHead(200, { 'content-type': 'application/json' }).end(JSON.stringify(ANSWER))
    expect(children).toEqual([])
    expect(adapter.requests).toHaveLength(1)
    expect(parent.session.snapshotEvents().filter(event => event.type === 'subagent/routing-decision')).toEqual([])
  })

  it('a router cannot send the real child outside the session allowlist', async () => {
    const router = await localRouter((response) => {
      response.writeHead(200, { 'content-type': 'application/json' }).end(JSON.stringify({ ...ANSWER, model: 'not-allowed' }))
    })
    const { parent, adapter, children } = await load(router.url)
    begin(parent)
    await parent.whenIdle()
    expect(children).toHaveLength(1)
    expect(adapter.requests[1]).toMatchObject({ provider: 'route-test', model: 'parent' })
    const decisions = parent.session.snapshotEvents().filter(event => event.type === 'subagent/routing-decision')
    expect(decisions).toHaveLength(1)
    expect(decisions[0]?.data).toMatchObject({ source: 'fallback', route: { provider: 'route-test', model: 'parent' },
      router: { fallback: true, reason: 'Router unavailable; default route retained.' } })
  })

})
