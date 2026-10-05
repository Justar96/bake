/** Model choices commit atomically through the live Harness selection reference. */
import { afterEach, describe, expect, it, vi } from 'vitest'
import type { ModelSelectionRef } from 'bake-agent'
import { ReasoningEffortId, type LlmResolvedModelInfo } from 'bake-llm'
import { formatRow, transcriptRows } from '@dsh-tui/ui'
import { dictionaries } from '@dsh-tui/ui/copy.ts'
import { openSession } from '../src/session.ts'
import { SessionController } from '../src/controller.ts'
import type { RecentModels } from '../src/model.ts'
import { harness, ScriptedModel, textResponse } from './harness.ts'

const cleanup: (() => Promise<void>)[] = []
/** The last command outcome, which commits under its command row. */
const outcome = (controller: SessionController): string | undefined => {
  const row = transcriptRows(controller.view.committed).at(-1)
  return row === undefined ? undefined : formatRow(row)
}
afterEach(async () => { for (const dispose of cleanup.splice(0).reverse()) await dispose() })

const info = (model: string): LlmResolvedModelInfo => ({
  provider: 'mock', id: model, name: model,
  ...model === 'plain' ? {} : { reasoning: {
    efforts: [{ id: ReasoningEffortId('low'), name: 'Low' }, { id: ReasoningEffortId('high'), name: 'High' }],
    defaultEffort: ReasoningEffortId('low'),
  } },
})

async function connected(recentModels?: RecentModels) {
  const fixture = await harness()
  cleanup.push(fixture.dispose)
  let controller!: SessionController
  let selection!: ModelSelectionRef
  vi.spyOn(fixture.model, 'listModels').mockResolvedValue(['model', 'other', 'plain'].map(id => ({ provider: 'mock', id, name: id })))
  const resolve = vi.spyOn(fixture.model, 'resolveModel').mockImplementation(async (_provider, model) => info(model))
  const handle = await openSession(fixture.ctx, {}, new AbortController().signal, (agent, ref) => {
    selection = ref
    controller = new SessionController(fixture.ctx, agent, dictionaries.en, { refs: [] }, () => {},
      { attachmentMaxBytes: 1048576, attachmentLimit: 8, ...recentModels === undefined ? {} : { recentModels } }, ref)
  })
  cleanup.push(async () => { controller.close(); await controller.drain(); await handle.dispose() })
  await controller.replay(new AbortController().signal)
  /** The open `/model` sheet. */
  const picker = async () => {
    await vi.waitFor(() => expect(controller.view.interaction).toMatchObject({ kind: 'select', tall: true }))
    const active = controller.view.interaction!
    if (active.kind !== 'select') throw new Error('Expected picker')
    return active
  }
  const chooseModel = async (route: string, level?: string) => {
    const active = await picker()
    controller.interactions.answer(active.id, level === undefined ? route : { value: route, level })
  }
  return { ...fixture, handle, controller, selection, resolve, picker, chooseModel }
}

describe('/model', () => {
  it('offers advertised routes once per active argument menu', async () => {
    const { controller, model } = await connected()
    controller.argumentQuery({ name: 'model', partial: 'mock/o' })
    await controller.drain()
    const values = () => controller.view.completion.argument?.entries.map(entry => typeof entry === 'string' ? entry : entry.value)
    expect(values()).toContain('mock/other')
    const reads = vi.mocked(model.listModels).mock.calls.length
    controller.argumentQuery({ name: 'model', partial: 'mock/p' })
    await controller.drain()
    expect(values()).toContain('mock/plain')
    expect(vi.mocked(model.listModels).mock.calls).toHaveLength(reads)
  })

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
        controller = new SessionController(fixture.ctx, agent, dictionaries.en, { refs: [] }, () => { paints += 1 },
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
    const routes = await picker()
    expect(routes.initial).toBe('mock/model')
    expect(routes.choices.find(choice => choice.value === 'mock/model')?.current).toBe(true)
    // The efforts ride on the sheet's levels row: one step, no second picker.
    expect(routes.choices.find(choice => choice.value === 'mock/other')?.levels?.items.map(item => item.value)).toEqual(['', 'low', 'high'])
    expect(selection.current).toEqual(before)
    expect(model.requests).toHaveLength(1)
    await chooseModel('mock/other', 'high')
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

  it('cancels the sheet without changing selection or sending input', async () => {
    const { controller, selection, model, picker } = await connected()
    const before = { ...selection.current }
    controller.submit('/model')
    await picker()
    controller.cancel()
    await controller.drain()
    expect(controller.view.interaction).toBeUndefined()
    expect(outcome(controller)).toContain(dictionaries.en.modelCancelled)
    expect(selection.current).toEqual(before)
    expect(model.requests).toHaveLength(0)
  })

  it('preselects explicit effort and restores provider defaults when requested', async () => {
    const { controller, model, selection, handle, picker, chooseModel } = await connected()
    controller.submit('/model mock/model high')
    await controller.drain()
    controller.submit('/model')
    const sheet = await picker()
    // The current model opens on its effort, and another that offers the same one opens on it too.
    expect(sheet.choices.find(choice => choice.value === 'mock/model')?.levels?.initial).toBe('high')
    expect(sheet.choices.find(choice => choice.value === 'mock/other')?.levels?.initial).toBe('high')
    await chooseModel('mock/model', '')
    await controller.drain()
    expect(selection.current).toEqual({ provider: 'mock', model: 'model' })
    expect(controller.view.thinkingLevel).toBe('low')
    controller.submit('Use the provider default')
    await handle.agent.whenIdle()
    expect(model.requests.at(-1)?.reasoningEffort).toBe('low')
    expect(handle.agent.session.requestHeader()?.adapterDefaults?.reasoningEffort).toBe(true)
  })

  it('accepts models without reasoning controls, which offer no levels', async () => {
    const { controller, selection, picker, chooseModel } = await connected()
    controller.submit('/model')
    expect((await picker()).choices.find(choice => choice.value === 'mock/plain')?.levels).toBeUndefined()
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
    const active = await picker()
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
    await picker()
    controller.close()
    await controller.drain()
    expect(controller.view.interaction).toBeUndefined()
    expect(selection.current?.model).toBe('model')
  })

  it('leads with Recent, then each provider newest first, and puts each choice first on Recent', async () => {
    const remembered: string[] = []
    const recent: RecentModels = { recentModels: ['mock/plain', 'gone/model'],
      rememberModel: async route => { remembered.push(route) } }
    const { controller, model, resolve, picker, chooseModel } = await connected(recent)
    const names: Record<string, string> = { model: 'Mock 2', old: 'Mock 1', other: 'Mock 3', plain: 'Plain' }
    vi.mocked(model.listModels).mockResolvedValue(['model', 'old', 'other', 'plain'].map(id => ({ provider: 'mock', id, name: names[id]!,
      ...id === 'plain' ? { inputModalities: ['text', 'image'] } : {} })))
    resolve.mockImplementation(async (_provider, id) => ({ ...info(id), name: names[id] ?? id }))
    controller.submit('/model')
    const sheet = await picker()
    // Recent holds the current model, then the routes chosen before that still exist, and nothing twice.
    expect(sheet.choices.map(choice => [choice.group, choice.value])).toEqual([
      [dictionaries.en.recentModels, 'mock/model'], [dictionaries.en.recentModels, 'mock/plain'],
      [expect.any(String), 'mock/other'], [expect.any(String), 'mock/old'],
    ])
    expect(sheet.title).toMatch(/^Model · 4 from /)
    expect(sheet.choices.find(choice => choice.value === 'mock/plain')?.facts).toEqual(['', '', dictionaries.en.factImage])
    expect(sheet.choices.find(choice => choice.value === 'mock/other')?.facts).toEqual(['', dictionaries.en.factThink, ''])
    await chooseModel('mock/other', 'low')
    await controller.drain()
    expect(remembered).toEqual(['mock/other'])
    controller.submit('/model mock/plain')
    await controller.drain()
    expect(remembered).toEqual(['mock/other', 'mock/plain'])
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
      expect(outcome(controller)).toContain(message!)
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
      resumed = new SessionController(ctx, agent, dictionaries.en, { refs: [] }, () => {}, { attachmentMaxBytes: 1048576, attachmentLimit: 8 }, ref)
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
      expect(outcome(controller)).toContain(dictionaries.en.modelBusy)
      expect(selection.current?.model).toBe('model')
      controller.submit('/model')
      await controller.drain()
      expect(controller.view.interaction).toBeUndefined()
      expect(outcome(controller)).toContain(dictionaries.en.modelBusy)
    } finally { resolved.resolve(info('other')); release.resolve(); await handle.agent.whenIdle() }
  })
})

describe('Shift-Tab thinking toggle', () => {
  it('steps through the route\'s efforts and wraps to the provider default, taken by the next request', async () => {
    const { controller, selection, model, handle } = await connected()
    await controller.drain()
    expect(selection.current).toEqual({ provider: 'mock', model: 'model' })
    const seen: (string | undefined)[] = []
    for (let press = 0; press < 3; press++) {
      controller.cycleThinking()
      seen.push(selection.current?.reasoningEffort)
    }
    expect(seen).toEqual(['low', 'high', undefined])
    expect(controller.view.notice).toBe(`${dictionaries.en.thinking}: ${dictionaries.en.providerDefault}`)
    controller.cycleThinking()
    controller.cycleThinking()
    expect(controller.view.notice).toBe(`${dictionaries.en.thinking}: High`)
    expect(controller.view.thinkingLevel).toBe('high')
    controller.submit('Think hard')
    await handle.agent.whenIdle()
    expect(model.requests.at(-1)).toMatchObject({ provider: 'mock', model: 'model', reasoningEffort: 'high' })
  })

  it('saves /model picks and each step as the new-session default, in order', async () => {
    const { ctx, controller } = await connected()
    await controller.drain()
    const saved = vi.spyOn(ctx.agentDefaultModel, 'saveSelection')
    controller.submit('/model mock/other high')
    await controller.drain()
    controller.cycleThinking()
    await controller.drain()
    expect(saved.mock.calls.map(([selection]) => selection)).toEqual([
      { provider: 'mock', model: 'other', reasoningEffort: 'high' },
      { provider: 'mock', model: 'other' },
    ])
  })

  it('says so for a route without efforts and changes nothing', async () => {
    const { controller, selection } = await connected()
    controller.submit('/model mock/plain')
    await controller.drain()
    controller.cycleThinking()
    expect(selection.current).toEqual({ provider: 'mock', model: 'plain' })
    expect(controller.view.notice).toBe(`${dictionaries.en.thinkingUnsupported}: mock/plain`)
  })
})

describe('/thinking', () => {
  it('sets an effort by id or name, and default restores the provider default', async () => {
    const { controller, selection, model, handle } = await connected()
    controller.submit('/thinking high')
    await controller.drain()
    expect(selection.current).toEqual({ provider: 'mock', model: 'model', reasoningEffort: 'high' })
    expect(outcome(controller)).toContain(`${dictionaries.en.thinking}: High`)
    controller.submit('/thinking Low')
    await controller.drain()
    expect(selection.current?.reasoningEffort).toBe('low')
    controller.submit('Think a little')
    await handle.agent.whenIdle()
    expect(model.requests.at(-1)).toMatchObject({ model: 'model', reasoningEffort: 'low' })
    controller.submit('/thinking default')
    await controller.drain()
    expect(selection.current).toEqual({ provider: 'mock', model: 'model' })
    expect(outcome(controller)).toContain(dictionaries.en.providerDefault)
  })

  it('refuses an effort the route does not offer, and a route without efforts', async () => {
    const { controller, selection } = await connected()
    controller.submit('/thinking extreme')
    await controller.drain()
    expect(outcome(controller)).toContain(`${dictionaries.en.unknownEffort}: low high default`)
    expect(selection.current).toEqual({ provider: 'mock', model: 'model' })
    controller.submit('/model mock/plain')
    await controller.drain()
    controller.submit('/thinking high')
    await controller.drain()
    expect(outcome(controller)).toContain(`${dictionaries.en.thinkingUnsupported}: mock/plain`)
  })

  it('chooses from the route\'s efforts without arguments', async () => {
    const { controller, selection } = await connected()
    await controller.drain()
    controller.submit('/thinking')
    await vi.waitFor(() => expect(controller.view.interaction).toBeDefined())
    const prompt = controller.view.interaction!
    expect(prompt).toMatchObject({ kind: 'select', title: dictionaries.en.thinkingTitle, initial: 'default' })
    controller.interactions.answer(prompt.id, 'high')
    await controller.drain()
    expect(selection.current?.reasoningEffort).toBe('high')
  })

  it('changes the effort during a turn, from its next step, while /model refuses', async () => {
    const { controller, selection, model, handle } = await connected()
    await controller.drain()
    const started = Promise.withResolvers<void>()
    const release = Promise.withResolvers<void>()
    model.response = async function* () { started.resolve(); await release.promise; yield* textResponse('Finished') }
    try {
      controller.submit('Start a turn')
      await started.promise
      controller.submit('/model mock/model high')
      await controller.drain()
      expect(outcome(controller)).toContain(dictionaries.en.modelBusy)
      controller.submit('/thinking high')
      await controller.drain()
      expect(outcome(controller)).toContain(`${dictionaries.en.thinking}: High · ${dictionaries.en.thinkingNextStep}`)
      expect(selection.current?.reasoningEffort).toBe('high')
    } finally { release.resolve(); await handle.agent.whenIdle() }
  })
})
