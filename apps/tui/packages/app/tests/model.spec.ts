/** Model choices commit atomically through the live Harness selection reference. */
import { afterEach, describe, expect, it, vi } from 'vitest'
import type { ModelSelectionRef } from '@deepseek-ai/dsh-agent'
import { ReasoningEffortId, type LlmResolvedModelInfo } from '@deepseek-ai/dsh-llm'
import { dictionaries } from '@dsh-tui/ui/copy.ts'
import { openSession } from '../src/session.ts'
import { SessionController } from '../src/controller.ts'
import { harness, ScriptedModel, textResponse } from './harness.ts'

const cleanup: (() => Promise<void>)[] = []
afterEach(async () => { for (const dispose of cleanup.splice(0).reverse()) await dispose() })

const info = (model: string): LlmResolvedModelInfo => ({
  provider: 'mock', id: model, name: model,
  ...model === 'plain' ? {} : { reasoning: {
    efforts: [{ id: ReasoningEffortId('low'), name: 'Low' }, { id: ReasoningEffortId('high'), name: 'High' }],
    defaultEffort: ReasoningEffortId('low'),
  } },
})

async function connected() {
  const fixture = await harness()
  cleanup.push(fixture.dispose)
  let controller!: SessionController
  let selection!: ModelSelectionRef
  vi.spyOn(fixture.model, 'listModels').mockResolvedValue(['model', 'other', 'plain'].map(id => ({ provider: 'mock', id, name: id })))
  const resolve = vi.spyOn(fixture.model, 'resolveModel').mockImplementation(async (_provider, model) => info(model))
  const handle = await openSession(fixture.ctx, {}, new AbortController().signal, (agent, ref) => {
    selection = ref
    controller = new SessionController(fixture.ctx, agent, dictionaries.en, [], () => {}, { attachmentMaxBytes: 1048576, attachmentLimit: 8 }, ref)
  })
  cleanup.push(async () => { controller.close(); await controller.drain(); await handle.dispose() })
  await controller.replay(new AbortController().signal)
  const picker = async (title: string) => {
    await vi.waitFor(() => expect(controller.view.interaction).toMatchObject({ kind: 'select', title }))
    const active = controller.view.interaction!
    if (active.kind !== 'select') throw new Error('Expected picker')
    return active
  }
  const chooseModel = async (route: string) => {
    const active = await picker(dictionaries.en.chooseModel)
    controller.interactions.answer(active.id, route)
  }
  return { ...fixture, handle, controller, selection, resolve, picker, chooseModel }
}

describe('/model', () => {
  it('cancels the initial default lookup when the session closes', async () => {
    const fixture = await harness()
    const began = Promise.withResolvers<void>()
    const release = Promise.withResolvers<LlmResolvedModelInfo>()
    let controller: SessionController | undefined
    let handle: Awaited<ReturnType<typeof openSession>> | undefined
    let paints = 0
    try {
      vi.spyOn(fixture.model, 'resolveModel').mockImplementation(async () => {
        began.resolve()
        return release.promise
      })
      handle = await openSession(fixture.ctx, {}, new AbortController().signal, (agent, ref) => {
        controller = new SessionController(fixture.ctx, agent, dictionaries.en, [], () => { paints += 1 },
          { attachmentMaxBytes: 1048576, attachmentLimit: 8 }, ref)
      })
      await began.promise
      controller!.close()
      const atClose = paints
      release.resolve(info('model'))
      await controller!.drain()
      expect(controller!.view.thinkingLevel).toBeUndefined()
      expect(paints).toBe(atClose)
    } finally {
      release.resolve(info('model'))
      controller?.close()
      await controller?.drain()
      await handle?.dispose()
      await fixture.dispose()
    }
  })

  it('commits model and effort together, then logs the exact request configuration', async () => {
    const { controller, selection, model, handle, picker, chooseModel } = await connected()
    controller.submit('First turn')
    await handle.agent.whenIdle()
    const before = { ...selection.current }
    controller.submit('/model')
    const routes = await picker(dictionaries.en.chooseModel)
    expect(routes.initial).toBe('mock/model')
    expect(routes.choices.find(choice => choice.value === 'mock/model')?.current).toBe(true)
    await chooseModel('mock/other')
    const efforts = await picker('Choose reasoning effort: mock/other')
    expect(efforts.choices.map(choice => choice.value)).toEqual(['', 'low', 'high'])
    expect(selection.current).toEqual(before)
    expect(model.requests).toHaveLength(1)
    controller.interactions.answer(efforts.id, 'high')
    await controller.drain()
    expect(selection.current).toEqual({ provider: 'mock', model: 'other', reasoningEffort: 'high' })
    expect(controller.view.model).toBe('mock/other')
    expect(controller.view.thinkingLevel).toBe('high')
    controller.submit('Use the selected model')
    await handle.agent.whenIdle()
    expect(model.requests.at(-1)).toMatchObject({ provider: 'mock', model: 'other', reasoningEffort: 'high' })
    const events = handle.agent.session.snapshotEvents()
    const notice = events.flatMap(event => event.type === 'user/message' && event.data.source.kind === 'plugin'
      && event.data.source.plugin === 'model-selection' ? event.data.content : [])
    await expect(JSON.stringify({ config: handle.agent.session.requestHeader()?.config, notice }, null, 2) + '\n')
      .toMatchFileSnapshot('./expected/model-selection.json')
  })

  it.each(['model', 'effort'] as const)('cancels the %s stage without changing selection or sending input', async stage => {
    const { controller, selection, model, picker, chooseModel } = await connected()
    const before = { ...selection.current }
    controller.submit('/model')
    const first = await picker(dictionaries.en.chooseModel)
    if (stage === 'effort') {
      await chooseModel('mock/other')
      const second = await picker('Choose reasoning effort: mock/other')
      controller.interactions.answer(first.id, 'mock/plain')
      expect(controller.view.interaction?.id).toBe(second.id)
    }
    controller.cancel()
    await controller.drain()
    expect(controller.view.interaction).toBeUndefined()
    expect(controller.view.notice).toBe(dictionaries.en.modelCancelled)
    expect(selection.current).toEqual(before)
    expect(model.requests).toHaveLength(0)
  })

  it('preselects explicit effort and restores provider defaults when requested', async () => {
    const { controller, model, selection, handle, picker, chooseModel } = await connected()
    controller.submit('/model mock/model high')
    await controller.drain()
    controller.submit('/model')
    await chooseModel('mock/model')
    const efforts = await picker('Choose reasoning effort: mock/model')
    expect(efforts.initial).toBe('high')
    controller.interactions.answer(efforts.id, '')
    await controller.drain()
    expect(selection.current).toEqual({ provider: 'mock', model: 'model' })
    expect(controller.view.thinkingLevel).toBe('low')
    controller.submit('Use the provider default')
    await handle.agent.whenIdle()
    expect(model.requests.at(-1)?.reasoningEffort).toBe('low')
    expect(handle.agent.session.requestHeader()?.adapterDefaults?.reasoningEffort).toBe(true)
  })

  it('accepts models without reasoning controls without an extra picker', async () => {
    const { controller, selection, chooseModel } = await connected()
    controller.submit('/model')
    await chooseModel('mock/plain')
    await controller.drain()
    expect(controller.view.interaction).toBeUndefined()
    expect(selection.current).toEqual({ provider: 'mock', model: 'plain' })
    expect(controller.view.thinkingLevel).toBeUndefined()
  })

  it('names a provider default when the adapter advertises no specific level', async () => {
    const { controller, resolve } = await connected()
    resolve.mockImplementation(async (_provider, model) => model === 'other'
      ? { ...info(model), reasoning: { efforts: info(model).reasoning!.efforts } }
      : info(model))
    controller.submit('/model mock/other')
    await controller.drain()
    expect(controller.view.thinkingLevel).toBe(dictionaries.en.providerDefault)
  })

  it('reports partial catalog failure and retains an unadvertised current route', async () => {
    const { ctx, controller, model, picker } = await connected()
    vi.mocked(model.listModels).mockResolvedValue([])
    const unavailable = new ScriptedModel()
    vi.spyOn(unavailable, 'listModels').mockRejectedValue(new Error('Offline'))
    ctx.llm.registerAdapter(['offline'], unavailable)
    controller.submit('/model')
    const active = await picker(dictionaries.en.chooseModel)
    expect(active.choices.map(choice => choice.value)).toEqual(['mock/model'])
    expect(active.warning).toBe('Unavailable model catalogs: offline')
    controller.cancel()
    await controller.drain()
  })

  it('drains canceled discovery and never opens a late picker', async () => {
    const { controller, model, selection } = await connected()
    const started = Promise.withResolvers<void>()
    const result = Promise.withResolvers<[]>()
    vi.mocked(model.listModels).mockImplementation(async () => { started.resolve(); return result.promise })
    try {
      controller.submit('/model')
      await started.promise
      controller.cancel()
      result.resolve([])
      await controller.drain()
      expect(controller.view.interaction).toBeUndefined()
      expect(selection.current?.model).toBe('model')
    } finally { result.resolve([]) }
  })

  it('withdraws an open picker and settles the command during terminal shutdown', async () => {
    const { controller, selection, picker } = await connected()
    controller.submit('/model')
    await picker(dictionaries.en.chooseModel)
    controller.close()
    await controller.drain()
    expect(controller.view.interaction).toBeUndefined()
    expect(selection.current?.model).toBe('model')
  })

  it('rejects unknown routes, unsupported efforts, and excess arguments without mutation', async () => {
    const { controller, selection } = await connected()
    const before = { ...selection.current }
    for (const [command, message] of [
      ['/model nonesuch/model', dictionaries.en.unknownModel],
      ['/model mock/model extreme', dictionaries.en.unknownEffort],
      ['/model mock/plain high', dictionaries.en.unknownEffort],
      ['/model mock/model high ignored', dictionaries.en.modelUsage],
    ]) {
      controller.submit(command!)
      await controller.drain()
      expect(controller.view.notice).toContain(message!)
      expect(selection.current).toEqual(before)
    }
  })

  it.each([undefined, 'high'] as const)('restores the last requested route and %s effort semantics on resume', async effort => {
    const { ctx, controller, handle, model } = await connected()
    controller.submit(`/model mock/other${effort === undefined ? '' : ` ${effort}`}`)
    await controller.drain()
    controller.submit('Persist the chosen model')
    await handle.agent.whenIdle()
    const id = handle.agent.id
    controller.submit('/model mock/plain')
    await controller.drain()
    controller.close()
    await handle.dispose()
    let restored!: ModelSelectionRef
    let resumed!: SessionController
    const next = await openSession(ctx, { resume: id }, new AbortController().signal, (agent, ref) => {
      restored = ref
      resumed = new SessionController(ctx, agent, dictionaries.en, [], () => {}, { attachmentMaxBytes: 1048576, attachmentLimit: 8 }, ref)
    })
    cleanup.push(async () => { resumed.close(); await resumed.drain(); await next.dispose() })
    expect(restored.current).toEqual({ provider: 'mock', model: 'other', ...effort === undefined ? {} : { reasoningEffort: effort } })
    await resumed.drain()
    expect(resumed.view.thinkingLevel).toBe(effort ?? 'low')
    resumed.submit('Continue with the recorded model')
    await next.agent.whenIdle()
    expect(model.requests.at(-1)).toMatchObject({ model: 'other', reasoningEffort: effort ?? 'low' })
  })

  it('refuses a switch if a turn starts while exact model resolution is pending', async () => {
    const { controller, selection, model, handle, resolve } = await connected()
    const resolving = Promise.withResolvers<AbortSignal | undefined>()
    const resolved = Promise.withResolvers<LlmResolvedModelInfo>()
    const started = Promise.withResolvers<void>()
    const release = Promise.withResolvers<void>()
    resolve.mockImplementation(async (_provider, name, signal) => {
      if (name === 'other') { resolving.resolve(signal); return resolved.promise }
      return info(name)
    })
    model.response = async function* () { started.resolve(); await release.promise; yield* textResponse('Finished') }
    try {
      controller.submit('/model mock/other high')
      const signal = await resolving.promise
      controller.submit('Start a turn during discovery')
      await started.promise
      expect(signal?.aborted).toBe(true)
      resolved.resolve(info('other'))
      await controller.drain()
      expect(controller.view.notice).toBe(dictionaries.en.modelBusy)
      expect(selection.current?.model).toBe('model')
      controller.submit('/model')
      await controller.drain()
      expect(controller.view.interaction).toBeUndefined()
      expect(controller.view.notice).toBe(dictionaries.en.modelBusy)
    } finally { resolved.resolve(info('other')); release.resolve(); await handle.agent.whenIdle() }
  })
})
