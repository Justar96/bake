import { afterEach, describe, expect, it, vi } from 'vitest'
import { mkdtempSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { Context } from '@deepseek-ai/cordis'
import type { Agent } from 'bake-agent'
import { ToolCallId, createUserMessage } from 'bake-llm'
import AgentLoop from 'bake-agent-loop'
import { mountAgentLoopTestDependencies } from 'bake-agent-loop-testkit'
import { SessionId } from 'bake-session'
import JsonlSessionPersistence from 'bake-session-persistence-jsonl'
import SubagentRuntime from 'bake-subagent'
import * as SubagentSpawn from 'bake-subagent-spawn-in-process'
import type { GenerateOptions, StreamChunk } from 'bake-llm'
import { LlmAdapter } from 'bake-llm'
import { MockAdapter, textResponse } from '../../../core/agent-loop/tests/mock-adapter.ts'
import * as tool from '../src/index.ts'
import { parkParent } from './park-parent.ts'
import { TestSessionQuery } from './test-session-query.ts'
import { loadStoredSession } from '../../subagent/tests/persistence-helpers.ts'

/** One scripted response that may wait on a caller-released gate before streaming. */
interface GatedEntry {
  chunks: StreamChunk[]
  gate?: Promise<undefined>
}

/** Adapter whose entries can hold a model call open until the test releases it. */
class GatedAdapter extends LlmAdapter {
  readonly requests: GenerateOptions[] = []

  constructor(private script: GatedEntry[]) {
    super()
  }

  async * stream(options: GenerateOptions): AsyncIterable<StreamChunk> {
    this.requests.push(options)
    const entry = this.script.shift()
    if (!entry) throw new Error('GatedAdapter: script exhausted')
    if (entry.gate) await entry.gate
    for (const chunk of entry.chunks) {
      if (options.signal?.aborted) throw new Error('aborted')
      yield chunk
    }
  }
}

const testToolSignal = new AbortController().signal

const roots: string[] = []
const contexts: Context[] = []
afterEach(async () => {
  // Dispose first: the persistence teardown closes every session handle and
  // releases its session.lock before the root disappears.
  for (const ctx of contexts.splice(0).reverse()) await ctx.fiber.dispose()
  for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 })
})

async function setupWith(adapter: MockAdapter | GatedAdapter, park = true) {
  const ctx = new Context()
  contexts.push(ctx)
  await mountAgentLoopTestDependencies(ctx)
  const root = mkdtempSync(join(tmpdir(), 'dsh-tool-subagent-control-'))
  roots.push(root)
  await ctx.plugin(JsonlSessionPersistence, { root })
  await ctx.plugin(TestSessionQuery)
  await ctx.plugin(AgentLoop, { agents: [] })
  await ctx.plugin(SubagentRuntime)
  await ctx.plugin(SubagentSpawn, { providerName: 'spawn' })
  await ctx.plugin(tool)
  ctx.llm.registerAdapter(['mock'], adapter)
  const parent = await ctx.agentLoop.create(SessionId('parent'), { provider: 'mock', model: 'mock' })
  if (park) parkParent(ctx, parent)
  return { ctx, parent, adapter }
}

async function setup(script: ConstructorParameters<typeof MockAdapter>[0]) {
  return setupWith(new MockAdapter(script))
}

function text(result: { content: { type: string; text?: string }[] }): string {
  return result.content.filter(block => block.type === 'text').map(block => block.text).join('')
}

let calls = 0
function callTool(
  ctx: Context,
  name: string,
  args: unknown,
  agent?: unknown,
  signal: AbortSignal = testToolSignal,
) {
  return ctx.tools.execute({
    signal,
    callId: ToolCallId(`call-${++calls}`),
    name,
    arguments: args,
    ...agent !== undefined ? { agent: agent as never } : {},
  })
}

/** Wait until a child's Activation released its handle. */
async function waitNoActivation(ctx: Context, childId: SessionId): Promise<void> {
  await vi.waitFor(() => {
    expect(ctx.agents.get(childId)).toBeUndefined()
  }, { timeout: 5_000 })
}

describe('bake-tool-subagent-control', () => {
  it('registers send_message once, globally, with the two required parameters', async () => {
    const { ctx } = await setup([])
    const schemas = ctx.tools.schemas().filter(schema => schema.name === 'send_message')
    expect(schemas).toHaveLength(1)
    const props = (schemas[0]!.parameters as { properties?: Record<string, unknown> }).properties ?? {}
    expect(Object.keys(props).sort()).toEqual(['agent_id', 'message'])
    // The continuable path has no Task, so the schema must not promise one.
    expect(schemas[0]!.description).not.toContain('job_output')
    expect(schemas[0]!.description).not.toContain('job id')
    expect(schemas[0]!.description).toBe(
      'Message a direct continuable subagent, or your parent if you are one. A busy target reads it next '
      + 'step; an idle one starts a new turn. Confirms delivery; no reply.',
    )
    // Both parameters are self-describing.
    expect(props.agent_id).not.toHaveProperty('description')
    expect(props.message).not.toHaveProperty('description')
  })

  it('titles a message by its target and first line, and an interrupt by its target', async () => {
    const { ctx } = await setup([])
    const send = ctx.tools.get('send_message')!
    const id = 'session-a382bf6a-a89d-48aa-babd-18fa894e64ed'
    expect(send.presentCall?.({ agent_id: id, message: 'Also check the lockfile.' }))
      .toEqual({ card: 'generic', title: `${id}: Also check the lockfile.`, kind: 'other' })
    // Later lines and a long first line are marked as left out, never shown.
    expect(send.presentCall?.({ agent_id: 'a1', message: '\n  Also check\tthe lockfile.\n\nThen the tests.' }))
      .toEqual({ card: 'generic', title: 'a1: Also check the lockfile. \u2026', kind: 'other' })
    expect(send.presentCall?.({ agent_id: 'a1', message: `${'word '.repeat(30)}\nmore` }))
      .toEqual({ card: 'generic', title: `a1: ${'word '.repeat(10).slice(0, 47).trimEnd()}\u2026`, kind: 'other' })
    expect(send.presentCall?.({ agent_id: 'a1', message: ' ' })).toEqual({ card: 'generic', title: 'a1', kind: 'other' })
    expect(ctx.tools.get('interrupt_agent')?.presentCall?.({ agent_id: 'a1' }))
      .toEqual({ card: 'generic', title: 'Interrupt a1', kind: 'execute' })
    // Obsolete or invalid logged arguments keep the generic rendering.
    expect(send.presentCall?.({ target: 'a1', message: 'hi' })).toBeUndefined()
    expect(ctx.tools.get('interrupt_agent')?.presentCall?.({ agent_id: 1 })).toBeUndefined()
  })

  it('JSON-encodes a caller-supplied parent id in the initial return instruction', async () => {
    const { ctx } = await setup([textResponse('child done')])
    const parent = await ctx.agentLoop.create(SessionId('parent"\nagent'), { provider: 'mock', model: 'mock' })
    parkParent(ctx, parent)
    const started = await ctx.subagents.startContinuable({
      provider: 'spawn',
      label: 'encoded parent',
      request: { prompt: [{ type: 'text', text: 'encoded task' }], parent },
      signal: testToolSignal,
    })
    await waitNoActivation(ctx, started.childId)
    const loaded = await loadStoredSession(ctx.sessionPersistence, started.childId)
    const prompt = loaded.events.find(event => event.type === 'user/message'
      && event.data.content.some(block => block.type === 'text' && block.text === 'encoded task'))
    if (prompt?.type !== 'user/message') throw new Error('expected the encoded initial task')
    const guidance = prompt.data.content.findLast(block => block.type === 'text')?.text ?? ''

    expect(guidance).toContain(`Your parent agent id is ${JSON.stringify(parent.id)}`)
    expect(guidance).toContain(`agent_id: ${JSON.stringify(parent.id)}`)
    expect(guidance).not.toContain(parent.id)
  })

  it('lets a continuable child steer its direct parent with send_message', async () => {
    const release = Promise.withResolvers<undefined>()
    const { ctx, parent, adapter } = await setupWith(new GatedAdapter([
      { chunks: textResponse('child done'), gate: release.promise },
    ]))
    const started = await ctx.subagents.startContinuable({
      provider: 'spawn',
      label: 'child task',
      request: { prompt: [{ type: 'text', text: 'child task' }], parent },
      signal: testToolSignal,
    })
    await vi.waitFor(() => { expect(adapter.requests).toHaveLength(1) })
    const child = ctx.agents.get(started.childId)
    if (child === undefined) throw new Error('expected a live child')
    const delivered: Array<{ agent: Agent; message: ReturnType<typeof createUserMessage> }> = []
    ctx.on('agent/inbox/inserted', ({ agent, message }) => {
      if (agent === parent && message.source.kind === 'agent-message') delivered.push({ agent, message })
    })

    const result = await callTool(ctx, 'send_message', {
      agent_id: parent.id,
      message: 'CHILD_FINDING',
    }, child)

    expect(result.isError).toBe(false)
    expect(text(result)).toBe(`message delivered to agent ${parent.id}`)
    expect(delivered).toHaveLength(1)
    expect(delivered[0]?.message.source).toEqual({
      kind: 'agent-message',
      form: 'relay',
      senderSessionId: started.childId,
    })
    expect(delivered[0]?.message.content).toEqual([
      { type: 'text', text: `Agent ${started.childId} sent a message: ` },
      { type: 'text', text: 'CHILD_FINDING' },
    ])

    release.resolve(undefined)
    await waitNoActivation(ctx, started.childId)
  })

  it('cold-resumes a settled child and reports delivery', async () => {
    const { ctx, parent } = await setup([textResponse('first answer'), textResponse('second answer')])
    const started = await ctx.subagents.startContinuable({
      provider: 'spawn',
      label: 'child task',
      request: { prompt: [{ type: 'text', text: 'child task' }], parent },
      signal: testToolSignal,
    })
    await waitNoActivation(ctx, started.childId)

    const result = await callTool(ctx, 'send_message', {
      agent_id: started.childId,
      message: 'and then?',
    }, parent)

    expect(result.isError).toBe(false)
    expect(text(result)).toBe(`message delivered to agent ${started.childId}`)
    await waitNoActivation(ctx, started.childId)

    const loaded = await loadStoredSession(ctx.sessionPersistence, started.childId)
    const followUp = loaded.events.findLast(event => event.type === 'user/message')
    // The durable message source records the calling agent without granting authority.
    expect(followUp?.type === 'user/message' && followUp.data.source).toEqual({
      kind: 'agent-message',
      form: 'relay',
      senderSessionId: parent.id,
    })
    expect(followUp?.type === 'user/message' && followUp.data.content).toEqual([
      { type: 'text', text: `Agent ${parent.id} sent a message: ` },
      { type: 'text', text: 'and then?' },
    ])
  })

  it('steers the nearest step of an open turn', async () => {
    const { ctx, parent, adapter } = await setup([textResponse('first'), textResponse('second')])
    const started = await ctx.subagents.startContinuable({
      provider: 'spawn',
      label: 'long work',
      request: { prompt: [{ type: 'text', text: 'long work' }], parent },
      signal: testToolSignal,
    })
    await vi.waitFor(() => { expect(adapter.requests).toHaveLength(1) })

    const result = await callTool(ctx, 'send_message', {
      agent_id: started.childId,
      message: 'also consider Y',
    }, parent)
    expect(result.isError).toBe(false)

    await waitNoActivation(ctx, started.childId)
    const loaded = await loadStoredSession(ctx.sessionPersistence, started.childId)
    const prompts = loaded.events.flatMap(event => event.type === 'user/message' && event.data.source.kind !== 'plugin'
      ? event.data.content.flatMap(block => block.type === 'text'
        && !block.text.startsWith('Your parent agent id is ')
        ? [block.text]
        : [])
      : [])
    expect(prompts).toEqual([
      'long work',
      `Agent ${parent.id} sent a message: `,
      'also consider Y',
    ])
  })

  it('reports a delivery failure as an errored, not-delivered result', async () => {
    const { ctx, parent } = await setup([])
    const result = await callTool(ctx, 'send_message', {
      agent_id: 'no-such-child',
      message: 'hello?',
    }, parent)
    expect(result.isError).toBe(true)
    expect(text(result)).toContain('unavailable')
  })

  it('rejects a caller that is not the child\'s durable direct parent', async () => {
    const { ctx, parent } = await setup([textResponse('first')])
    const started = await ctx.subagents.startContinuable({
      provider: 'spawn',
      label: 'child task',
      request: { prompt: [{ type: 'text', text: 'child task' }], parent },
      signal: testToolSignal,
    })
    await waitNoActivation(ctx, started.childId)
    const stranger = await ctx.agentLoop.create(SessionId('stranger'), { provider: 'mock', model: 'mock' })

    const result = await callTool(ctx, 'send_message', {
      agent_id: started.childId,
      message: 'mine now',
    }, stranger)
    expect(result.isError).toBe(true)
    expect(text(result)).toContain('another parent session')
  })

  it('fails loud when invoked without a calling agent', async () => {
    const { ctx } = await setup([])
    const result = await callTool(ctx, 'send_message', { agent_id: 'x', message: 'y' })
    expect(result.isError).toBe(true)
    expect(text(result)).toContain('requires a calling agent')
  })

  it('unregisters with its plugin fiber (HMR safety)', async () => {
    const ctx = new Context()
    contexts.push(ctx)
    await mountAgentLoopTestDependencies(ctx)
    await ctx.plugin(AgentLoop, { agents: [] })
    await ctx.plugin(SubagentRuntime)
    const fiber = await ctx.plugin(tool)
    expect(ctx.tools.schemas().some(schema => schema.name === 'send_message')).toBe(true)
    expect(ctx.tools.schemas().some(schema => schema.name === 'interrupt_agent')).toBe(true)
    await fiber.dispose()
    expect(ctx.tools.schemas().some(schema => schema.name === 'send_message')).toBe(false)
    expect(ctx.tools.schemas().some(schema => schema.name === 'interrupt_agent')).toBe(false)
  })

  it('has the namespace-plugin export shape (no stray default)', () => {
    expect('default' in tool).toBe(false)
    expect(tool.name).toBe('tool-subagent-control')
    expect(tool.inject).toEqual(['tools', 'subagents'])
    expect(typeof tool.apply).toBe('function')
  })
})

describe('bake-tool-subagent-control interrupt_agent', () => {
  it('registers interrupt_agent with the single agent_id parameter and current-turn wording', async () => {
    const { ctx } = await setup([])
    const schemas = ctx.tools.schemas().filter(schema => schema.name === 'interrupt_agent')
    expect(schemas).toHaveLength(1)
    const props = (schemas[0]!.parameters as { properties?: Record<string, unknown> }).properties ?? {}
    expect(Object.keys(props)).toEqual(['agent_id'])
    expect(schemas[0]!.description).toBe(
      'Request a stop of a continuable subagent\'s current turn; it stays available, and its own '
      + 'subagents keep running.',
    )
    expect(props.agent_id).not.toHaveProperty('description')
  })

  it('interrupts a running direct child with the parent cause, parking its queue', async () => {
    const releaseFirst = Promise.withResolvers<undefined>()
    const adapter = new GatedAdapter([
      { chunks: textResponse('held'), gate: releaseFirst.promise },
      { chunks: textResponse('parked answer') },
      { chunks: textResponse('waking answer') },
    ])
    const { ctx, parent } = await setupWith(adapter)
    const started = await ctx.subagents.startContinuable({
      provider: 'spawn',
      label: 'long work',
      request: { prompt: [{ type: 'text', text: 'long work' }], parent },
      signal: testToolSignal,
    })
    await vi.waitFor(() => { expect(adapter.requests).toHaveLength(1) })
    const child = ctx.agents.get(started.childId)!
    const queued = await callTool(ctx, 'send_message', {
      agent_id: started.childId,
      message: 'parked follow-up',
    }, parent)
    expect(queued.isError).toBe(false)
    const cancelSpy = vi.spyOn(child, 'cancel')

    const result = await callTool(ctx, 'interrupt_agent', { agent_id: started.childId }, parent)

    expect(result.isError).toBe(false)
    expect(text(result)).toBe(`interrupt requested for agent ${started.childId}`)
    expect(cancelSpy).toHaveBeenCalledExactlyOnceWith({ kind: 'parent' }, { keepInbox: true })
    releaseFirst.resolve(undefined)
    await child.whenIdle()
    // Parked, not resumed: the steering waits for another waking send.
    expect(adapter.requests).toHaveLength(1)
    expect(child.inbox.nextStep).toHaveLength(1)

    const waking = await callTool(ctx, 'send_message', {
      agent_id: started.childId,
      message: 'wake up',
    }, parent)
    expect(waking.isError).toBe(false)
    await waitNoActivation(ctx, started.childId)
    const loaded = await loadStoredSession(ctx.sessionPersistence, started.childId)
    const prompts = loaded.events.flatMap(event => event.type === 'user/message' && event.data.source.kind !== 'plugin'
      ? event.data.content.flatMap(block => block.type === 'text'
        && !block.text.startsWith('Your parent agent id is ')
        ? [block.text]
        : [])
      : [])
    expect(prompts).toEqual([
      'long work',
      `Agent ${parent.id} sent a message: `,
      'parked follow-up',
      `Agent ${parent.id} sent a message: `,
      'wake up',
    ])
  })

  it('lets a deep live ancestor interrupt a descendant it did not directly create', async () => {
    const releaseChild = Promise.withResolvers<undefined>()
    const releaseGrandchild = Promise.withResolvers<undefined>()
    const adapter = new GatedAdapter([
      { chunks: textResponse('child'), gate: releaseChild.promise },
      { chunks: textResponse('grandchild'), gate: releaseGrandchild.promise },
    ])
    const { ctx, parent } = await setupWith(adapter)
    const started = await ctx.subagents.startContinuable({
      provider: 'spawn',
      label: 'child',
      request: { prompt: [{ type: 'text', text: 'child work' }], parent },
      signal: testToolSignal,
    })
    await vi.waitFor(() => { expect(adapter.requests).toHaveLength(1) })
    const child = ctx.agents.get(started.childId)!
    const grandchild = await ctx.subagents.startContinuable({
      provider: 'spawn',
      label: 'grandchild',
      request: { prompt: [{ type: 'text', text: 'grandchild work' }], parent: child },
      signal: testToolSignal,
    })
    await vi.waitFor(() => { expect(adapter.requests).toHaveLength(2) })
    const grandchildAgent = ctx.agents.get(grandchild.childId)!
    const cancelSpy = vi.spyOn(grandchildAgent, 'cancel')

    const result = await callTool(ctx, 'interrupt_agent', { agent_id: grandchild.childId }, parent)

    expect(result.isError).toBe(false)
    expect(cancelSpy).toHaveBeenCalledExactlyOnceWith({ kind: 'parent' }, { keepInbox: true })
    releaseChild.resolve(undefined)
    releaseGrandchild.resolve(undefined)
    await waitNoActivation(ctx, grandchild.childId)
    await waitNoActivation(ctx, started.childId)
  })

  it('rejects self, sibling, and unrelated callers without touching the target', async () => {
    const releaseA = Promise.withResolvers<undefined>()
    const releaseB = Promise.withResolvers<undefined>()
    const adapter = new GatedAdapter([
      { chunks: textResponse('a'), gate: releaseA.promise },
      { chunks: textResponse('b'), gate: releaseB.promise },
    ])
    const { ctx, parent } = await setupWith(adapter)
    const target = await ctx.subagents.startContinuable({
      provider: 'spawn',
      label: 'target',
      request: { prompt: [{ type: 'text', text: 'a' }], parent },
      signal: testToolSignal,
    })
    await vi.waitFor(() => { expect(adapter.requests).toHaveLength(1) })
    const sibling = await ctx.subagents.startContinuable({
      provider: 'spawn',
      label: 'sibling',
      request: { prompt: [{ type: 'text', text: 'b' }], parent },
      signal: testToolSignal,
    })
    await vi.waitFor(() => { expect(adapter.requests).toHaveLength(2) })
    const targetAgent = ctx.agents.get(target.childId)!
    const siblingAgent = ctx.agents.get(sibling.childId)!
    const stranger = await ctx.agentLoop.create(SessionId('stranger'), { provider: 'mock', model: 'mock' })
    const cancelSpy = vi.spyOn(targetAgent, 'cancel')

    const self = await callTool(ctx, 'interrupt_agent', { agent_id: target.childId }, targetAgent)
    expect(self.isError).toBe(true)
    expect(text(self)).toContain('cannot interrupt itself')
    const fromSibling = await callTool(ctx, 'interrupt_agent', { agent_id: target.childId }, siblingAgent)
    expect(fromSibling.isError).toBe(true)
    expect(text(fromSibling)).toContain('not a live descendant')
    const fromStranger = await callTool(ctx, 'interrupt_agent', { agent_id: target.childId }, stranger)
    expect(fromStranger.isError).toBe(true)
    expect(text(fromStranger)).toContain('not a live descendant')
    expect(cancelSpy).not.toHaveBeenCalled()

    releaseA.resolve(undefined)
    releaseB.resolve(undefined)
    await waitNoActivation(ctx, target.childId)
    await waitNoActivation(ctx, sibling.childId)
  })

  it('accepts an absent target as a no-op without cold-resuming it', async () => {
    const { ctx, parent } = await setup([textResponse('done')])
    const started = await ctx.subagents.startContinuable({
      provider: 'spawn',
      label: 'settled child',
      request: { prompt: [{ type: 'text', text: 'child work' }], parent },
      signal: testToolSignal,
    })
    await waitNoActivation(ctx, started.childId)

    const settled = await callTool(ctx, 'interrupt_agent', { agent_id: started.childId }, parent)
    expect(settled.isError).toBe(false)
    expect(text(settled)).toBe(`interrupt requested for agent ${started.childId}`)
    const unknown = await callTool(ctx, 'interrupt_agent', { agent_id: 'no-such-agent' }, parent)
    expect(unknown.isError).toBe(false)
    // No cold resume: the settled target never rematerialized.
    expect(ctx.agents.get(started.childId)).toBeUndefined()
  })

  it('fails loud when invoked without a calling agent', async () => {
    const { ctx } = await setup([])
    const result = await callTool(ctx, 'interrupt_agent', { agent_id: 'x' })
    expect(result.isError).toBe(true)
    expect(text(result)).toContain('requires a calling agent')
  })
})
