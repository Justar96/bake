/**
 * `bake update`: replace a managed install with the newest signed release.
 * @module @deepseek-ai/dsh/update
 */

import { fileURLToPath } from 'node:url'
import { resolveDshHome } from '@deepseek-ai/dsh-home-paths'
import { startProgress, type ProgressTerminal } from './progress.ts'
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
 * Show one install step on the progress rows: each phase is its own step, and
 * the download fills its meter with the bytes received.
 * @param progress - the rows.
 * @param version - the release being installed.
 * @param update - the updater's report.
 * @param previous - the phase reported before this one.
 */
export function showStep(progress: ReturnType<typeof startProgress>, version: string, update: InstallProgress,
  previous: InstallProgress | undefined): void {
  if (update.phase !== previous?.phase) {
    if (previous?.phase === 'download') progress.note(`${megabytes(previous.total)} MB`)
    switch (update.phase) {
      case 'download': progress.step('Downloading', 'Downloaded'); break
      case 'unpack': progress.step('Unpacking', 'Unpacked'); break
      case 'verify': progress.step('Verifying', 'Verified'); progress.note(`v${version} starts`); break
    }
  }
  if (update.phase === 'download') {
    progress.progress(update.total === 0 ? 1 : update.received / update.total, `${megabytes(update.received)} / ${megabytes(update.total)} MB`)
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
  /** Where progress is drawn; the process's stderr unless `out` or `err` is given. */
  readonly terminal?: ProgressTerminal
} = {}): Promise<number> {
  const out = io.out ?? (line => process.stdout.write(`${line}\n`))
  const err = io.err ?? (line => process.stderr.write(`${line}\n`))
  const env = io.env ?? process.env
  const layout = io.layout ?? detectInstall(releaseRoot())
  let progress: ReturnType<typeof startProgress> | undefined
  let previous: InstallProgress | undefined
  let found = ''
  try {
    const outcome = await selfUpdate({
      running, layout, check, env, home: resolveDshHome(undefined, env),
      ...io.signal === undefined ? {} : { signal: io.signal },
      ...io.fetch === undefined ? {} : { fetch: io.fetch },
      onFound: (version) => {
        found = version
        const terminal = io.terminal ?? (io.out === undefined && io.err === undefined ? process.stderr : { write() {} })
        progress = startProgress(terminal, env, ['update', `v${running} \u2192 v${version}`])
        if (!progress.animated) out(`Downloading Bake ${version}…`)
      },
      onProgress: (update) => {
        if (progress !== undefined) showStep(progress, found, update, previous)
        previous = update
      },
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
        const next = outcome.version
        const sessions = `New sessions start ${next}; sessions already open keep ${running}.`
        const after = [
          sessions,
          ...outcome.result.launcherPending ? ['The bake command switches over once this window\'s bake exits.'] : [],
          ...outcome.result.pruned.length > 0 ? [`Removed releases unused for a week: ${outcome.result.pruned.join(', ')}`] : [],
        ]
        if (progress?.animated === true) {
          progress.finish({ title: `Updated Bake ${running} \u2192 ${next}`, next: after })
          return 0
        }
        out(`Updated Bake ${running} → ${next}. ${sessions}`)
        for (const line of after.slice(1)) out(line)
        return 0
      }
      default:
        outcome satisfies never
        throw new Error(`bake update: unhandled outcome ${JSON.stringify(outcome)}`)
    }
  } catch (error) {
    progress?.fail()
    if (io.signal?.aborted === true) { err('Update cancelled; the install is unchanged.'); return 1 }
    if (error instanceof UpdateError) { err(error.message); return 1 }
    throw error
  } finally {
    progress?.stop()
  }
}
