/** Slash discovery and dispatch use the same scoped services as Harness clients. */
import { afterEach, expect, it, vi } from 'vitest'
import Skills, { type SkillCandidate } from '@deepseek-ai/dsh-skill'
import * as ToolSkill from '@deepseek-ai/dsh-tool-skill'
import Goals from '@deepseek-ai/dsh-goal'
import * as CommandGoal from '@deepseek-ai/dsh-command-goal'
import { SessionId } from '@deepseek-ai/dsh-session'
import { dictionaries } from '@dsh-tui/ui/copy.ts'
import { SessionController } from '../src/controller.ts'
import { openSession } from '../src/session.ts'
import { harness } from './harness.ts'

const cleanup: (() => Promise<void>)[] = []
afterEach(async () => { for (const dispose of cleanup.splice(0).reverse()) await dispose() })

async function connected() {
  const fixture = await harness()
  cleanup.push(fixture.dispose)
  await fixture.ctx.plugin(Skills)
  await fixture.ctx.plugin(ToolSkill)
  await fixture.ctx.plugin(Goals)
  await fixture.ctx.plugin(CommandGoal)
  let controller!: SessionController
  const changed = vi.fn()
  const handle = await openSession(fixture.ctx, {}, new AbortController().signal, (agent, selection) => {
    controller = new SessionController(fixture.ctx, agent, dictionaries.en, [], changed, { attachmentMaxBytes: 1048576, attachmentLimit: 8 }, selection)
  })
  cleanup.push(async () => { controller.close(); await controller.drain(); await handle.dispose() })
  await controller.replay(new AbortController().signal)
  await controller.drain()
  return { ...fixture, handle, controller, changed }
}

it('discovers user-invocable skills and gives registered commands the shared name', async () => {
  const { ctx, controller, handle, model } = await connected()
  for (const [name, userInvocable, modelInvocable] of [['review', true, false], ['hidden', false, true], ['goal', true, true]] as const) {
    handle.agent.ctx.get('skills')!.register({ name, description: `${name} instructions`, content: `${name} body`, source: 'runtime', invocation: { userInvocable, modelInvocable } })
  }
  await controller.drain()
  const entries = controller.view.completion.entries
  expect(entries).toContainEqual({ name: 'review', description: 'review instructions', kind: 'skill' })
  expect(entries.some(entry => entry.name === 'hidden')).toBe(false)
  expect(entries.filter(entry => entry.name === 'goal')).toEqual([expect.objectContaining({ kind: 'command' })])
  controller.submit('/goal')
  await controller.drain()
  expect(model.requests).toHaveLength(0)
  controller.submit('/goal Review the patch')
  await controller.drain()
  await handle.agent.whenIdle()
  expect(ctx.goals.get(handle.agent)?.objective).toBe('Review the patch')
  expect(handle.agent.session.snapshotEvents().some(event => event.type === 'command/run' && event.data.name === 'goal')).toBe(true)
  expect(JSON.stringify(model.requests)).not.toContain('goal body')
})

it('routes a user-only skill through the logged pre-step injection without expanding it in the TUI', async () => {
  const { handle, controller, model } = await connected()
  handle.agent.ctx.get('skills')!.register({
    name: 'review', description: 'Review a change', content: 'Check the change and report concrete findings.', source: 'runtime',
    invocation: { userInvocable: true, modelInvocable: false },
  })
  await controller.drain()
  controller.submit('/review  current patch')
  await handle.agent.whenIdle()
  expect(model.requests).toHaveLength(1)
  const inputs = handle.agent.session.snapshotEvents().flatMap(event => event.type === 'user/message'
    && (event.data.source.kind === 'user' || event.data.source.kind === 'skill-invocation')
    ? [{ source: event.data.source, content: event.data.content }] : [])
  expect(inputs).toHaveLength(2)
  expect(inputs[0]?.content).toEqual([{ type: 'text', text: '/review  current patch' }])
  expect(inputs[1]?.source).toEqual({ kind: 'skill-invocation', name: 'review', form: 'instructions' })
  expect(JSON.stringify(model.requests[0]?.messages)).toContain('Check the change and report concrete findings.')
  await expect(JSON.stringify(inputs, null, 2) + '\n').toMatchFileSnapshot('./expected/skill-invocation.json')
})

it('refreshes scoped registrations and excludes another agent’s commands and skills', async () => {
  const { ctx, handle, controller } = await connected()
  const other = await ctx.agents.create({ sessionId: SessionId('catalog-other-agent'), agentOptions: { provider: 'mock', model: 'model' } })
  cleanup.push(() => other.dispose())
  other.agent.ctx.get('commands')!.register({ name: 'foreign', description: 'Another agent', handler: () => ({ kind: 'success' }) })
  other.agent.ctx.get('skills')!.register({ name: 'foreign-skill', description: 'Another agent', content: 'Foreign body', source: 'runtime' })
  const unregister = handle.agent.ctx.get('commands')!.register({ name: 'local', description: 'This agent', handler: () => ({ kind: 'success' }) })
  await controller.drain()
  expect(controller.view.completion.entries.some(entry => entry.name === 'local')).toBe(true)
  expect(controller.view.completion.entries.some(entry => entry.name.startsWith('foreign'))).toBe(false)
  unregister()
  await controller.drain()
  expect(controller.view.completion.entries.some(entry => entry.name === 'local')).toBe(false)
})

it('ignores superseded discovery and stops observing after closure', async () => {
  const { ctx, controller, changed } = await connected()
  const started = Promise.withResolvers<void>()
  const stale = Promise.withResolvers<readonly SkillCandidate[]>()
  let invalidate!: () => void
  let calls = 0
  const candidate = (name: string): SkillCandidate => ({
    name, description: name, provider: 'test', source: 'runtime', rank: 1, locator: name,
    invocation: { userInvocable: true, modelInvocable: true },
  })
  ctx.skills.registerProvider(control => {
    invalidate = control.invalidate
    return {
      name: 'test',
      list: async () => { if (++calls === 1) { started.resolve(); return stale.promise }; return [candidate('current')] },
      get: async () => undefined,
    }
  })
  try {
    await started.promise
    invalidate()
    await controller.drain()
    expect(controller.view.completion.entries.some(entry => entry.name === 'current')).toBe(true)
    controller.close()
    const notifications = changed.mock.calls.length
    stale.resolve([candidate('stale')])
    await stale.promise
    await controller.drain()
    ctx.commands.register({ name: 'later', description: 'After closure', handler: () => ({ kind: 'success' }) })
    expect(changed).toHaveBeenCalledTimes(notifications)
    expect(controller.view.completion.entries.some(entry => entry.name === 'stale')).toBe(false)
  } finally { stale.resolve([]) }
})

it('reports incomplete skill discovery while keeping commands available', async () => {
  const { ctx, controller } = await connected()
  ctx.skills.registerProvider(() => ({ name: 'unavailable', list: async () => ({ candidates: [], complete: false }), get: async () => undefined }))
  await controller.drain()
  expect(controller.view.completion.error).toBe(dictionaries.en.catalogIncomplete)
  expect(controller.view.completion.loading).toBe(false)
  expect(controller.view.completion.entries.some(entry => entry.name === 'goal')).toBe(true)
})

it('aborts an active catalog read before draining terminal shutdown', async () => {
  const { ctx, controller, changed } = await connected()
  const started = Promise.withResolvers<AbortSignal>()
  const late = Promise.withResolvers<[]>()
  ctx.skills.registerProvider(() => ({
    name: 'delayed',
    list: async options => { started.resolve(options.signal!); return late.promise },
    get: async () => undefined,
  }))
  try {
    const signal = await started.promise
    controller.close()
    const notifications = changed.mock.calls.length
    await controller.drain()
    expect(signal.aborted).toBe(true)
    late.resolve([])
    await late.promise
    expect(changed).toHaveBeenCalledTimes(notifications)
  } finally { late.resolve([]) }
})

it('supersedes argument choices and drains an in-flight provider on close', async () => {
  const { ctx, controller, changed } = await connected()
  const started = Promise.withResolvers<AbortSignal>()
  const stale = Promise.withResolvers<readonly string[]>()
  ctx.commands.register({ name: 'choices', description: 'Choose', input: { hint: '<value>',
    choices: (_agent, partial, signal) => {
      if (partial === 'a') { started.resolve(signal); return stale.promise }
      return ['bee']
    } }, handler: () => ({ kind: 'success' }) })
  controller.argumentQuery({ name: 'choices', partial: 'a' })
  const signal = await started.promise
  controller.argumentQuery({ name: 'choices', partial: 'b' })
  await vi.waitFor(() => expect(controller.view.completion.argument?.entries).toEqual(['bee']))
  expect(signal.aborted).toBe(true)
  controller.close()
  const notifications = changed.mock.calls.length
  stale.resolve(['stale'])
  await controller.drain()
  expect(changed).toHaveBeenCalledTimes(notifications)
  expect(controller.view.completion.argument?.entries).toEqual(['bee'])
})

it('reuses a completed argument list until the menu closes', async () => {
  const { ctx, controller } = await connected()
  const choices = vi.fn(() => ['alpha', 'beta'])
  ctx.commands.register({ name: 'cached', description: 'Cached', input: { hint: '<value>', choices },
    handler: () => ({ kind: 'success' }) })
  controller.argumentQuery({ name: 'cached', partial: 'a' })
  await controller.drain()
  controller.argumentQuery({ name: 'cached', partial: 'b' })
  await controller.drain()
  expect(choices).toHaveBeenCalledTimes(1)
  controller.argumentQuery(undefined)
  controller.argumentQuery({ name: 'cached', partial: 'b' })
  await controller.drain()
  expect(choices).toHaveBeenCalledTimes(2)
})
