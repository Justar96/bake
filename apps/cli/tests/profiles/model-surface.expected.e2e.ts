/**
 * Model-surface snapshots for the shipped `headless` and `desktop` profiles:
 * the exact system prompt, context messages, and tool declarations each one
 * sends on its first model request.
 *
 * Each case launches the built `dsh --profile <name>` with one patch that
 * swaps the default model for a keyless adapter recording the first
 * agent-loop request, so the composition, its bundles, and its driver are the
 * shipped ones. The recorded request is normalized (paths, homes, the model
 * id, dates) and compared with `expected/model-surface/<name>.md`.
 *
 * Update after an intended change with
 * `bun run test:integration apps/cli/tests/profiles/model-surface.expected.e2e.ts -u`.
 * Any diff here is a model-visible change and needs an eval record.
 *
 * Windows ships `pwsh` in place of `bash`, a different tool, so the snapshots
 * are recorded on POSIX and the cases skip on Windows.
 */

import { spawn } from 'node:child_process'
import { mkdir, mkdtemp, readFile, realpath, rm } from 'node:fs/promises'
import { homedir, tmpdir } from 'node:os'
import { join } from 'node:path'
import { createInterface } from 'node:readline'
import { fileURLToPath } from 'node:url'
import { afterEach, describe, expect, it } from 'vitest'
import {
  MODEL_SURFACE_TASK,
  normalizeModelSurface,
  pathPlaceholders,
  plantModelSurfaceWorkspace,
  renderModelSurface,
  resolveExampleLaunch,
  type ModelSurfaceRequest,
} from 'bake-loader-smoke'

const binScript = fileURLToPath(new URL('../../src/bin.ts', import.meta.url))
const capturePatch = fileURLToPath(new URL('./fixtures/surface-capture.patch.yml', import.meta.url))
const expected = (name: string) => fileURLToPath(new URL(`./expected/model-surface/${name}.md`, import.meta.url))

/** The model id the capture patch selects; the persona names it, so it is normalized too. */
const MODEL_ID = 'surface-model'
const PROCESS_TIMEOUT_MS = 60_000
const TEST_TIMEOUT_MS = PROCESS_TIMEOUT_MS + 30_000

const cleanups: (() => Promise<unknown>)[] = []
afterEach(async () => {
  for (const cleanup of cleanups.splice(0).reverse()) await cleanup()
})

/** A private root holding the workspace, both homes, and the capture file. */
interface SurfaceWorld {
  readonly root: string
  readonly workspace: string
  readonly home: string
  readonly agentsHome: string
  readonly capture: string
}

async function world(): Promise<SurfaceWorld> {
  // The real path, so the desktop bridge sees its cwd as the workspace it is given.
  const root = await realpath(await mkdtemp(join(tmpdir(), 'bake-model-surface-')))
  cleanups.push(() => rm(root, { recursive: true, force: true }))
  const surface = {
    root,
    workspace: join(root, 'workspace'),
    home: join(root, 'home'),
    agentsHome: join(root, 'agents'),
    capture: join(root, 'request.json'),
  }
  await mkdir(surface.workspace)
  await plantModelSurfaceWorkspace(surface.workspace)
  return surface
}

/**
 * Start the built launcher on one shipped profile with only the capture patch.
 * Every ambient `BAKE_*` and `DSH_*` name is dropped, since several of them
 * (permission mode, tool mode) change what the model is sent.
 */
function launch(profile: string, args: readonly string[], surface: SurfaceWorld) {
  const env: NodeJS.ProcessEnv = { ...process.env }
  for (const name of Object.keys(env)) {
    if (/^(BAKE|DSH)_/iu.test(name)) Reflect.deleteProperty(env, name)
  }
  const resolved = resolveExampleLaunch({
    srcBin: binScript,
    // Profile plugins always load from built `lib/`; a source entry would load a second copy.
    mode: 'lib',
    configArgs: ['--profile', profile, '--patch', capturePatch, ...args],
    env: {
      BAKE_HOME: surface.home,
      BAKE_AGENTS_HOME: surface.agentsHome,
      BAKE_TELEMETRY_DISABLED: '1',
      BAKE_SURFACE_CAPTURE: surface.capture,
    },
  })
  for (const [name, value] of Object.entries(resolved.env)) {
    if (value === undefined) Reflect.deleteProperty(env, name)
    else env[name] = value
  }
  const child = spawn(resolved.command, resolved.args, { cwd: surface.workspace, env, stdio: ['pipe', 'pipe', 'pipe'] })
  let stderr = ''
  child.stderr.on('data', (chunk: Buffer) => { stderr += chunk.toString() })
  const exited = new Promise<number | null>((resolve) => { child.on('exit', resolve) })
  const timer = setTimeout(() => { child.kill('SIGKILL') }, PROCESS_TIMEOUT_MS)
  cleanups.push(async () => {
    clearTimeout(timer)
    if (child.exitCode === null && child.signalCode === null) child.kill('SIGKILL')
    await exited
  })
  return { child, exited, stderr: () => stderr }
}

/** @returns the recorded request, normalized and rendered. */
async function snapshot(title: string, surface: SurfaceWorld): Promise<string> {
  const request = JSON.parse(await readFile(surface.capture, 'utf8')) as ModelSurfaceRequest
  return renderModelSurface(title, normalizeModelSurface(request, [
    ...pathPlaceholders(surface.workspace, '<cwd>'),
    ...pathPlaceholders(surface.home, '<bake-home>'),
    ...pathPlaceholders(surface.agentsHome, '<agents-home>'),
    ...pathPlaceholders(surface.root, '<root>'),
    ...pathPlaceholders(tmpdir(), '<tmp>'),
    ...pathPlaceholders(homedir(), '<home>'),
    [MODEL_ID, '<model>'],
  ]))
}

describe.skipIf(process.platform === 'win32')('shipped model surface', () => {
  it('pins the headless profile', async () => {
    const surface = await world()
    const run = launch('headless', [MODEL_SURFACE_TASK], surface)
    run.child.stdin.end()
    expect(await run.exited, run.stderr()).toBe(0)
    await expect(await snapshot('headless (bake-base + bake-headless)', surface)).toMatchFileSnapshot(expected('headless'))
  }, TEST_TIMEOUT_MS)

  it('pins the desktop profile', async () => {
    const surface = await world()
    const run = launch('desktop', [], surface)
    const messages: { type: string }[] = []
    const waiters = new Set<() => void>()
    createInterface({ input: run.child.stdout }).on('line', (line) => {
      messages.push(JSON.parse(line) as { type: string })
      for (const waiter of waiters) waiter()
    })
    const waitFor = (type: string) => new Promise<void>((resolve, reject) => {
      const check = () => {
        if (!messages.some(message => message.type === type)) return false
        waiters.delete(check)
        resolve()
        return true
      }
      void run.exited.then(() => {
        if (!check()) reject(new Error(`the bridge exited before ${type}; stderr:\n${run.stderr()}`))
      })
      if (!check()) waiters.add(check)
    })
    const send = (message: Record<string, unknown>) => { run.child.stdin.write(`${JSON.stringify({ v: 1, ...message })}\n`) }

    await waitFor('ready')
    // `normal` is the tier that asks before writes and commands, the app's middle setting.
    send({ type: 'init', workspace: surface.workspace, permission: 'normal' })
    await waitFor('initialized')
    send({ type: 'user.message', text: MODEL_SURFACE_TASK })
    await waitFor('turn.done')
    send({ type: 'shutdown' })
    expect(await run.exited, run.stderr()).toBe(0)
    await expect(await snapshot('desktop (bake-base + bake-desktop)', surface)).toMatchFileSnapshot(expected('desktop'))
  }, TEST_TIMEOUT_MS)
})
