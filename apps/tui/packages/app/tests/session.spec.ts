/** Durable session identity, command dispatch, and live state through real harness services. */
import { afterEach, describe, expect, it, vi } from 'vitest'
import { createUserMessage, type StreamChunk } from '@deepseek-ai/dsh-llm'
import type { AgentHandle, AssistantStreamFrame } from '@deepseek-ai/dsh-agent'
import { brandString } from '@deepseek-ai/dsh-brand'
import type { SessionId } from '@deepseek-ai/dsh-session'
import { transcriptRows } from '@dsh-tui/ui'
import { dictionaries } from '@dsh-tui/ui/copy.ts'
import { inputHistory } from '@dsh-tui/ui/history.ts'
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
    controller = new SessionController(fixture.ctx, agent, dictionaries.en, [], () => {}, { attachmentMaxBytes: 1048576, attachmentLimit: 8 })
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
    const before = transcriptRows(controller.view.committed)
    expect(before).toEqual([
      { kind: 'user', text: 'Remember this turn' }, { kind: 'assistant', text: 'Recorded answer' },
      { kind: 'notice', placement: 'turn-end', tone: 'info', text: 'Completed' },
    ])
    const id = handle.agent.id
    controller.close()
    await handle.dispose()
    let resumed!: SessionController
    const second = await openSession(ctx, { resume: id }, new AbortController().signal, agent => {
      resumed = new SessionController(ctx, agent, dictionaries.en, [], () => {}, { attachmentMaxBytes: 1048576, attachmentLimit: 8 })
    })
    cleanup.push(async () => { resumed.close(); await second.dispose() })
    await resumed.replay(new AbortController().signal)
    expect(second.agent.id).toBe(id)
    expect(ctx.sessionProjections.stateOf(second.agent.session, 'agentPreset')).toBe('audit')
    expect(transcriptRows(resumed.view.committed)).toEqual(before)
    resumed.submit('Continue here')
    await second.agent.whenIdle()
    expect(transcriptRows(resumed.view.committed).filter(row => row.kind === 'user')).toHaveLength(2)
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
    const { ctx, controller, model, handle } = await connected()
    const handler = vi.fn(({ rawInput }: { rawInput: string }) => ({ kind: 'success' as const, text: `Selected:${rawInput}` }))
    ctx.effect(() => ctx.commands.register({ name: 'select', description: 'Select an item', handler }))
    controller.submit('/select  item\nnext')
    await controller.drain()
    expect(handler).toHaveBeenCalledWith(expect.objectContaining({ rawInput: '  item\nnext' }))
    expect(transcriptRows(controller.view.committed)).toEqual([
      { kind: 'command', name: 'select', args: '  item\nnext' },
      { kind: 'notice', placement: 'command', tone: 'info', text: 'Selected:  item\nnext' },
    ])
    // An unregistered name stays in the composer instead of becoming a
    // prompt; a leading space is the way to send it as one.
    expect(controller.submit('/missing')).toBe(false)
    expect(controller.view.notice).toContain(`${dictionaries.en.unknownCommand}: /missing`)
    expect(model.requests).toHaveLength(0)
    controller.submit(' /missing')
    await handle.agent.whenIdle()
    expect(model.requests).toHaveLength(1)
    expect(JSON.stringify(model.requests[0]?.messages)).toContain('/missing')
  })

  it('recalls local redacted commands without retaining login arguments', async () => {
    const { ctx, controller } = await connected()
    expect(controller.submit('/Model mock/model')).toBe(true)
    await controller.drain()
    expect(controller.submit('/agents extra')).toBe(true)
    await controller.drain()
    expect(controller.submit('/login private-credential')).toBe(true)
    await controller.drain()
    const rows = transcriptRows(controller.view.committed).filter(row => row.kind === 'command')
    expect(rows).toEqual([
      { kind: 'command', name: 'model', args: '', inputOmitted: true, recall: '/Model mock/model' },
      { kind: 'command', name: 'agents', args: '', inputOmitted: true, recall: '/agents extra' },
      { kind: 'command', name: 'login', args: '', inputOmitted: true },
    ])
    expect([...inputHistory(controller.view.committed, [])]).toEqual(['/agents extra', '/Model mock/model'])
    using observation = await ctx.sessionQuery.observeSession(controller.agent.id, { projectionMode: 'none' })
    expect(observation.events.filter(event => event.type === 'command/run').map(event => event.data)).toEqual([
      expect.objectContaining({ name: 'model' }),
      expect.objectContaining({ name: 'agents' }),
      expect.objectContaining({ name: 'login' }),
    ])
    expect(JSON.stringify(observation.events)).not.toContain('private-credential')
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
    expect(transcriptRows(controller.view.committed).filter(row => row.kind === 'command')).toEqual([{ kind: 'command', name: 'wait', args: '' }])
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
    expect(transcriptRows(controller.view.committed)).toContainEqual({ kind: 'notice', placement: 'turn-end', tone: 'warn', text: 'Interrupted' })
    const context = createUserMessage({ content: [{ type: 'text', text: 'Plugin context' }], source: { kind: 'plugin', plugin: 'test' } })
    handle.agent.inject(context)
    handle.agent.inbox.append('next-turn', createUserMessage({ content: [{ type: 'text', text: 'Queued followup' }], source: { kind: 'user' } }))
    controller.submit('/clear-pending extra')
    await controller.drain()
    expect(controller.view.pending).toHaveLength(2)
    controller.submit('/clear-pending')
    await controller.drain()
    expect(controller.view.pending).toEqual([])
    expect(handle.agent.inbox.nextStep).toEqual([context])
    expect(handle.agent.inbox.nextTurn).toEqual([])
    controller.submit('/clear-pending')
    await controller.drain()
    expect(transcriptRows(controller.view.committed).at(-1)).toEqual({ kind: 'notice', placement: 'command', tone: 'info', text: dictionaries.en.noPending })
    model.response = async function* () { yield* textResponse('New task answer') }
    controller.submit('A different task')
    await handle.agent.whenIdle()
    expect(JSON.stringify(model.requests.at(-1)?.messages)).not.toContain('Keep this steering')
    expect(JSON.stringify(model.requests.at(-1)?.messages)).not.toContain('Queued followup')
  })

  it('does not replay settled text when a stream start is repeated', async () => {
    const { ctx, handle, controller, model } = await connected()
    const paused = Promise.withResolvers<void>()
    const resume = Promise.withResolvers<void>()
    cleanup.push(async () => { resume.resolve() })
    let start: Extract<AssistantStreamFrame, { type: 'start' }> | undefined
    const off = ctx.on('agent/assistant-stream', ({ agent, frame }) => {
      if (agent === handle.agent && frame.type === 'start') start = frame
    })
    cleanup.push(async () => { off() })
    model.response = async function* () {
      yield { type: 'text-delta', index: 0, text: 'First.\n\nNext' }
      paused.resolve()
      await resume.promise
      yield { type: 'text-delta', index: 0, text: ' paragraph.' }
      yield { type: 'finish', reason: { kind: 'stop' } }
    }
    controller.submit('Start')
    await paused.promise
    await vi.waitFor(() => expect(controller.view.live).toEqual([{ kind: 'assistant', text: '\nNext', continued: true }]))
    expect(start).toBeDefined()
    handle.agent.ctx.emit('agent/assistant-stream', { agent: handle.agent, frame: start! })
    expect(controller.view.live).toEqual([{ kind: 'assistant', text: '\nNext', continued: true }])
    resume.resolve()
    await handle.agent.whenIdle()
    expect(transcriptRows(controller.view.committed).filter(row => row.kind === 'assistant')).toEqual([
      { kind: 'assistant', text: 'First.' }, { kind: 'assistant', text: '\nNext paragraph.', continued: true },
    ])
  })

  it('prints settled Markdown while streaming and commits only the rest', async () => {
    const { ctx, handle, controller, model } = await connected()
    const paused = Promise.withResolvers<void>()
    const resume = Promise.withResolvers<void>()
    cleanup.push(async () => { resume.resolve() })
    const text = 'Line one.\n\nLine two.\n\nLine three.'
    model.response = async function* () {
      yield { type: 'block-start', index: 0, blockType: 'text' }
      yield { type: 'text-delta', index: 0, text: 'Line one.\n\nLine two.\n\nLi' }
      paused.resolve()
      await resume.promise
      yield { type: 'text-delta', index: 0, text: 'ne three.' }
      yield { type: 'block-end', index: 0, block: { type: 'text', text } }
      yield { type: 'finish', reason: { kind: 'stop' } }
    }
    controller.submit('Stream it')
    await paused.promise
    // Settled paragraphs are history; the unfinished paragraph stays live.
    await vi.waitFor(() => expect(controller.view.live).toEqual([{ kind: 'assistant', text: '\nLi', continued: true }]))
    expect(transcriptRows(controller.view.committed).filter(row => row.kind === 'assistant')).toEqual([
      { kind: 'assistant', text: 'Line one.\n\nLine two.' },
    ])
    resume.resolve()
    await handle.agent.whenIdle()
    const answer = transcriptRows(controller.view.committed).filter(row => row.kind === 'assistant')
    expect(answer).toEqual([
      { kind: 'assistant', text: 'Line one.\n\nLine two.' }, { kind: 'assistant', text: '\nLine three.', continued: true },
    ])
    expect(controller.view.live).toEqual([])
    // Printing is display only. The log holds one message, and a resume draws it whole.
    let resumed!: SessionController
    const id = handle.agent.id
    controller.close()
    await handle.dispose()
    const second = await openSession(ctx, { resume: id }, new AbortController().signal, agent => {
      resumed = new SessionController(ctx, agent, dictionaries.en, [], () => {}, { attachmentMaxBytes: 1048576, attachmentLimit: 8 })
    })
    cleanup.push(async () => { resumed.close(); await second.dispose() })
    await resumed.replay(new AbortController().signal)
    expect(transcriptRows(resumed.view.committed).filter(row => row.kind === 'assistant')).toEqual([{ kind: 'assistant', text }])
  })

  it('marks printed lines discarded when their attempt never commits', async () => {
    const { handle, controller, model } = await connected()
    model.response = async function* () {
      yield { type: 'block-start', index: 0, blockType: 'text' }
      yield { type: 'text-delta', index: 0, text: 'Printed line.\n\nrest' }
      // A block the assembler cannot finalize abandons the attempt.
      yield { type: 'block-start', index: 1, blockType: 'external-block' } as unknown as StreamChunk
      yield { type: 'finish', reason: { kind: 'stop' } }
    }
    controller.submit('Fail to settle')
    await handle.agent.whenIdle()
    const rows = transcriptRows(controller.view.committed)
    const printed = rows.findIndex(row => row.kind === 'assistant' && row.text === 'Printed line.')
    expect(printed).toBeGreaterThan(-1)
    expect(rows[printed + 1]).toEqual({ kind: 'notice', tone: 'info', text: dictionaries.en.attemptDiscarded })
  })

  it('replaces a completed live response with exactly one committed row', async () => {
    const { handle, controller, model } = await connected()
    model.response = async function* () { yield* textResponse('Only once') }
    controller.submit('Answer')
    await handle.agent.whenIdle()
    expect(controller.view.live).toEqual([])
    expect(transcriptRows(controller.view.committed).filter(row => row.kind === 'assistant')).toEqual([{ kind: 'assistant', text: 'Only once' }])
  })
})
