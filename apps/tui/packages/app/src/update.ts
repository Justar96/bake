/**
 * The status line's update notice: known at launch from the daily cache, and
 * refreshed in the background without delaying the first frame.
 * @module @dsh-tui/app/update
 */

import { resolveDshHome } from '@deepseek-ai/dsh-home-paths'
import {
  cachedUpdate, checksDisabled, detectInstall, hostTarget, refreshCheck, releaseSource,
} from '@deepseek-ai/dsh-updater'

/** What the check reads besides the network; injected so tests own it. */
export interface UpdateNoticeOptions {
  readonly running: string
  /** The running release's root, which decides whether the install is managed. */
  readonly release: string
  readonly env?: Record<string, string | undefined>
  /** Replaces the global `fetch`, for tests. */
  readonly fetch?: typeof fetch
  readonly now?: () => number
}

/**
 * The newer release to name in the status line, if any.
 *
 * Only a managed install offers one: `bake update` refuses a source checkout,
 * so naming an update there would point at a command that cannot run. The
 * notice never installs anything; the user runs `bake update`.
 */
export class UpdateNotice {
  /** The newer version, or undefined when there is none or it is not known yet. */
  version: string | undefined
  private work: Promise<void> | undefined

  constructor(private readonly options: UpdateNoticeOptions) {}

  /**
   * Read the cached answer now, and ask again in the background when it is a day old.
   * @param signal - the application's lifetime; aborting it abandons the request.
   * @param onChange - called when the background check changes the answer.
   */
  start(signal: AbortSignal, onChange: () => void): void {
    const env = this.options.env ?? process.env
    const target = hostTarget()
    if (checksDisabled(env) || target === undefined || detectInstall(this.options.release).kind !== 'managed') return
    const home = resolveDshHome(undefined, env)
    this.version = cachedUpdate(home, this.options.running)
    this.work = refreshCheck({
      ...releaseSource(env), signal, home, running: this.options.running, target,
      ...this.options.fetch === undefined ? {} : { fetch: this.options.fetch },
      ...this.options.now === undefined ? {} : { now: this.options.now },
    }).then(version => {
      if (signal.aborted || version === this.version) return
      this.version = version
      onChange()
    }, () => {})
  }

  /** @returns once the background check has settled, so teardown leaves no late callback. */
  async drain(): Promise<void> { await this.work }
}
