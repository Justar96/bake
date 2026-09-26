/** The TUI reads child identities from the real Harness subagent listing. */
import { expect, it, vi } from 'vitest'
import SubagentRuntime, { SUBAGENT_DESCRIPTOR_VERSION, type SubagentResult } from '@deepseek-ai/dsh-subagent'
import { SessionId } from '@deepseek-ai/dsh-session'
import { transcriptRows } from '@dsh-tui/ui'
import { dictionaries } from '@dsh-tui/ui/copy.ts'
import { childOutcome } from '../src/subagents.ts'
import { SessionController } from '../src/controller.ts'
import { openSession } from '../src/session.ts'
import { harness } from './harness.ts'
import { createUserMessage } from '@deepseek-ai/dsh-llm'
import TokenMeter from '@deepseek-ai/dsh-token-meter'

it('shows live delegated children and lists their authoritative saved metadata', async () => {
  const fixture = await harness()
  let controller: SessionController | undefined
  let handle: Awaited<ReturnType<typeof openSession>> | undefined
  try {
    await fixture.ctx.plugin(SubagentRuntime)
    handle = await openSession(fixture.ctx, {}, new AbortController().signal, (agent, selection) => {
      controller = new SessionController(fixture.ctx, agent, dictionaries.en, [], () => {},
        { attachmentMaxBytes: 1048576, attachmentLimit: 8 }, selection)
    })
    await controller!.replay(new AbortController().signal)
    const child = fixture.ctx.sessions.create(SessionId('tui-child'), {
      meta: { parentSession: handle.agent.id, origin: 'subagent' },
    })
    child.append('turn/start', { turn: 1 })
    child.append('subagent/descriptor', {
      version: SUBAGENT_DESCRIPTOR_VERSION, mode: 'continuable', provider: 'spawn', label: 'Review tests',
    })
    await controller!.drain()
    expect(controller!.view.subagents).toEqual([expect.objectContaining({ label: 'Review tests', state: 'live', inspectable: true })])

    controller!.submit('/agents')
    await vi.waitFor(() => expect(controller!.view.interaction?.kind).toBe('select'))
    const choice = controller!.view.interaction!
    if (choice.kind !== 'select') throw new Error('Expected child picker')
    expect(choice.choices[0]).toMatchObject({ label: 'Review tests', status: { text: 'Live' } })
    controller!.interactions.answer(choice.id, child.id)
    await controller!.drain()
    expect(controller!.view.inspection?.sessionId).toBe(child.id)
    controller!.cancel()
    expect(controller!.view.inspection).toBeUndefined()
    // An id opens that child without the picker, as the agents sheet's Enter does.
    controller!.submit(`/agents ${child.id}`)
    await controller!.drain()
    expect(controller!.view.interaction).toBeUndefined()
    expect(controller!.view.inspection?.sessionId).toBe(child.id)
    controller!.cancel()
    controller!.submit('/agents missing-child')
    await controller!.drain()
    expect(controller!.view.inspection).toBeUndefined()
    expect(JSON.stringify(transcriptRows(controller!.view.committed))).toContain(dictionaries.en.agentsUsage)
    const output = transcriptRows(controller!.view.committed)
    expect(output).toContainEqual(expect.objectContaining({ kind: 'command', name: 'agents' }))
    expect(fixture.model.requests).toEqual([])

    const settled = Promise.withResolvers<SubagentResult>()
    const off = fixture.ctx.subagents.registerProvider({
      name: 'remote-test', inheritsParentContext: false,
      capabilities: { agentOptions: false, outputSchema: false, depthLimit: false, toolFilter: false, persona: false },
      start: async () => ({ id: SessionId('remote-child'), localAgent: undefined,
        result: settled.promise, dispose: async () => {} }),
    })
    try {
      const run = await fixture.ctx.subagents.start('remote-test', {
        parent: handle.agent, prompt: [{ type: 'text', text: 'Review the branch' }], signal: new AbortController().signal,
      })
      await vi.waitFor(() => expect(controller!.view.subagents).toContainEqual(expect.objectContaining({ label: 'remote-test', state: 'working', inspectable: false })))
      controller!.submit('/agents')
      await vi.waitFor(() => expect(controller!.view.interaction?.kind).toBe('select'))
      controller!.interactions.answer(controller!.view.interaction!.id, 'remote-child')
      await controller!.drain()
      expect(controller!.view.inspection).toBeUndefined()
      expect(JSON.stringify(transcriptRows(controller!.view.committed))).toContain(dictionaries.en.subagentNoTranscript)
      settled.resolve({ output: [{ type: 'text', text: 'Done' }], stopReason: 'completed' })
      await run.result
      await vi.waitFor(() => expect(controller!.view.subagents.some(entry => entry.id === 'remote-child')).toBe(false))
      await run.dispose()
    } finally { off() }
  } finally {
    controller?.close()
    try { await controller?.drain() } finally { await handle?.dispose(); await fixture.dispose() }
  }
})


it('observes a running child, returns without cancellation, and opens its saved transcript without adopting it', async () => {
  const fixture = await harness()
  const release = Promise.withResolvers<void>()
  const finish = Promise.withResolvers<void>()
  let controller: SessionController | undefined
  let parent: Awaited<ReturnType<typeof openSession>> | undefined
  let child: Awaited<ReturnType<typeof fixture.ctx.agents.create>> | undefined
  try {
    await fixture.ctx.plugin(SubagentRuntime)
    await fixture.ctx.plugin(TokenMeter)
    parent = await openSession(fixture.ctx, {}, new AbortController().signal, (agent, selection) => {
      controller = new SessionController(fixture.ctx, agent, dictionaries.en, [], () => {},
        { attachmentMaxBytes: 1048576, attachmentLimit: 8 }, selection)
    })
    await controller!.replay(new AbortController().signal)
    child = await fixture.ctx.agents.create({ sessionId: SessionId('observed-child'),
      meta: { parentSession: parent.agent.id, origin: 'subagent' }, agentOptions: { provider: 'mock', model: 'model' } })
    child.agent.session.append('subagent/descriptor', {
      version: SUBAGENT_DESCRIPTOR_VERSION, mode: 'continuable', provider: 'spawn', label: 'Inspect the code',
    })
    fixture.model.response = async function* (request) {
      yield { type: 'usage', usage: { inputTokens: request.sessionId === child?.agent.id ? 900 : 100, outputTokens: 20 } }
      yield { type: 'block-start', index: 0, blockType: 'text' }
      yield { type: 'text-delta', index: 0, text: 'Child ' }
      await release.promise
      yield { type: 'text-delta', index: 0, text: 'completed the review' }
      await finish.promise
      yield { type: 'block-end', index: 0, block: { type: 'text', text: 'Child completed the review' } }
      yield { type: 'finish', reason: { kind: 'stop' } }
    }
    child.agent.followup(createUserMessage({ content: [{ type: 'text', text: 'Child task' }], source: { kind: 'user' } }))
    controller!.submit('Parent task')
    await vi.waitFor(() => expect(fixture.model.requests).toHaveLength(2))
    const open = async () => {
      expect(controller!.submit('/agents')).toBe(true)
      await vi.waitFor(() => expect(controller!.view.interaction?.kind).toBe('select'))
      controller!.interactions.answer(controller!.view.interaction!.id, child!.agent.id)
      await controller!.drain()
      expect(controller!.view.inspection?.sessionId).toBe(child!.agent.id)
    }
    await open()
    expect(controller!.view.inspection?.status).toBe('running')
    expect(controller!.view.inspection?.context).toBeUndefined()
    expect(transcriptRows(controller!.view.inspection!.committed)).toContainEqual({ kind: 'user', text: 'Child task' })
    expect(controller!.submit('Must not steer either agent')).toBe(false)
    controller!.cancel()
    expect(child.agent.status).toBe('running')
    expect(parent.agent.status).toBe('running')
    await open()
    const approval = fixture.ctx.approval.request({ agent: parent.agent, toolName: 'bash' })
    await vi.waitFor(() => expect(controller!.view.interaction?.kind).toBe('approval'))
    expect(controller!.view.inspection).toBeUndefined()
    controller!.cancel()
    await expect(approval).resolves.toBe('cancelled')
    await open()
    release.resolve()
    await vi.waitFor(() => expect(controller!.view.inspection?.live).toContainEqual({ kind: 'assistant', text: 'completed the review' }))
    finish.resolve()
    await Promise.all([child.agent.whenIdle(), parent.agent.whenIdle()])
    expect(controller!.view.inspection?.status).toBe('idle')
    expect(controller!.view.context?.used).toBeLessThan(200)
    expect(controller!.view.inspection?.context?.used).toBeGreaterThan(900)
    expect(controller!.view.inspection?.context?.window).toBe(8192)
    expect(controller!.view.inspection?.usage).toEqual({ input: 900, output: 20 })
    expect(transcriptRows(controller!.view.inspection!.committed).filter(row => row.kind === 'assistant'))
      .toEqual([{ kind: 'assistant', text: 'Child completed the review' }])
    await child.dispose()
    await controller!.drain()
    expect(controller!.view.inspection?.context?.used).toBeGreaterThan(900)
    expect(controller!.view.inspection?.usage).toEqual({ input: 900, output: 20 })
    expect(controller!.view.subagents[0]).toMatchObject({ state: 'saved', outcome: 'completed' })
    controller!.cancel()
    await open()
    expect(fixture.ctx.agents.get(child.agent.id)).toBeUndefined()
    expect(controller!.view.inspection?.context?.used).toBeGreaterThan(900)
    expect(controller!.view.inspection?.usage).toEqual({ input: 900, output: 20 })
    expect(transcriptRows(controller!.view.inspection!.committed)).toContainEqual({ kind: 'assistant', text: 'Child completed the review' })
    expect(fixture.model.requests).toHaveLength(2)
    controller!.close()
    await controller!.drain()
  } finally {
    finish.resolve()
    release.resolve()
    controller?.close()
    try { await controller?.drain() } finally { await child?.dispose(); await parent?.dispose(); await fixture.dispose() }
  }
})


it('shows each saved child outcome and clears it when another turn starts', async () => {
  const fixture = await harness()
  let controller: SessionController | undefined
  let handle: Awaited<ReturnType<typeof openSession>> | undefined
  try {
    await fixture.ctx.plugin(SubagentRuntime)
    handle = await openSession(fixture.ctx, {}, new AbortController().signal, (agent, selection) => {
      controller = new SessionController(fixture.ctx, agent, dictionaries.en, [], () => {},
        { attachmentMaxBytes: 1048576, attachmentLimit: 8 }, selection)
    })
    const child = fixture.ctx.sessions.create(SessionId('outcome-child'), {
      meta: { parentSession: handle.agent.id, origin: 'subagent' },
    })
    child.append('subagent/descriptor', { version: SUBAGENT_DESCRIPTOR_VERSION, mode: 'continuable', provider: 'spawn', label: 'Review' })
    const observe = vi.spyOn(fixture.ctx.sessionQuery, 'observeSession')
    const reasons = [
      { kind: 'completed' } as const,
      { kind: 'error', error: { code: 'TRANSPORT', message: 'lost stream' } } as const,
      { kind: 'blocked' } as const,
      { kind: 'interrupted' } as const,
      { kind: 'aborted', reason: { kind: 'user' } } as const,
      { kind: 'max-tokens' } as const,
    ]
    for (const [index, reason] of reasons.entries()) {
      child.append('turn/start', { turn: index + 1 })
      await controller!.drain()
      expect(controller!.view.subagents[0]?.outcome).toBeUndefined()
      child.append('turn/end', { turn: index + 1, reason })
      await controller!.drain()
      expect(controller!.view.subagents[0]?.outcome).toBe(reason.kind === 'completed' ? 'completed' : reason.kind === 'error' ? 'failed' : 'stopped')
    }
    // The child's own turn events keep its outcome; its log is not reread on each.
    expect(observe.mock.calls.filter(([id]) => id === child.id).length).toBeLessThanOrEqual(1)
    const events = child.snapshotEvents()
    expect(childOutcome(events, events.length)).toBeUndefined()
    controller!.close()
    child.append('turn/start', { turn: reasons.length + 1 })
    await controller!.drain()
  } finally {
    controller?.close()
    try { await controller?.drain() } finally { await handle?.dispose(); await fixture.dispose() }
  }
})
