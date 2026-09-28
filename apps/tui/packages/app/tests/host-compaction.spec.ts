/**
 * The host compaction rows as the shipped terminal profile composes them:
 * `dsh-base` with the terminal bundle's patch applied by the Loader's own patch
 * semantics, mounted through the Loader. Automatic compaction reaches a preset
 * agent only through its preset's own engine, a preset without one never
 * compacts on its own, and `/compact` works under both. Only the model is
 * scripted.
 */
import { mkdir, readFile, symlink, writeFile } from 'node:fs/promises'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'
import * as yaml from 'js-yaml'
import { afterEach, expect, it, vi } from 'vitest'
import { applyEntryPatches, entryListSchema, type PatchOptions } from '@deepseek-ai/cordis-plugin-include'
import { Group, type EntryOptions } from '@deepseek-ai/cordis-plugin-loader'
import { SHIPPED_PRESET_ROOT } from '@deepseek-ai/dsh-agent-presets'
import type { GenerateOptions, StreamChunk } from '@deepseek-ai/dsh-llm'
import type { SessionEvent } from '@deepseek-ai/dsh-session'
import { dictionaries } from '@dsh-tui/ui/copy.ts'
import { SessionController } from '../src/controller.ts'
import { openSession } from '../src/session.ts'
import { harness, textResponse } from './harness.ts'

const REPOSITORY = fileURLToPath(new URL('../../../../../', import.meta.url))

/** Each shipped layer's patch file, relative to the repository root. */
const LAYERS = {
  base: 'packages/bundle/base/cordis.patch.yml',
  // The bundle patch the shipped `tui` profile applies; a source launch applies the next.
  built: 'apps/tui/packages/app/cordis.built.patch.yml',
  source: 'apps/tui/packages/app/cordis.patch.yml',
  headless: 'packages/bundle/headless/cordis.patch.yml',
  desktop: 'packages/bundle/desktop/cordis.patch.yml',
} as const

/** The base rows that own compaction on the host plane. */
const HOST_ROWS = ['token-meter', 'tool-result-pruner', 'compaction-basic', 'command-compact']

async function load(path: string): Promise<unknown> {
  return yaml.load(await readFile(path, 'utf8'), { schema: entryListSchema })
}

/** @returns the rows `dsh-base` inserts, after the named layer's patch. */
async function composed(layer: Exclude<keyof typeof LAYERS, 'base'>): Promise<EntryOptions[]> {
  const patches = await Promise.all([LAYERS.base, LAYERS[layer]].map(async path => await load(join(REPOSITORY, path)) as PatchOptions[]))
  return applyEntryPatches([], patches.flat(), () => {})
}

/** @returns one shipped preset's composition rows. */
async function shipped(preset: string): Promise<EntryOptions[]> {
  return await load(join(SHIPPED_PRESET_ROOT, preset, 'agent.cordis.yml')) as EntryOptions[]
}

/**
 * One prompt of about 1800 tokens. Four of them cross the default pressure
 * threshold, 80% of the 8192-token window, so the fifth turn's first step
 * finds earlier turns outside the retained tail; one alone is below it, so
 * only `/compact` folds it.
 */
const CHUNK = 'Older conversation history that the summary can fold. '.repeat(135)
const TURNS = 5

const cleanup: (() => Promise<void>)[] = []
afterEach(async () => { for (const dispose of cleanup.splice(0).reverse()) await dispose() })

it('turns the host engine manual in both terminal patches, and leaves headless and desktop automatic', async () => {
  for (const layer of ['built', 'source'] as const) {
    const rows = await composed(layer)
    expect(rows.find(row => row.id === 'compaction-basic'), layer).toMatchObject({ config: { auto: false } })
    expect(rows.find(row => row.id === 'compaction-basic')?.disabled, layer).toBeFalsy()
    // Kept for the host `/compact`.
    expect(rows.find(row => row.id === 'command-compact')?.disabled, layer).toBeFalsy()
  }
  for (const layer of ['headless', 'desktop'] as const) {
    const row = (await composed(layer)).find(entry => entry.id === 'compaction-basic')
    expect(row?.disabled, layer).toBeFalsy()
    expect((row?.config as { auto?: boolean } | undefined)?.auto, layer).not.toBe(false)
  }
})

/**
 * Boot the harness with the composed host rows mounted through the Loader, and
 * two presets: `standard`, carrying the shipped `standard` preset's own
 * compaction group verbatim, and `minimal`, the shipped `minimal` composition
 * without the persistent shell, which needs terminal services the harness
 * does not mount.
 */
async function terminal() {
  const fixture = await harness()
  cleanup.push(fixture.dispose)
  // Bare row names resolve from the Loader's base, here the fixture root, as
  // the launcher's resolve from the installation. Removing the root unlinks it.
  await mkdir(join(fixture.root, 'node_modules'))
  await symlink(join(REPOSITORY, 'node_modules', '@deepseek-ai'), join(fixture.root, 'node_modules', '@deepseek-ai'), 'junction')
  fixture.ctx.loader.builtins.group = Group

  const group = (await shipped('standard')).find(row => row.id === 'compaction')
  expect(group?.isolate).toMatchObject({ compaction: true })
  await mkdir(join(fixture.root, 'presets', 'standard'))
  await writeFile(join(fixture.root, 'presets', 'standard', 'agent.cordis.yml'), JSON.stringify([group]))
  const minimal = (await shipped('minimal')).filter(row => row.id !== 'persistent-shell')
  expect(JSON.stringify(minimal)).not.toContain('compaction')
  await mkdir(join(fixture.root, 'presets', 'minimal'))
  await writeFile(join(fixture.root, 'presets', 'minimal', 'agent.cordis.yml'), JSON.stringify(minimal))

  const rows = await composed('built')
  for (const id of HOST_ROWS) {
    const row = rows.find(entry => entry.id === id)
    if (row === undefined) throw new Error(`dsh-base must insert ${id}`)
    await fixture.ctx.loader.create(row)
  }
  await fixture.ctx.loader.await()
  const host = fixture.ctx.get('compaction')
  if (host === undefined) throw new Error('the host compaction row did not mount')

  fixture.model.response = async function* (options: GenerateOptions): AsyncGenerator<StreamChunk> {
    if (options.purpose === 'compaction') {
      yield* textResponse('The earlier turn asked for history to be folded.')
      return
    }
    yield { type: 'usage', usage: { inputTokens: 900, outputTokens: 20 } }
    yield* textResponse('Recorded answer')
  }

  const open = async (preset: 'standard' | 'minimal') => {
    let controller!: SessionController
    const handle = await openSession(fixture.ctx, { preset }, new AbortController().signal, agent => {
      controller = new SessionController(fixture.ctx, agent, dictionaries.en, [], () => {},
        { attachmentMaxBytes: 1048576, attachmentLimit: 8 })
    })
    cleanup.push(async () => { controller.close(); await controller.drain(); await handle.dispose() })
    await controller.replay(new AbortController().signal)
    const events = async (): Promise<readonly SessionEvent[]> => {
      using observation = await fixture.ctx.sessionQuery.observeSession(handle.agent.id, { projectionMode: 'none' })
      return [...observation.events]
    }
    const turn = async (text: string) => {
      controller.submit(text)
      await handle.agent.whenIdle()
    }
    /** @returns the log after `/compact` settles, once it reports a reduction. */
    const compact = async () => {
      controller.submit('/compact')
      await controller.drain()
      const log = await events()
      expect(log.find(event => event.type === 'command/done')?.data)
        .toMatchObject({ kind: 'success', text: expect.stringMatching(/^Compacted \d+ history items/u) })
      return log
    }
    return { controller, events, turn, compact, engine: fixture.ctx.agentPresets.serviceFor(handle.agent, 'compaction') }
  }
  return { ...fixture, host, open }
}

/** @returns each committed summary's owner: the turn that compacted itself, or `null` for `/compact`. */
function compactions(events: readonly SessionEvent[]): (number | null)[] {
  let owner: number | null = null
  return events.flatMap(event => {
    if (event.type === 'compaction/start') owner = event.data.turn
    return event.type === 'compaction/summary' ? [owner] : []
  })
}

/**
 * Call counts only: the calls carry live Agents, whose Cordis proxies a
 * failure diff cannot print.
 */
function calls(spies: Record<string, { mock: { calls: unknown[] } }>): Record<string, number> {
  return Object.fromEntries(Object.entries(spies).map(([name, spy]) => [name, spy.mock.calls.length]))
}

it('compacts a standard-preset agent once, through its own engine alone, and /compact still compacts', async () => {
  const { host, open } = await terminal()
  const route = { provider: 'mock', model: 'model' }
  expect(host.pressureThreshold(route, 8192)).toBeUndefined()
  const hostPressure = vi.spyOn(host, 'compactIfNeeded')

  const automatic = await open('standard')
  const own = automatic.engine
  if (own === undefined) throw new Error('the standard preset did not mount its own engine')
  expect(own.pressureThreshold(route, 8192)).toBe(6553)
  const ownPressure = vi.spyOn(own, 'compactIfNeeded')
  await automatic.turn(CHUNK)
  // The status line marks the preset's threshold; the host reports none.
  expect(automatic.controller.view.context).toMatchObject({ window: 8192, compactAt: 6553 })
  for (let turn = 2; turn <= TURNS; turn++) await automatic.turn(CHUNK)
  // One pressure check per step, every one the preset's own, and one compaction.
  expect(calls({ own: ownPressure, host: hostPressure })).toEqual({ own: TURNS, host: 0 })
  expect(compactions(await automatic.events())).toEqual([TURNS])

  // The preset's own `/compact` shadows the host command.
  const ownManual = vi.spyOn(own, 'compactNow')
  const hostManual = vi.spyOn(host, 'compactNow')
  const manual = await open('standard')
  await manual.turn(CHUNK)
  expect(compactions(await manual.events())).toEqual([])
  expect(compactions(await manual.compact())).toEqual([null])
  expect(calls({ own: ownManual, host: hostManual })).toEqual({ own: 1, host: 0 })
})

it('never compacts a minimal-preset agent on its own, and /compact reaches the host engine', async () => {
  const { host, open } = await terminal()
  const hostPressure = vi.spyOn(host, 'compactIfNeeded')
  const hostManual = vi.spyOn(host, 'compactNow')

  const automatic = await open('minimal')
  expect(automatic.engine).toBeUndefined()
  await automatic.turn(CHUNK)
  // Neither engine reports a start, so none is shown.
  expect(automatic.controller.view.context).toMatchObject({ window: 8192 })
  expect(automatic.controller.view.context).not.toHaveProperty('compactAt')
  for (let turn = 2; turn <= TURNS; turn++) await automatic.turn(CHUNK)
  expect(compactions(await automatic.events())).toEqual([])

  const manual = await open('minimal')
  await manual.turn(CHUNK)
  expect(compactions(await manual.compact())).toEqual([null])
  expect(calls({ pressure: hostPressure, manual: hostManual })).toEqual({ pressure: 0, manual: 1 })
})
