/** Context occupancy follows the meter's public view through growth and compaction. */
import { expect, it } from 'vitest'
import TokenMeter from '@deepseek-ai/dsh-token-meter'
import { CompactionId } from '@deepseek-ai/dsh-compaction'
import { createUserMessage } from '@deepseek-ai/dsh-llm'
import type { AgentHandle } from '@deepseek-ai/dsh-agent'
import { dictionaries } from '@dsh-tui/ui/copy.ts'
import { SessionController } from '../src/controller.ts'
import { openSession } from '../src/session.ts'
import { contextFor } from '../src/status.ts'
import { harness, textResponse } from './harness.ts'

it('does not pair one route’s sample with a changed capacity on the same route', () => {
  const route = { provider: 'mock', model: 'model' }
  const pressure = { projectedTokens: 900, contextWindow: 64_000, sampledContextWindow: 8192,
    requestRoute: route, sampledRoute: route }
  expect(contextFor(pressure, 'mock/model')).toBeUndefined()
  expect(contextFor({ ...pressure, sampledContextWindow: 64_000 }, 'mock/model'))
    .toEqual({ used: 900, window: 64_000 })
})

it('reports projected context after output and reduces it immediately after compaction', async () => {
  const fixture = await harness()
  let controller: SessionController | undefined
  let handle: AgentHandle | undefined
  try {
    await fixture.ctx.plugin(TokenMeter)
    handle = await openSession(fixture.ctx, {}, new AbortController().signal, agent => {
      controller = new SessionController(fixture.ctx, agent, dictionaries.en, [], () => {}, { attachmentMaxBytes: 1048576, attachmentLimit: 8 })
    })
    const view = controller!
    await view.replay(new AbortController().signal)
    expect(view.view.context).toBeUndefined()
    fixture.model.response = async function* () {
      yield { type: 'usage', usage: { inputTokens: 900, outputTokens: 100 } }
      yield* textResponse('An answer that grows the conversation. '.repeat(20))
    }
    view.submit('Explain the changes')
    await handle.agent.whenIdle()
    const session = handle.agent.session
    const pressure = () => fixture.ctx.sessionProjections.snapshot(session, ['contextPressure']).values.contextPressure!
    const before = pressure().projectedTokens!
    expect(before).toBeGreaterThan(900)
    expect(view.view.context).toEqual({ used: before, window: 8192 })

    // Record the meter-owned shadow price and replacement used by compaction-basic.
    // The first surface node is the system prompt; compaction preserves it.
    const shadowed = fixture.ctx.tokenMeter.measure(session).nodes.slice(1)
    const start = shadowed[0]!.seq
    const end = shadowed.at(-1)!.seq
    session.append('compaction/summary', {
      compactionId: CompactionId('tui-context-test'), summary: [{ type: 'text', text: 'summary' }],
      shadowedRange: { start, end }, shadowedSeqs: shadowed.map(node => node.seq),
      shadowedTokenCount: shadowed.reduce((total, node) => total + node.heuristicTokens, 0),
      provider: 'mock', model: 'model',
    })
    session.append('user/message', createUserMessage({
      content: [{ type: 'text', text: 'summary' }], source: { kind: 'plugin', plugin: 'test' },
    }), { surfaceOp: { op: 'replace', startSeq: start, endSeq: end }, sourceEventSeqs: shadowed.map(node => node.seq) })
    expect(pressure().pressureTokens).toBe(900)
    expect(pressure().projectedTokens).toBeLessThan(before)
    expect(view.view.context?.used).toBe(pressure().projectedTokens)
  } finally {
    controller?.close()
    await handle?.dispose()
    await fixture.dispose()
  }
})

it('withholds a previous model’s context through selection and the next request until its usage arrives', async () => {
  const fixture = await harness()
  const began = Promise.withResolvers<void>()
  const release = Promise.withResolvers<void>()
  let controller: SessionController | undefined
  let handle: AgentHandle | undefined
  try {
    await fixture.ctx.plugin(TokenMeter)
    const resolve = fixture.model.resolveModel.bind(fixture.model)
    fixture.model.resolveModel = async (provider, model, signal) => ({
      ...await resolve(provider, model, signal), context: { contextWindow: model === 'other' ? 64_000 : 8192 },
    })
    handle = await openSession(fixture.ctx, {}, new AbortController().signal, (agent, selection) => {
      controller = new SessionController(fixture.ctx, agent, dictionaries.en, [], () => {},
        { attachmentMaxBytes: 1048576, attachmentLimit: 8 }, selection)
    })
    await controller!.replay(new AbortController().signal)
    fixture.model.response = async function* () {
      yield { type: 'usage', usage: { inputTokens: 900, outputTokens: 20 } }
      yield* textResponse('First answer')
    }
    controller!.submit('First question')
    await handle.agent.whenIdle()
    expect(controller!.view.context?.window).toBe(8192)
    expect(controller!.view.usage).toEqual({ input: 900, output: 20 })

    controller!.submit('/model mock/other')
    await controller!.drain()
    expect(controller!.view.model).toBe('mock/other')
    expect(controller!.view.context).toBeUndefined()
    expect(controller!.view.usage).toEqual({ input: 900, output: 20 })

    fixture.model.response = async function* () {
      began.resolve()
      await release.promise
      yield { type: 'usage', usage: { inputTokens: 1250, outputTokens: 30 } }
      yield* textResponse('Second answer')
    }
    controller!.submit('Use the other model')
    await began.promise
    const mixed = fixture.ctx.sessionProjections.snapshot(handle.agent.session, ['contextPressure']).values.contextPressure
    expect(mixed?.contextWindow).toBe(64_000)
    expect(mixed?.sampledRoute).toEqual({ provider: 'mock', model: 'model' })
    expect(controller!.view.context).toBeUndefined()
    release.resolve()
    await handle.agent.whenIdle()
    expect(controller!.view.context?.window).toBe(64_000)
    expect(controller!.view.context?.used).toBeGreaterThan(1250)
    expect(controller!.view.usage).toEqual({ input: 2150, output: 50 })
  } finally {
    release.resolve()
    controller?.close()
    try { await controller?.drain() } finally { await handle?.dispose(); await fixture.dispose() }
  }
})

it('reports billed tokens once a request has, and a cache hit only from a provider that reports cache traffic', async () => {
  const fixture = await harness()
  let controller: SessionController | undefined
  let handle: AgentHandle | undefined
  try {
    await fixture.ctx.plugin(TokenMeter)
    handle = await openSession(fixture.ctx, {}, new AbortController().signal, agent => {
      controller = new SessionController(fixture.ctx, agent, dictionaries.en, [], () => {}, { attachmentMaxBytes: 1048576, attachmentLimit: 8 })
    })
    const view = controller!
    await view.replay(new AbortController().signal)
    expect(view.view.usage).toBeUndefined()

    fixture.model.response = async function* () {
      yield { type: 'usage', usage: { inputTokens: 900, outputTokens: 100 } }
      yield* textResponse('First answer.')
    }
    view.submit('First question')
    await handle.agent.whenIdle()
    expect(view.view.usage).toEqual({ input: 900, output: 100 })

    // Billed input is the disjoint buckets together; the hit is the read share.
    fixture.model.response = async function* () {
      yield { type: 'usage', usage: { inputTokens: 100, outputTokens: 50, cacheReadTokens: 800, cacheWriteTokens: 100 } }
      yield* textResponse('Second answer.')
    }
    view.submit('Second question')
    await handle.agent.whenIdle()
    expect(view.view.usage).toEqual({ input: 1900, output: 150, cached: 800 })
  } finally {
    controller?.close()
    await handle?.dispose()
    await fixture.dispose()
  }
})
