/**
 * One update run, shared by `bake update` and the terminal app's `/update`:
 * check the signed manifest, record the answer, and install a newer release.
 * Each surface words the outcome itself.
 * @module @deepseek-ai/dsh-updater/update
 */

import { existsSync } from 'node:fs'
import { join } from 'node:path'
import { hostTarget, recordCheck, statusOf, type ReleaseStatus } from './check.ts'
import { installRelease, type InstallProgress, type InstallResult } from './install.ts'
import { releaseSource } from './keys.ts'
import type { InstallLayout } from './layout.ts'
import { fetchRelease } from './manifest.ts'

/** What one update run found, or did. */
export type UpdateOutcome =
  /** An install run on a release the updater does not manage; nothing was asked. */
  | { readonly kind: 'unmanaged'; readonly running: string }
  /** Bake publishes no release for this host; nothing was asked. */
  | { readonly kind: 'unsupported'; readonly platform: string }
  | { readonly kind: 'current'; readonly version: string }
  /** Newer, but published without an archive for this host yet. */
  | { readonly kind: 'unavailable'; readonly version: string; readonly target: string }
  /** A check-only run found a newer release. */
  | { readonly kind: 'available'; readonly version: string }
  | { readonly kind: 'installed'; readonly version: string; readonly result: InstallResult }

/** Options for {@link selfUpdate}. */
export interface SelfUpdateOptions {
  /** The running Bake version. */
  readonly running: string
  readonly layout: InstallLayout
  /** Only report; never install. A check may run from an unmanaged release. */
  readonly check?: boolean
  /** Bake home whose check cache records the answer, so update notices agree with this run. */
  readonly home?: string
  readonly env?: Record<string, string | undefined>
  readonly signal?: AbortSignal
  /** Replaces the global `fetch`, for tests. */
  readonly fetch?: typeof fetch
  /** Called once the manifest names the release to install. */
  readonly onFound?: (version: string) => void
  readonly onProgress?: (progress: InstallProgress) => void
}

/**
 * Check for a newer release and, unless `check` is set, install it.
 * @param options - the running release, where it lives, and the effects to use.
 * @returns what the run found or did.
 * @throws {UpdateError} when the check or the install fails; `current` is unchanged in that case.
 */
export async function selfUpdate(options: SelfUpdateOptions): Promise<UpdateOutcome> {
  const { layout, running } = options
  if (options.check !== true && layout.kind === 'unmanaged') return { kind: 'unmanaged', running: layout.running }
  const target = hostTarget()
  if (target === undefined) return { kind: 'unsupported', platform: `${process.platform}-${process.arch}` }
  const env = options.env ?? process.env
  const source = {
    ...releaseSource(env),
    ...options.signal === undefined ? {} : { signal: options.signal },
    ...options.fetch === undefined ? {} : { fetch: options.fetch },
  }
  let status: ReleaseStatus
  try {
    status = statusOf(await fetchRelease(source), running, target)
  } catch (error) {
    if (options.home !== undefined && options.signal?.aborted !== true) await recordCheck(options.home, undefined)
    throw error
  }
  if (options.home !== undefined) await recordCheck(options.home, status)
  if (status.kind === 'current') return { kind: 'current', version: running }
  if (status.kind === 'unavailable') return { kind: 'unavailable', version: status.version, target }
  const version = status.manifest.version
  if (options.check === true || layout.kind === 'unmanaged') return { kind: 'available', version }
  options.onFound?.(version)
  const result = await installRelease({
    ...source, layout, manifest: status.manifest, artifact: status.artifact,
    launcher: windowsLauncherPath(layout.root, env), onProgress: options.onProgress,
  })
  return { kind: 'installed', version, result }
}

/**
 * The Windows `bake.cmd` that started this process, so the install can bring
 * an older one onto the pointer form.
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
