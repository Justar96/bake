/**
 * The terminal profile as it ships, mounted in-process for specs that read
 * what its agents send: `bake-base` with the terminal bundle's patch applied
 * by the Loader's own patch semantics, every row the composition leaves
 * active mounted through the Loader, and the shipped presets the runner
 * offers. Only the model, the persistence root, the workspace, and the
 * terminal surface itself are replaced.
 */
import { mkdir, mkdtemp, readFile, rm, symlink } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'
import * as yaml from 'js-yaml'
import { expect, vi } from 'vitest'
import { Context } from '@deepseek-ai/cordis'
import Include, { applyEntryPatches, entryListSchema, type PatchOptions } from '@deepseek-ai/cordis-plugin-include'
import Loader, { Group } from '@deepseek-ai/cordis-plugin-loader'
import { dshHomePath } from 'bake-home-paths'
import type { SessionEvent } from 'bake-session'
import { dictionaries } from 'bake-tui-ui/copy.ts'
import { SessionController } from '../src/controller.ts'
import { openSession } from '../src/session.ts'
import { ScriptedModel } from './harness.ts'

export const REPOSITORY = fileURLToPath(new URL('../../../../../', import.meta.url))

/** The layers the shipped `tui` profile applies over its empty root, in order. */
export const LAYERS = ['packages/bundle/base/cordis.patch.yml', 'apps/tui/packages/app/cordis.built.patch.yml']

/** The presets the terminal runner offers. */
export type ShippedPreset = 'standard' | 'ptc' | 'minimal' | 'cordis'

/** How one spec sets up the composed profile. */
export interface ComposedProfileOptions {
  /** Fill the private workspace before the profile mounts. */
  readonly plant: (workspace: string) => Promise<void>
  /** The default model id the overlay selects on the `mock` route; defaults to `model`. */
  readonly model?: string
}

/** @returns the shipped layers, parsed. */
export async function shippedLayers(): Promise<PatchOptions[][]> {
  return Promise.all(LAYERS.map(async path =>
    yaml.load(await readFile(join(REPOSITORY, path), 'utf8'), { schema: entryListSchema }) as PatchOptions[]))
}

/**
 * Boot the composed profile in a private home and workspace. The caller
 * registers `cleanup` and runs it after each test.
 * @returns the settled root context, the scripted model, a session opener, and the private paths.
 */
export async function composedProfile(cleanup: (() => Promise<void>)[], options: ComposedProfileOptions) {
  const root = await mkdtemp(join(tmpdir(), 'dsh-tui-plane-'))
  const ctx = new Context()
  cleanup.push(async () => { await ctx.fiber.dispose(); await rm(root, { recursive: true, force: true }) })
  const workspace = join(root, 'workspace')
  await mkdir(workspace, { recursive: true })
  await options.plant(workspace)
  const home = join(root, 'home')
  const agentsHome = join(root, 'agents')
  vi.stubEnv('DSH_HOME', home)
  vi.stubEnv('DSH_AGENTS_HOME', agentsHome)

  const layers = await shippedLayers()
  // What this test replaces: the terminal surface, the network model routes
  // and exporter, the title model call, the profile reload watcher, storage,
  // and the workspace.
  const overlay: PatchOptions[] = [
    ...['tui-startup', 'tui-runner', 'llm-pi-ai', 'session-title-llm', 'session-telemetry-otel', 'hmr']
      .map(id => ({ id, disabled: true })),
    { id: 'agent-default-model', config: { provider: 'mock', model: options.model ?? 'model' } },
    { id: 'session-persistence-jsonl', config: { root: join(root, 'sessions'), compression: 'none' } },
    { id: 'fs-sandbox', config: { cwd: workspace } },
    { id: 'sandbox-policy', config: { mode: 'workspace-write', workspaceRoot: workspace } },
  ]
  const skipped: string[] = []
  const rows = applyEntryPatches([], [...layers.flat(), ...overlay], message => { skipped.push(message) })
  expect(skipped).toEqual([])

  // Bare row names resolve from the Loader's base, here the private root, as
  // the launcher's resolve from the installation. Removing the root unlinks it.
  await symlink(join(REPOSITORY, 'node_modules'), join(root, 'node_modules'), 'junction')
  ctx.baseUrl = pathToFileURL(root).href + '/'
  ctx.provide('dshHomePath', dshHomePath)
  // What the launcher provides a profile it starts, here a private one. The
  // plugin manager mounts only under it, and the `cordis` preset's tool needs it.
  const profileDir = join(home, 'profiles', 'tui')
  ctx.provide('profileContext', {
    name: 'tui', dir: profileDir, patchPath: join(profileDir, 'cordis.patch.yml'),
    installAnchor: join(REPOSITORY, 'apps/cli/package.json'), cwd: workspace, home,
    startedBundles: ['bake-base', 'bake-tui-app'], overlays: [], telemetryDisabledEnv: undefined,
  })
  await ctx.plugin(Loader)
  ctx.loader.builtins.include = Include
  ctx.loader.builtins.group = Group
  for (const row of rows) await ctx.loader.create(row)
  await ctx.loader.await()

  const model = new ScriptedModel()
  ctx.llm.registerAdapter(['mock'], model)

  const open = async (preset: ShippedPreset) => {
    let controller!: SessionController
    const handle = await openSession(ctx, { preset }, new AbortController().signal, agent => {
      controller = new SessionController(ctx, agent, dictionaries.en, { refs: [] }, () => {},
        { attachmentMaxBytes: 1048576, attachmentLimit: 8 })
    })
    cleanup.push(async () => { controller.close(); await controller.drain(); await handle.dispose() })
    await controller.replay(new AbortController().signal)
    const turn = async (text: string) => {
      const before = model.requests.length
      expect(controller.submit(text)).toBe(true)
      await vi.waitFor(() => { expect(model.requests.length).toBeGreaterThan(before) })
      await handle.agent.whenIdle()
      return model.requests.slice(before)
    }
    const events = async (): Promise<readonly SessionEvent[]> => {
      using observation = await ctx.sessionQuery.observeSession(handle.agent.id, { projectionMode: 'none' })
      return [...observation.events]
    }
    return { controller, agent: handle.agent, turn, events }
  }
  return { ctx, model, open, workspace, root, home, agentsHome }
}
