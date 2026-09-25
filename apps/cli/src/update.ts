/**
 * `bake update`: replace a managed install with the newest signed release.
 * @module @deepseek-ai/dsh/update
 */

import { existsSync } from 'node:fs'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { startBakery } from './bakery.ts'
import {
  detectInstall, fetchRelease, hostTarget, installRelease, markLaunched, releaseSource, statusOf, UpdateError,
  type InstallLayout,
} from '@deepseek-ai/dsh-updater'

/** `--check` exit status when a newer release is available, so a script can tell it from "up to date" (0) and failure (1). */
export const UPDATE_AVAILABLE_EXIT = 10

/**
 * The release this command runs from: three directories above both
 * `apps/cli/src` and `apps/cli/lib`.
 * @returns the release root, the directory holding `apps/`.
 */
export function releaseRoot(): string {
  return fileURLToPath(new URL('../../../', import.meta.url))
}

/**
 * Note that a session started from this release, so an update's pruning
 * leaves the release alone while it may still be running. Best effort, and
 * never awaited by startup.
 */
export async function recordLaunch(): Promise<void> {
  const layout = detectInstall(releaseRoot())
  if (layout.kind === 'managed') await markLaunched(layout.running)
}

/**
 * Run `bake update`.
 * @param check - only report; never install.
 * @param running - the running Bake version.
 * @param io - where the report goes, and the environment and layout to act on.
 * @returns the process exit status.
 */
export async function runUpdate(check: boolean, running: string, io: {
  readonly out?: (line: string) => void
  readonly err?: (line: string) => void
  readonly env?: Record<string, string | undefined>
  readonly layout?: InstallLayout
  readonly signal?: AbortSignal
  /** Replaces the global `fetch`, for tests. */
  readonly fetch?: typeof fetch
} = {}): Promise<number> {
  const out = io.out ?? (line => process.stdout.write(`${line}\n`))
  const err = io.err ?? (line => process.stderr.write(`${line}\n`))
  const env = io.env ?? process.env
  const layout = io.layout ?? detectInstall(releaseRoot())
  if (!check && layout.kind === 'unmanaged') {
    err(`This Bake runs from ${layout.running}, which the updater does not manage.`)
    err('A source checkout updates with: git pull && bun install --frozen-lockfile && bun run build')
    err('Any other copy updates by installing again: curl -fsSL https://bake.justar.dev/install.sh | sh')
    return 1
  }
  const target = hostTarget()
  if (target === undefined) {
    err(`Bake publishes no release for ${process.platform}-${process.arch}.`)
    return 1
  }
  const source = {
    ...releaseSource(env),
    ...io.signal === undefined ? {} : { signal: io.signal },
    ...io.fetch === undefined ? {} : { fetch: io.fetch },
  }
  let bakery: ReturnType<typeof startBakery> | undefined
  try {
    const status = statusOf(await fetchRelease(source), running, target)
    if (status.kind === 'current') { out(`Bake ${running} is up to date.`); return 0 }
    if (status.kind === 'unavailable') { out(`Bake ${status.version} is published, but not yet for ${target}; ${running} stays.`); return 0 }
    const next = status.manifest.version
    if (check) { out(`Bake ${next} is available (running ${running}). Run: bake update`); return UPDATE_AVAILABLE_EXIT }
    if (layout.kind === 'unmanaged') return 1
    out(`Downloading Bake ${next} for ${target}…`)
    bakery = startBakery(io.out === undefined && io.err === undefined ? process.stderr : { write() {} }, env, 'Baking your update...')
    const result = await installRelease({
      ...source, layout, manifest: status.manifest, artifact: status.artifact, launcher: windowsLauncherPath(layout.root, env),
    })
    bakery.finish(true)
    out(`Updated Bake ${running} → ${next}. New sessions start ${next}; sessions already open keep ${running}.`)
    if (result.launcherPending) out('The bake command switches over once this window\'s bake exits.')
    if (result.pruned.length > 0) out(`Removed releases unused for a week: ${result.pruned.join(', ')}`)
    return 0
  } catch (error) {
    bakery?.finish()
    if (io.signal?.aborted === true) { err('Update cancelled; the install is unchanged.'); return 1 }
    if (error instanceof UpdateError) { err(error.message); return 1 }
    throw error
  } finally {
    bakery?.finish()
  }
}

/**
 * The Windows `bake.cmd` that started this process.
 *
 * The pointer-form launcher names itself in `BAKE_LAUNCHER`. An older one
 * does not, and sits where the installer puts it by default.
 */
function windowsLauncherPath(root: string, env: Record<string, string | undefined>): string | undefined {
  if (process.platform !== 'win32') return undefined
  const named = env['BAKE_LAUNCHER']
  if (named !== undefined && named !== '') return named
  const fallback = join(env['BAKE_BIN_DIR'] ?? join(root, 'bin'), 'bake.cmd')
  return existsSync(fallback) ? fallback : undefined
}
