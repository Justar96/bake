/** A session with no default provider starts on no model, and its first sign-in chooses one. */
import { afterEach, expect, it, vi } from 'vitest'
import { ReasoningEffortId } from 'bake-llm'
import type { ModelSelectionRef } from 'bake-agent'
import { formatRow, transcriptRows } from '@dsh-tui/ui'
import { dictionaries } from '@dsh-tui/ui/copy.ts'
import { openSession } from '../src/session.ts'
import { SessionController } from '../src/controller.ts'
import type { LoginSources } from '../src/login.ts'
import { harness, ScriptedModel } from './harness.ts'
import { MemoryCredentials } from './memory-credentials.ts'

const copy = dictionaries.en
const cleanup: (() => Promise<void>)[] = []
afterEach(async () => { for (const dispose of cleanup.splice(0).reverse()) await dispose() })

/** The last command outcome, which commits under its command row. */
const outcome = (controller: SessionController): string | undefined => {
  const row = transcriptRows(controller.view.committed).at(-1)
  return row === undefined ? undefined : formatRow(row)
}

/** A route whose key the profile names, as the shipped profile names DeepSeek's. */
const SOURCES: LoginSources = { refs: [{ ref: 'LISTED_API_KEY', label: 'Listed', provider: 'listed' }] }

async function modelless(seed: Record<string, string> = {}) {
  const fixture = await harness({ defaultModel: false })
  cleanup.push(fixture.dispose)
  await fixture.ctx.plugin(MemoryCredentials, seed)
  const listed = new ScriptedModel()
  vi.spyOn(listed, 'listModels').mockResolvedValue([{ provider: 'listed', id: 'first', name: 'First' }])
  vi.spyOn(listed, 'resolveModel').mockResolvedValue({ provider: 'listed', id: 'first', name: 'First', inputModalities: ['text'],
    context: { contextWindow: 8192 }, reasoning: {
      efforts: [{ id: ReasoningEffortId('low'), name: 'Low' }], defaultEffort: ReasoningEffortId('low'),
    } })
  fixture.ctx.llm.registerAdapter(['listed'], listed)
  let controller!: SessionController
  let selection!: ModelSelectionRef
  const handle = await openSession(fixture.ctx, {}, new AbortController().signal, (agent, ref) => {
    selection = ref
    controller = new SessionController(fixture.ctx, agent, copy, SOURCES, () => {},
      { attachmentMaxBytes: 1048576, attachmentLimit: 8 }, ref)
  }, SOURCES)
  cleanup.push(async () => { controller.close(); await controller.drain(); await handle.dispose() })
  await controller.replay(new AbortController().signal)
  return { ...fixture, listed, controller, selection }
}

it('starts on no model, keeps a message in the composer, and puts /login first', async () => {
  const { controller, selection, model, listed } = await modelless()
  expect(selection.current).toBeUndefined()
  expect(controller.view.model).toBeUndefined()
  await controller.reportCredentials()
  expect(controller.view.notice).toBe(copy.noCredentials)
  expect(controller.view.completion.first).toEqual(['login'])
  expect(controller.submit('hello')).toBe(false)
  expect(controller.view.notice).toBe(copy.noModelSubmit)
  expect([...model.requests, ...listed.requests]).toEqual([])
})

it('selects the provider a first sign-in unlocks, and only reports a later one', async () => {
  const { ctx, controller, selection } = await modelless()
  await controller.reportCredentials()
  controller.submit('/login listed')
  await vi.waitFor(() => expect(controller.view.interaction).toMatchObject({ kind: 'login', secret: true, title: 'Sign in \u00b7 Listed' }))
  controller.interactions.answer(controller.view.interaction!.id, 'sk-listed')
  await controller.drain()
  await vi.waitFor(() => expect(outcome(controller)).toContain(`Listed: ${copy.signedIn} \u00b7 ${copy.nowUsing} listed/first`))
  expect(selection.current).toEqual({ provider: 'listed', model: 'first' })
  expect(controller.view.model).toBe('listed/first')
  expect(controller.view.completion.first).toBeUndefined()
  controller.cycleThinking()
  expect(controller.view.notice).toContain(`${copy.thinking}: Low`)
  expect(await ctx.credentials.resolve('LISTED_API_KEY' as never)).toMatchObject({ value: 'sk-listed' })

  controller.submit('/login listed')
  await vi.waitFor(() => expect(controller.view.interaction?.kind).toBe('login'))
  controller.interactions.answer(controller.view.interaction!.id, 'sk-again')
  await controller.drain()
  await vi.waitFor(() => expect(outcome(controller)).toContain(`Listed: ${copy.stored}`))
  expect(selection.current).toMatchObject({ provider: 'listed', model: 'first' })
})

it('starts on a signed-in provider, and names the model a sign-out strands', async () => {
  const { controller, selection } = await modelless({ LISTED_API_KEY: 'sk-listed' })
  expect(selection.current).toEqual({ provider: 'listed', model: 'first' })
  controller.submit('/logout')
  await vi.waitFor(() => expect(controller.view.interaction).toMatchObject({ kind: 'select', title: copy.chooseLogout,
    choices: [{ value: 'listed', label: 'Listed', status: { text: copy.configured } }] }))
  controller.interactions.answer(controller.view.interaction!.id, 'listed')
  await controller.drain()
  await vi.waitFor(() => expect(outcome(controller)).toContain(`Listed: ${copy.keyRemoved} \u00b7 ${copy.modelNeedsProvider}`))
  controller.submit('/logout')
  await controller.drain()
  await vi.waitFor(() => expect(outcome(controller)).toContain(copy.nothingToSignOut))
  controller.submit('/logout lsited')
  await controller.drain()
  await vi.waitFor(() => expect(outcome(controller)).toContain(`${copy.unknownLogoutTarget}: lsited`))
})

it('describes each target in the /login argument menu', async () => {
  const { controller } = await modelless()
  controller.argumentQuery({ name: 'login', partial: '' })
  await controller.drain()
  expect(controller.view.completion.argument?.entries).toEqual([
    { value: 'listed', description: `Listed \u00b7 ${copy.notSet} \u00b7 LISTED_API_KEY` },
    // This harness mounts no settings, so nothing here can write the proxy's route.
    { value: 'cliproxyapi', description: `CLIProxyAPI \u00b7 ${copy.notSet} \u00b7 ${copy.cliProxySetupHint} \u00b7 ${copy.readOnly}` },
  ])
})
