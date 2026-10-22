/**
 * Process-local tool progress and per-call finish signals through the real
 * loop: coalescing, late-drop, finish order ahead of the ordered commit, and
 * that neither reaches the session log nor a model request.
 */

import { afterEach, describe, expect, it, vi } from 'vitest'
import { Context } from '@deepseek-ai/cordis'
import { createUserMessage, ToolCallId, type StreamChunk } from 'bake-llm'
import SessionStore, { SessionId } from 'bake-session'
import SystemPrompt from 'bake-system-prompt'
import LlmRuntime from 'bake-llm'
import ToolRuntime, { defineContentToolFixture, type ToolRunContext } from 'bake-tools'
import AgentRegistry, { TOOL_PROGRESS_MAX_CHARS, type Agent, type ToolProgress } from 'bake-agent'
import AgentLoop from 'bake-agent-loop'
import SessionProjectionRegistry from 'bake-session-projection'
import { boundProgress, ProgressThrottle } from '../src/tool-progress.ts'
import { MockAdapter, textResponse } from './mock-adapter.ts'

const contexts: Context[] = []
afterEach(async () => {
  vi.useRealTimers()
  for (const ctx of contexts.splice(0)) await ctx.fiber.dispose()
})

async function harness(adapter: MockAdapter) {
  const ctx = new Context()
  contexts.push(ctx)
  await ctx.plugin(LlmRuntime)
  await ctx.plugin(SessionStore)
  await ctx.plugin(SessionProjectionRegistry)
  await ctx.plugin(SystemPrompt, { personaPrefix: '' })
  await ctx.plugin(ToolRuntime)
  await ctx.plugin(AgentRegistry)
  await ctx.plugin(AgentLoop, { agents: [] })
  ctx.llm.registerAdapter(['mock'], adapter)
  return ctx
}

function calls(list: { id: string; name: string; args: object }[]): StreamChunk[] {
  return [
    ...list.flatMap((call, index): StreamChunk[] => [
      { type: 'block-start', index, blockType: 'tool-call' },
      { type: 'block-end', index, block: { type: 'tool-call', id: ToolCallId(call.id), name: call.name, arguments: JSON.stringify(call.args) } },
    ]),
    { type: 'finish', reason: { kind: 'tool-calls' } },
  ]
}

function whenIdle(ctx: Context, agent: Agent): Promise<void> {
  return new Promise((resolve) => {
    const dispose = ctx.on('agent/status', ({ agent: subject, status }) => {
      if (subject === agent && status === 'idle') { dispose(); resolve() }
    })
  })
}

async function until(predicate: () => boolean): Promise<void> {
  for (let i = 0; i < 1000 && !predicate(); i++) await new Promise(r => setTimeout(r, 0))
  if (!predicate()) throw new Error('until: condition never held')
}

/** Everything the loop published for the test's agent, in arrival order. */
function record(ctx: Context, agent: Agent) {
  const seen: Array<{ kind: 'progress'; callId: string; output: string } | { kind: 'executed'; callId: string; isError: boolean } | { kind: 'result'; callId: string }> = []
  ctx.on('agent/tool-progress', ({ agent: subject, callId, progress }) => {
    if (subject === agent) seen.push({ kind: 'progress', callId, output: progress.output })
  })
  ctx.on('agent/tool-executed', ({ agent: subject, callId, isError }) => {
    if (subject === agent) seen.push({ kind: 'executed', callId, isError })
  })
  ctx.on('session/event', (session, event) => {
    if (session === agent.session && event.type === 'tool/result') seen.push({ kind: 'result', callId: event.data.message.source.callId })
  })
  return seen
}

describe('ProgressThrottle', () => {
  it('publishes the first snapshot at once and coalesces the rest to the newest per interval', () => {
    vi.useFakeTimers()
    let clock = 0
    const published: string[] = []
    const throttle = new ProgressThrottle((progress) => { published.push(progress.output) }, 100, () => clock)
    throttle.push({ output: 'a' })
    throttle.push({ output: 'b' })
    throttle.push({ output: 'c' })
    expect(published).toEqual(['a'])
    clock = 100
    vi.advanceTimersByTime(100)
    expect(published).toEqual(['a', 'c'])
    clock = 150
    throttle.push({ output: 'd' })
    expect(published).toEqual(['a', 'c'])
    clock = 200
    vi.advanceTimersByTime(50)
    expect(published).toEqual(['a', 'c', 'd'])
  })

  it('drops a pending snapshot and every later one once closed', () => {
    vi.useFakeTimers()
    let clock = 0
    const published: string[] = []
    const throttle = new ProgressThrottle((progress) => { published.push(progress.output) }, 100, () => clock)
    throttle.push({ output: 'a' })
    throttle.push({ output: 'b' })
    throttle.close()
    clock = 500
    vi.advanceTimersByTime(500)
    throttle.push({ output: 'c' })
    expect(published).toEqual(['a'])
    expect(vi.getTimerCount()).toBe(0)
  })

  it('keeps the tail of an oversized snapshot without splitting a surrogate pair', () => {
    expect(boundProgress({ output: 'short' })).toEqual({ output: 'short' })
    const long = `head${'x'.repeat(TOOL_PROGRESS_MAX_CHARS)}`
    expect(boundProgress({ output: long }).output).toBe('x'.repeat(TOOL_PROGRESS_MAX_CHARS))
    const pair = `${'x'.repeat(10)}\u{1F600}${'y'.repeat(TOOL_PROGRESS_MAX_CHARS - 1)}`
    const bounded = boundProgress({ output: pair }).output
    expect(bounded).toBe('y'.repeat(TOOL_PROGRESS_MAX_CHARS - 1))
  })
})

describe('tool progress through the loop', () => {
  it('signals a fast parallel call finished before the slower earlier call commits', async () => {
    const adapter = new MockAdapter([
      calls([{ id: 'slow', name: 'p', args: { id: 'slow' } }, { id: 'fast', name: 'p', args: { id: 'fast' } }]),
      textResponse('done'),
    ])
    const ctx = await harness(adapter)
    const gates = new Map<string, () => void>()
    ctx.tools.register(defineContentToolFixture({
      name: 'p', description: 'gated', parameters: { id: { type: 'string', required: true } },
      isConcurrencySafe: () => true,
      async execute(args, exec) {
        exec.reportProgress({ output: `running ${args.id}` })
        await new Promise<void>((resolve) => { gates.set(args.id, resolve) })
        return [{ type: 'text', text: `done-${args.id}` }]
      },
    }))
    const agent = await ctx.agentLoop.create(SessionId('progress-order'), { provider: 'mock', model: 'mock' })
    const seen = record(ctx, agent)
    const idle = whenIdle(ctx, agent)
    agent.followup(createUserMessage({ content: [{ type: 'text', text: 'go' }], source: { kind: 'user' } }))
    await until(() => gates.size === 2)
    gates.get('fast')!()
    await until(() => seen.some(entry => entry.kind === 'executed'))
    expect(seen.filter(entry => entry.kind === 'result')).toEqual([])
    gates.get('slow')!()
    await idle

    expect(seen).toEqual([
      { kind: 'progress', callId: 'slow', output: 'running slow' },
      { kind: 'progress', callId: 'fast', output: 'running fast' },
      { kind: 'executed', callId: 'fast', isError: false },
      { kind: 'executed', callId: 'slow', isError: false },
      { kind: 'result', callId: 'slow' },
      { kind: 'result', callId: 'fast' },
    ])
  })

  it('drops progress reported after the body settles and keeps it out of the log and requests', async () => {
    const adapter = new MockAdapter([
      calls([{ id: 'c1', name: 'late', args: {} }]),
      textResponse('done'),
    ])
    const ctx = await harness(adapter)
    let kept: ToolRunContext | undefined
    ctx.tools.register(defineContentToolFixture({
      name: 'late', description: 'reports after settling', parameters: {},
      async execute(_args, exec) {
        kept = exec
        for (let i = 0; i < 50; i++) exec.reportProgress({ output: `PROGRESS_MARK ${i}` })
        return [{ type: 'text', text: 'ok' }]
      },
    }))
    const agent = await ctx.agentLoop.create(SessionId('progress-late'), { provider: 'mock', model: 'mock' })
    const seen = record(ctx, agent)
    const idle = whenIdle(ctx, agent)
    agent.followup(createUserMessage({ content: [{ type: 'text', text: 'go' }], source: { kind: 'user' } }))
    await idle
    kept!.reportProgress({ output: 'PROGRESS_MARK late' })
    await new Promise(r => setTimeout(r, 150))

    // Fifty synchronous reports coalesce to the first; the pending newest is
    // dropped when the call settles, and the late one never publishes.
    expect(seen).toEqual([
      { kind: 'progress', callId: 'c1', output: 'PROGRESS_MARK 0' },
      { kind: 'executed', callId: 'c1', isError: false },
      { kind: 'result', callId: 'c1' },
    ])
    expect(JSON.stringify(agent.session.snapshotEvents())).not.toContain('PROGRESS_MARK')
    expect(JSON.stringify(adapter.requests.map(request => request.messages))).not.toContain('PROGRESS_MARK')
  })

  it('rejects a progress snapshot without a string output as a tool error', async () => {
    const adapter = new MockAdapter([
      calls([{ id: 'c1', name: 'bad', args: {} }]),
      textResponse('done'),
    ])
    const ctx = await harness(adapter)
    ctx.tools.register(defineContentToolFixture({
      name: 'bad', description: 'malformed progress', parameters: {},
      async execute(_args, exec) {
        exec.reportProgress({ output: 42 } as unknown as ToolProgress)
        return [{ type: 'text', text: 'unreachable' }]
      },
    }))
    const agent = await ctx.agentLoop.create(SessionId('progress-bad'), { provider: 'mock', model: 'mock' })
    const seen = record(ctx, agent)
    const idle = whenIdle(ctx, agent)
    agent.followup(createUserMessage({ content: [{ type: 'text', text: 'go' }], source: { kind: 'user' } }))
    await idle
    expect(seen).toEqual([{ kind: 'executed', callId: 'c1', isError: true }, { kind: 'result', callId: 'c1' }])
  })

  it('contains a throwing progress receiver passed to a direct execution', async () => {
    const ctx = await harness(new MockAdapter([]))
    ctx.tools.register(defineContentToolFixture({
      name: 'echo', description: 'reports once', parameters: {},
      async execute(_args, exec) {
        exec.reportProgress({ output: 'x' })
        return [{ type: 'text', text: 'ok' }]
      },
    }))
    const received: string[] = []
    const result = await ctx.tools.execute({
      callId: ToolCallId('direct'), name: 'echo', arguments: {}, signal: new AbortController().signal,
      onProgress: (progress) => { received.push(progress.output); throw new Error('receiver failed') },
    })
    expect(received).toEqual(['x'])
    expect(result.isError).toBe(false)
  })
})
