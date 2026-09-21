/** Durable session identity, command dispatch, and live state through real harness services. */
import { afterEach, describe, expect, it, vi } from 'vitest'
import { createUserMessage } from '@deepseek-ai/dsh-llm'
import type { AgentHandle } from '@deepseek-ai/dsh-agent'
import { brandString } from '@deepseek-ai/dsh-brand'
import type { SessionId } from '@deepseek-ai/dsh-session'
import { dictionaries } from '@dsh-tui/ui/copy.ts'
import { openSession } from '../src/session.ts'
import { SessionController } from '../src/controller.ts'
import { harness, textResponse } from './harness.ts'

const cleanup: (() => Promise<void>)[] = []
afterEach(async () => { for (const dispose of cleanup.splice(0).reverse()) await dispose() })

async function connected() {
  const fixture = await harness()
  cleanup.push(fixture.dispose)
  let controller!: SessionController
  const handle = await openSession(fixture.ctx, {}, new AbortController().signal, agent => {
    controller = new SessionController(fixture.ctx, agent, dictionaries.en, [], () => {})
  })
  cleanup.push(async () => { controller.close(); await handle.dispose(); await controller.drain() })
  await controller.replay(new AbortController().signal)
  return { ...fixture, handle, controller }
}

describe('session wiring', () => {
  it('persists the selected preset and resumes the exact history once', async () => {
    const { ctx, handle, controller, model } = await connected()
    expect(handle.agent.session.header.agentPreset).toBe('audit')
    controller.submit('Remember this turn')
    await handle.agent.whenIdle()
    expect(model.requests).toHaveLength(1)
    const before = controller.view.committed
    expect(before).toEqual([{ kind: 'user', text: 'Remember this turn' }, { kind: 'assistant', text: 'Recorded answer' }])
    const id = handle.agent.id
    controller.close()
    await handle.dispose()
    let resumed!: SessionController
    const second = await openSession(ctx, { resume: id }, new AbortController().signal, agent => {
      resumed = new SessionController(ctx, agent, dictionaries.en, [], () => {})
    })
    cleanup.push(async () => { resumed.close(); await second.dispose() })
    await resumed.replay(new AbortController().signal)
    expect(second.agent.id).toBe(id)
    expect(ctx.sessionProjections.stateOf(second.agent.session, 'agentPreset')).toBe('audit')
    expect(resumed.view.committed).toEqual(before)
    resumed.submit('Continue here')
    await second.agent.whenIdle()
    expect(resumed.view.committed.filter(row => row.kind === 'user')).toHaveLength(2)
  })

  it('refuses an unknown id, live owner, or a conflicting preset', async () => {
    const { ctx, handle, controller } = await connected()
    const signal = new AbortController().signal
    await expect(openSession(ctx, { resume: 'missing' }, signal, () => {})).rejects.toThrow()
    expect(ctx.agents.get(brandString<SessionId>('missing'))).toBeUndefined()
    await expect(openSession(ctx, { resume: handle.agent.id }, signal, () => {})).rejects.toThrow('live owner')
    controller.submit('Persist this session')
    await handle.agent.whenIdle()
    const id = handle.agent.id
    controller.close()
    await handle.dispose()
    await expect(openSession(ctx, { resume: id, preset: 'other' }, signal, () => {})).rejects.toThrow('cannot change')
    expect(ctx.agents.get(id)).toBeUndefined()
  })

  it('rejects a different workspace and requires an explicit preset for legacy sessions', async () => {
    const { ctx } = await connected()
    const signal = new AbortController().signal
    const create = async (id: string, cwd: string): Promise<AgentHandle> => {
      const handle = await ctx.agents.create({
        sessionId: brandString<SessionId>(id), meta: { cwd }, agentOptions: { provider: 'mock', model: 'model' },
      })
      handle.agent.followup(createUserMessage({ content: [{ type: 'text', text: 'Persist' }], source: { kind: 'user' } }))
      await handle.agent.whenIdle()
      return handle
    }
    const foreign = await create('foreign', '/different-workspace')
    await foreign.dispose()
    await expect(openSession(ctx, { resume: 'foreign', preset: 'audit' }, signal, () => {})).rejects.toThrow('workspace')
    const legacy = await create('legacy', process.cwd())
    await legacy.dispose()
    await expect(openSession(ctx, { resume: 'legacy' }, signal, () => {})).rejects.toThrow('no recorded preset')
    const restored = await openSession(ctx, { resume: 'legacy', preset: 'audit' }, signal, () => {})
    cleanup.push(() => restored.dispose())
    expect(ctx.sessionProjections.stateOf(restored.agent.session, 'agentPreset')).toBe('audit')
  })

  it('routes slash commands to the registry with exact arguments and durable output', async () => {
    const { ctx, controller, model } = await connected()
    const handler = vi.fn(({ rawInput }: { rawInput: string }) => ({ kind: 'success' as const, text: `Selected:${rawInput}` }))
    ctx.effect(() => ctx.commands.register({ name: 'select', description: 'Select an item', handler }))
    controller.submit('/select  item\nnext')
    await controller.drain()
    expect(handler).toHaveBeenCalledWith(expect.objectContaining({ rawInput: '  item\nnext' }))
    expect(controller.view.committed).toEqual([
      { kind: 'user', text: '/select  item\nnext' },
      { kind: 'notice', tone: 'info', text: 'Selected:  item\nnext' },
    ])
    controller.submit('/missing')
    await controller.drain()
    expect(controller.view.notice).toContain('Unknown command')
    expect(model.requests).toHaveLength(0)
  })

  it('serializes commands and records cancellation before releasing the session', async () => {
    const { ctx, handle, controller, model } = await connected()
    const started = Promise.withResolvers<void>()
    const finished = Promise.withResolvers<void>()
    ctx.effect(() => ctx.commands.register({
      name: 'wait', description: 'Wait for cancellation',
      handler: async ({ signal }) => {
        started.resolve()
        await new Promise<void>(resolve => {
          if (signal.aborted) resolve()
          else signal.addEventListener('abort', () => resolve(), { once: true })
        })
        finished.resolve()
        return { kind: 'success' }
      },
    }))
    controller.submit('/wait')
    await started.promise
    controller.submit('/wait')
    expect(controller.view.notice).toBe(dictionaries.en.commandBusy)
    expect(controller.view.command).toBe('/wait')
    controller.cancel()
    await controller.drain()
    await finished.promise
    expect(controller.view.command).toBeUndefined()
    expect(controller.view.committed.filter(row => row.kind === 'user')).toEqual([{ kind: 'user', text: '/wait' }])
    expect(model.requests).toHaveLength(0)
    expect(handle.agent.status).toBe('idle')
    using observation = await ctx.sessionQuery.observeSession(handle.agent.id, { projectionMode: 'none' })
    expect(observation.events.filter(event => event.type === 'command/done')).toHaveLength(1)
  })

  it('shows streaming separately and retains authoritative steering after interruption', async () => {
    const { handle, controller, model } = await connected()
    const streaming = Promise.withResolvers<void>()
    model.response = async function* (options) {
      yield { type: 'block-start', index: 0, blockType: 'text' }
      yield { type: 'text-delta', index: 0, text: 'Partial answer' }
      streaming.resolve()
      await new Promise<void>(resolve => {
        if (options.signal?.aborted) resolve()
        else options.signal?.addEventListener('abort', () => resolve(), { once: true })
      })
      options.signal?.throwIfAborted()
    }
    controller.submit('Start a turn')
    await streaming.promise
    await vi.waitFor(() => expect(controller.view.live).toEqual([{ kind: 'assistant', text: 'Partial answer' }]))
    expect(controller.view.status).toBe('running')
    controller.submit('Keep this steering')
    expect(controller.view.pending).toEqual([expect.objectContaining({ text: 'Keep this steering', target: 'next-step' })])
    controller.cancel()
    expect(controller.view.stopping).toBe(true)
    await handle.agent.whenIdle()
    expect(controller.view.status).toBe('idle')
    expect(controller.view.stopping).toBe(false)
    expect(controller.view.live).toEqual([])
    expect(controller.view.pending).toEqual([expect.objectContaining({ text: 'Keep this steering' })])
    expect(controller.view.committed).toContainEqual({ kind: 'notice', tone: 'warn', text: 'Interrupted' })
  })

  it('replaces a completed live response with exactly one committed row', async () => {
    const { handle, controller, model } = await connected()
    model.response = async function* () { yield* textResponse('Only once') }
    controller.submit('Answer')
    await handle.agent.whenIdle()
    expect(controller.view.live).toEqual([])
    expect(controller.view.committed.filter(row => row.kind === 'assistant')).toEqual([{ kind: 'assistant', text: 'Only once' }])
  })
})
