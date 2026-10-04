/**
 * `bake update --rollback`: move `current` back to an earlier release still
 * installed beside it.
 *
 * The updater records no history of `current`; the version directories are
 * the record. The rollback target is the newest release under `versions/`
 * whose version is older than the one `current` names and whose command is
 * on disk. Among copies of that version, the one started most recently wins.
 * After an update that is the release it replaced, which pruning always
 * keeps; a second rollback goes one release further back, while one is left.
 *
 * A rollback takes the install lock, runs the target's launch check as an
 * install does, and moves `current` with the same atomic pointer move, so
 * any failure leaves `current` where it was. It removes nothing.
 *
 * @module @deepseek-ai/dsh-updater/rollback
 */

import { existsSync } from 'node:fs'
import { readdir, stat } from 'node:fs/promises'
import { join } from 'node:path'
import { acquireLock, LAUNCH_MARKER, pointAt, type InstallProgress } from './install.ts'
import { currentOf, versionDirectory, type InstallLayout } from './layout.ts'
import { UpdateError } from './manifest.ts'
import { launchProblem, RELEASE_COMMAND } from './verify.ts'
import { compareVersions } from './version.ts'

/** What a rollback found, or did. Releases are named by their version directory. */
export type RollbackOutcome =
  /** A release the updater does not manage keeps no earlier ones. */
  | { readonly kind: 'unmanaged'; readonly running: string }
  /** No release older than `current` is installed. */
  | { readonly kind: 'none'; readonly current: string }
  | { readonly kind: 'rolled-back'; readonly from: string; readonly to: string }

/** Options for {@link rollbackRelease}, injected so tests own every effect. */
export interface RollbackOptions {
  readonly layout: InstallLayout
  /** Node executable that runs the target's launch check; the running one by default. */
  readonly node?: string
  /** Time source, for the lock's age. */
  readonly now?: () => number
  /** Platform whose pointer the rollback moves; the running one by default. */
  readonly platform?: NodeJS.Platform
  readonly signal?: AbortSignal
  /** Called once the target is chosen, before its check runs. */
  readonly onFound?: ((from: string, to: string) => void) | undefined
  /** Reports the `verify` phase as the target's check starts. */
  readonly onProgress?: ((progress: InstallProgress) => void) | undefined
}

/**
 * Make the newest installed release older than the current one current again.
 * @param options - the install, and the effects to use.
 * @returns what the rollback found or did.
 * @throws {UpdateError} when the install has no current release, another
 *   update holds the lock, or the target fails its check; `current` is
 *   unchanged in every case.
 */
export async function rollbackRelease(options: RollbackOptions): Promise<RollbackOutcome> {
  const { layout } = options
  if (layout.kind === 'unmanaged') return { kind: 'unmanaged', running: layout.running }
  const release = await acquireLock(layout.root, options.now ?? Date.now)
  try {
    // Read under the lock: an update may have moved it since the layout was detected.
    const from = currentOf(layout.root)
    if (from === undefined) throw new UpdateError(`${layout.root} names no current release; run the installer again`)
    const to = await rollbackTarget(layout.root, from)
    if (to === undefined) return { kind: 'none', current: from }
    options.onFound?.(from, to)
    options.onProgress?.({ phase: 'verify' })
    const version = versionDirectory(to)?.version ?? to
    const problem = await launchProblem({
      node: options.node ?? process.execPath, release: join(layout.root, 'versions', to), version, signal: options.signal,
    })
    if (problem !== undefined) throw new UpdateError(`The earlier Bake ${version} did not start; the current install is unchanged: ${problem}`)
    await pointAt(layout.root, to, options.platform ?? process.platform)
    return { kind: 'rolled-back', from, to }
  } finally {
    await release()
  }
}

/**
 * The release a rollback from `current` returns to; see the module comment.
 * @param root - the install root.
 * @param current - the version directory `current` names.
 * @returns a version directory's name, or undefined when no older release is installed.
 */
export async function rollbackTarget(root: string, current: string): Promise<string | undefined> {
  const from = versionDirectory(current)
  if (from === undefined) return undefined
  const candidates: { readonly name: string; readonly version: string; readonly used: number }[] = []
  for (const name of await readdir(join(root, 'versions')).catch(() => [] as string[])) {
    const parsed = versionDirectory(name)
    if (parsed === undefined || compareVersions(parsed.version, from.version) >= 0) continue
    const directory = join(root, 'versions', name)
    if (!existsSync(join(directory, RELEASE_COMMAND))) continue
    const used = await stat(join(directory, LAUNCH_MARKER)).catch(() => stat(directory)).then(found => found.mtimeMs, () => 0)
    candidates.push({ name, version: parsed.version, used })
  }
  candidates.sort((left, right) => compareVersions(right.version, left.version) || right.used - left.used
    || (left.name < right.name ? -1 : 1))
  return candidates[0]?.name
}
