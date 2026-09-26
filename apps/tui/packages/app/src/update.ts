/**
 * The terminal app's updates: the status line's notice, known at launch from
 * the check cache and refreshed in the background for the life of the
 * process, and the `/update` command that installs a newer release.
 * @module @dsh-tui/app/update
 */

import { setTimeout as sleep } from 'node:timers/promises'
import type { CommandResult } from '@deepseek-ai/dsh-commands'
import { resolveDshHome } from '@deepseek-ai/dsh-home-paths'
import {
  cachedUpdate, checksDisabled, compareVersions, currentVersion, detectInstall, FAILED_CHECK_RETRY_MS, hostTarget,
  refreshCheck, releaseSource, selfUpdate, UpdateError, type InstallProgress,
} from '@deepseek-ai/dsh-updater'
import { assertNever } from '@deepseek-ai/dsh-util-values'
import type { TuiCopy } from '@dsh-tui/ui/copy.ts'

/** What the check reads besides the network; injected so tests own it. */
export interface UpdatesOptions {
  readonly running: string
  /** The running release's root, which decides whether the install is managed. */
  readonly release: string
  readonly env?: Record<string, string | undefined>
  /** Replaces the global `fetch`, for tests. */
  readonly fetch?: typeof fetch
  readonly now?: () => number
  /**
   * Pause between background checks. Each one asks the host only once the
   * shared cache has gone stale, so terminals left open together do not
   * multiply the requests.
   */
  readonly pollMs?: number
}

/** The newer release the status line names. */
export interface UpdateState {
  readonly version: string
  /** `current` already names it, so the next launch runs it. */
  readonly installed: boolean
}

/**
 * The newer release to name in the status line, if any, and the install that
 * `/update` runs.
 *
 * Only a managed install offers one: the updater refuses a source checkout,
 * so naming an update there would point at a command that cannot run.
 */
export class Updates {
  /** The newer release, or undefined when there is none or it is not known yet. */
  state: UpdateState | undefined
  private work: Promise<void> | undefined
  private changed: () => void = () => {}

  constructor(private readonly options: UpdatesOptions) {}

  /**
   * Read the cached answer now, then keep asking in the background.
   * @param signal - the application's lifetime; aborting it abandons the request and the schedule.
   * @param onChange - called when the answer changes.
   */
  start(signal: AbortSignal, onChange: () => void): void {
    this.changed = onChange
    const env = this.options.env ?? process.env
    const target = hostTarget()
    const layout = detectInstall(this.options.release)
    if (checksDisabled(env) || target === undefined || layout.kind !== 'managed') return
    const home = resolveDshHome(undefined, env)
    this.state = this.read(cachedUpdate(home, this.options.running), layout.root)
    this.work = (async () => {
      while (!signal.aborted) {
        try {
          const version = await refreshCheck({
            ...releaseSource(env), signal, home, running: this.options.running, target,
            ...this.options.fetch === undefined ? {} : { fetch: this.options.fetch },
            ...this.options.now === undefined ? {} : { now: this.options.now },
          })
          if (!signal.aborted) this.set(this.read(version, layout.root))
        } catch {
          // Only cancellation reaches here; a failed check is an answer.
        }
        try {
          await sleep(this.options.pollMs ?? FAILED_CHECK_RETRY_MS, undefined, { signal, ref: false })
        } catch { return }
      }
    })()
  }

  /**
   * Run `/update`: check, and install a newer release beside the running one.
   * @param copy - localized labels.
   * @param notify - shows progress in the notice region; undefined clears it.
   * @param signal - command cancellation. An interrupted install leaves `current` as it was.
   * @returns the command's result, committed to the transcript.
   */
  async update(copy: TuiCopy, notify: (text: string | undefined) => void, signal: AbortSignal): Promise<CommandResult> {
    const env = this.options.env ?? process.env
    const layout = detectInstall(this.options.release)
    let found = ''
    let step = ''
    notify(copy.updateChecking)
    try {
      const outcome = await selfUpdate({
        running: this.options.running, layout, env, signal, home: resolveDshHome(undefined, env),
        ...this.options.fetch === undefined ? {} : { fetch: this.options.fetch },
        onFound: (version) => { found = version },
        onProgress: (progress) => {
          // Keyed by percent, not bytes, so a download repaints about a hundred times.
          const next = progress.phase === 'download' ? `download ${percentOf(progress)}` : progress.phase
          if (next !== step) { step = next; notify(progressText(copy, found, progress)) }
        },
      })
      switch (outcome.kind) {
        case 'unmanaged': return { kind: 'error', text: `${copy.updateUnmanaged}: ${outcome.running}; ${copy.updateUnmanagedHint}` }
        case 'unsupported': return { kind: 'error', text: `${copy.updateUnsupported}: ${outcome.platform}` }
        case 'current': return { kind: 'success', text: `${copy.updateCurrent}: v${outcome.version}` }
        case 'unavailable': return { kind: 'success', text: `${copy.updateUnavailable}: v${outcome.version} (${outcome.target})` }
        // Only a check-only run answers this; `/update` always installs.
        case 'available': return { kind: 'success', text: `${copy.updateLabel}: v${outcome.version}` }
        case 'installed':
          if (layout.kind === 'managed') this.set(this.read(undefined, layout.root))
          return { kind: 'success', text: `${copy.updateInstalled}: v${this.options.running} → v${outcome.version}` }
        default: return assertNever(outcome)
      }
    } catch (error) {
      if (signal.aborted) return { kind: 'error', text: copy.updateCancelled }
      if (error instanceof UpdateError) return { kind: 'error', text: `${copy.updateFailed}: ${error.message}` }
      throw error
    } finally {
      notify(undefined)
    }
  }

  /** @returns once the background checks have stopped, so teardown leaves no late callback. */
  async drain(): Promise<void> { await this.work }

  /**
   * What to name: a release `current` already points past the running one,
   * unless the check knows a newer one still.
   */
  private read(checked: string | undefined, root: string): UpdateState | undefined {
    const current = currentVersion(root)
    const installed = current !== undefined && compareVersions(current, this.options.running) > 0 ? current : undefined
    if (checked !== undefined && (installed === undefined || compareVersions(checked, installed) > 0)) {
      return { version: checked, installed: false }
    }
    return installed === undefined ? undefined : { version: installed, installed: true }
  }

  private set(next: UpdateState | undefined): void {
    if (next?.version === this.state?.version && next?.installed === this.state?.installed) return
    this.state = next
    this.changed()
  }
}

/**
 * The notice for one install step.
 * @param copy - localized labels.
 * @param version - the release being installed.
 * @param progress - the step.
 * @returns one line.
 */
export function progressText(copy: TuiCopy, version: string, progress: InstallProgress): string {
  switch (progress.phase) {
    case 'download': {
      const mb = (bytes: number): string => (bytes / 1_000_000).toFixed(1)
      return `${copy.updateDownloading} v${version}… ${percentOf(progress)}% (${mb(progress.received)} / ${mb(progress.total)} MB)`
    }
    case 'unpack': return `${copy.updateUnpacking} v${version}…`
    case 'verify': return `${copy.updateVerifying}: v${version}…`
  }
}

function percentOf(progress: { readonly received: number; readonly total: number }): number {
  return progress.total === 0 ? 100 : Math.floor(progress.received * 100 / progress.total)
}
