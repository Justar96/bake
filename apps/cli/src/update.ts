/**
 * `bake update`: replace a managed install with the newest signed release.
 * @module @deepseek-ai/dsh/update
 */

import { fileURLToPath } from 'node:url'
import { resolveDshHome } from '@deepseek-ai/dsh-home-paths'
import { startBakery } from './bakery.ts'
import {
  detectInstall, markLaunched, selfUpdate, UpdateError, type InstallLayout, type InstallProgress,
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
 * The spinner's label for an install step.
 * @param version - the release being installed.
 * @param progress - the step.
 * @returns one line, without a trailing newline.
 */
export function progressLabel(version: string, progress: InstallProgress): string {
  switch (progress.phase) {
    case 'download': {
      const percent = progress.total === 0 ? 100 : Math.floor(progress.received * 100 / progress.total)
      return `Downloading Bake ${version}… ${percent}% (${megabytes(progress.received)} / ${megabytes(progress.total)} MB)`
    }
    case 'unpack': return `Unpacking Bake ${version}…`
    case 'verify': return `Checking Bake ${version} starts…`
  }
}

function megabytes(bytes: number): string {
  return (bytes / 1_000_000).toFixed(1)
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
  let bakery: ReturnType<typeof startBakery> | undefined
  let found = ''
  try {
    const outcome = await selfUpdate({
      running, layout, check, env, home: resolveDshHome(undefined, env),
      ...io.signal === undefined ? {} : { signal: io.signal },
      ...io.fetch === undefined ? {} : { fetch: io.fetch },
      onFound: (version) => {
        found = version
        out(`Downloading Bake ${version}…`)
        bakery = startBakery(io.out === undefined && io.err === undefined ? process.stderr : { write() {} }, env, 'Baking your update...')
      },
      onProgress: (progress) => { bakery?.stage(progressLabel(found, progress)) },
    })
    switch (outcome.kind) {
      case 'unmanaged':
        err(`This Bake runs from ${outcome.running}, which the updater does not manage.`)
        err('A source checkout updates with: git pull && bun install --frozen-lockfile && bun run build')
        err('Any other copy updates by installing again: curl -fsSL https://bake.justar.dev/install.sh | sh')
        return 1
      case 'unsupported': err(`Bake publishes no release for ${outcome.platform}.`); return 1
      case 'current': out(`Bake ${running} is up to date.`); return 0
      case 'unavailable': out(`Bake ${outcome.version} is published, but not yet for ${outcome.target}; ${running} stays.`); return 0
      case 'available':
        out(`Bake ${outcome.version} is available (running ${running}). Run: bake update`)
        return UPDATE_AVAILABLE_EXIT
      case 'installed': {
        bakery?.finish(true)
        const next = outcome.version
        out(`Updated Bake ${running} → ${next}. New sessions start ${next}; sessions already open keep ${running}.`)
        if (outcome.result.launcherPending) out('The bake command switches over once this window\'s bake exits.')
        if (outcome.result.pruned.length > 0) out(`Removed releases unused for a week: ${outcome.result.pruned.join(', ')}`)
        return 0
      }
      default:
        outcome satisfies never
        throw new Error(`bake update: unhandled outcome ${JSON.stringify(outcome)}`)
    }
  } catch (error) {
    bakery?.finish()
    if (io.signal?.aborted === true) { err('Update cancelled; the install is unchanged.'); return 1 }
    if (error instanceof UpdateError) { err(error.message); return 1 }
    throw error
  } finally {
    bakery?.finish()
  }
}
