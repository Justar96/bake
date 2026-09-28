/**
 * Prompts sent while context is compacted: queued behind `/compact` and run
 * once it settles, and steering a turn that compacts itself. Real command,
 * compaction, and agent services; only the model is scripted.
 */
import { afterEach, expect, it } from 'vitest'
import TokenMeter from '@deepseek-ai/dsh-token-meter'
import BasicCompaction from '@deepseek-ai/dsh-compaction-basic'
import * as CommandCompact from '@deepseek-ai/dsh-command-compact'
import type { GenerateOptions, StreamChunk } from '@deepseek-ai/dsh-llm'
import type { SessionEvent } from '@deepseek-ai/dsh-session'
import { dictionaries } from '@dsh-tui/ui/copy.ts'
import { SessionController } from '../src/controller.ts'
import { openSession } from '../src/session.ts'
import { harness, textResponse, type ScriptedModel } from './harness.ts'

const cleanup: (() => Promise<void>)[] = []
afterEach(async () => { for (const dispose of cleanup.splice(0).reverse()) await dispose() })

/** Enough history that a summary is a useful reduction. */
const HISTORY = 'Older conversation history that the summary can fold. '.repeat(60)

async function connected(compaction: Record<string, unknown> = {}) {
  const fixture = await harness()
  cleanup.push(fixture.dispose)
  await fixture.ctx.plugin(TokenMeter)
  await fixture.ctx.plugin(BasicCompaction, compaction)
  await fixture.ctx.plugin(CommandCompact)
  let controller!: SessionController
  const handle = await openSession(fixture.ctx, {}, new AbortController().signal, agent => {
    controller = new SessionController(fixture.ctx, agent, dictionaries.en, [], () => {}, { attachmentMaxBytes: 1048576, attachmentLimit: 8 })
  })
  cleanup.push(async () => { controller.close(); await controller.drain(); await handle.dispose() })
  await controller.replay(new AbortController().signal)
  const events = async (): Promise<readonly SessionEvent[]> => {
    using observation = await fixture.ctx.sessionQuery.observeSession(handle.agent.id, { projectionMode: 'none' })
    return [...observation.events]
  }
  return { ...fixture, handle, controller, events }
}

/**
 * Hold summary requests until released, and answer every other request.
 * @returns the first summary request's arrival, and the release of every
 *   summary: text to summarize with, or an error to fail it.
 */
function holdSummaries(model: ScriptedModel) {
  const asked = Promise.withResolvers<void>()
  const release = Promise.withResolvers<string | Error>()
  model.response = async function* (options: GenerateOptions): AsyncGenerator<StreamChunk> {
    if (options.purpose !== 'compaction') {
      yield* textResponse('Recorded answer')
      return
    }
    asked.resolve()
    const aborted = new Promise<never>((_, reject) => {
      if (options.signal?.aborted) reject(options.signal.reason)
      options.signal?.addEventListener('abort', () => reject(options.signal!.reason), { once: true })
    })
    const outcome = await Promise.race([release.promise, aborted])
    if (outcome instanceof Error) throw outcome
    yield* textResponse(outcome)
  }
  return { asked: asked.promise, release: (outcome: string | Error) => { release.resolve(outcome) } }
}

/** The texts of human prompts in log order, each with the turn it opened. */
function prompts(events: readonly SessionEvent[]): { text: string; turn: number }[] {
  let turn = 0
  return events.flatMap(event => {
    if (event.type === 'turn/start') turn = event.data.turn
    if (event.type !== 'user/message' || event.data.source.kind !== 'user') return []
    return event.data.content.flatMap(block => block.type === 'text' ? [{ text: block.text, turn }] : [])
  })
}

it('queues a prompt sent during /compact in the inbox and runs it as its own turn once the summary commits', async () => {
  const { handle, controller, model, events } = await connected()
  const summaries = holdSummaries(model)
  controller.submit(HISTORY)
  await handle.agent.whenIdle()

  expect(controller.submit('/compact')).toBe(true)
  // Claimed in the submitting turn: the compaction's own start is already logged.
  expect(controller.view.compactPhase).toBe('summarizing')
  await summaries.asked
  expect(controller.submit('Run this after compaction')).toBe(true)
  expect(controller.view.notice).toBeUndefined()
  // Held in the inbox, which the pending panel reads; the agent stays idle.
  expect(controller.view.pending).toEqual([expect.objectContaining({ target: 'next-turn', text: 'Run this after compaction' })])
  expect(handle.agent.status).toBe('idle')
  // Commands still wait their turn.
  expect(controller.submit('/help')).toBe(false)
  expect(controller.view.notice).toBe(dictionaries.en.commandBusy)

  summaries.release('The earlier turn asked for history to be folded.')
  await controller.drain()
  await handle.agent.whenIdle()
  expect(controller.view.pending).toEqual([])
  expect(controller.view.compactPhase).toBeUndefined()
  const log = await events()
  const end = log.findIndex(event => event.type === 'compaction/end')
  expect(log[end]).toMatchObject({ data: { turn: null } })
  expect(log.find(event => event.type === 'command/done')?.data).toMatchObject({ kind: 'success', text: expect.stringMatching(/^Compacted \d+ history items/u) })
  // Its own turn, opened after the compaction closed, and answered once.
  const queued = log.findIndex(event => prompts([event]).some(prompt => prompt.text === 'Run this after compaction'))
  expect(queued).toBeGreaterThan(end)
  expect(prompts(log)).toEqual([{ text: HISTORY, turn: 1 }, { text: 'Run this after compaction', turn: 2 }])
  expect(model.requests.filter(request => request.purpose !== 'compaction')).toHaveLength(2)
  expect(JSON.stringify(model.requests.at(-1)!.messages)).toContain('The earlier turn asked for history to be folded.')
})

it('runs a prompt sent in the same turn as /compact after it, even when nothing is compactable', async () => {
  const { handle, controller, model, events } = await connected()
  holdSummaries(model)
  // A fresh session has no history, so the compaction ends without a start.
  expect(controller.submit('/compact')).toBe(true)
  expect(controller.view.compactPhase).toBe('preparing')
  expect(controller.submit('Run this after compaction')).toBe(true)
  await controller.drain()
  await handle.agent.whenIdle()
  const log = await events()
  // Not refused as busy: the prompt waited for the maintenance it found.
  expect(log.find(event => event.type === 'command/done')?.data).toMatchObject({ kind: 'success', text: 'No compactable history yet.' })
  expect(prompts(log)).toEqual([{ text: 'Run this after compaction', turn: 1 }])
  const done = log.findIndex(event => event.type === 'command/done')
  expect(log.findIndex(event => event.type === 'turn/start')).toBeGreaterThan(log.findIndex(event => event.type === 'command/run'))
  expect(done).toBeGreaterThan(-1)
})

it('keeps a queued prompt when Esc cancels or the summary fails, and runs it once the compaction settles', async () => {
  for (const settle of ['cancel', 'fail'] as const) {
    const { handle, controller, model, events } = await connected()
    const summaries = holdSummaries(model)
    controller.submit(HISTORY)
    await handle.agent.whenIdle()
    controller.submit('/compact')
    await summaries.asked
    controller.submit('Run this after compaction')
    if (settle === 'cancel') {
      controller.cancel()
      // Esc cancels the compaction, not the queued prompt.
      expect(controller.view.pending).toEqual([expect.objectContaining({ text: 'Run this after compaction' })])
    } else summaries.release(new Error('summary service unavailable'))
    await controller.drain()
    await handle.agent.whenIdle()
    const log = await events()
    expect(log.some(event => event.type === 'compaction/summary'), settle).toBe(false)
    expect(log.find(event => event.type === 'compaction/end')?.data, settle).toMatchObject({ error: expect.any(String) })
    expect(prompts(log), settle).toEqual([{ text: HISTORY, turn: 1 }, { text: 'Run this after compaction', turn: 2 }])
    expect(controller.view.pending, settle).toEqual([])
    expect(controller.view.notice, settle).toBe(settle === 'cancel' ? dictionaries.en.cancelled : undefined)
  }
})

it('shows a turn compacting its own context from the live events, and steers it meanwhile', async () => {
  // A tiny threshold, so the second turn compacts the first before its request.
  const { handle, controller, model, events } = await connected({ thresholdRatio: 0.05, retainTokens: 0 })
  const summaries = holdSummaries(model)
  controller.submit(HISTORY)
  await handle.agent.whenIdle()
  expect(controller.view.autoCompacting).toBeUndefined()
  controller.submit('Continue from there')
  await summaries.asked
  expect(controller.view.status).toBe('running')
  expect(controller.view.autoCompacting).toBe(true)
  expect(controller.view.compactPhase).toBeUndefined()
  // Enter steers the turn as it always does.
  expect(controller.submit('Steer after compaction')).toBe(true)
  expect(controller.view.pending).toEqual([expect.objectContaining({ target: 'next-step', text: 'Steer after compaction' })])
  summaries.release('A long prompt about history.')
  await handle.agent.whenIdle()
  expect(controller.view.autoCompacting).toBeUndefined()
  const log = await events()
  const start = log.find(event => event.type === 'compaction/start')
  expect(start?.data).toMatchObject({ turn: 2 })
  expect(start?.data).not.toHaveProperty('sourceCommandId')
  expect(prompts(log).map(prompt => prompt.text)).toEqual([HISTORY, 'Continue from there', 'Steer after compaction'])
})
