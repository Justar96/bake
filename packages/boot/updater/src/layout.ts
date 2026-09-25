/**
 * Where a managed Bake install lives, as the installers lay it out.
 *
 * ```
 * <root>/versions/<version>-<sha256 prefix>/   one complete release each
 * <root>/current                               Unix: absolute link to one of them
 * <root>/current.txt                           Windows: the name of one of them
 * ```
 *
 * `~/.local/bin/bake` links to `<root>/current/bin/bake` on Unix, so moving
 * `current` moves the command. On Windows, `bake.cmd` reads `current.txt`.
 * A running process has already resolved its own version directory, so it
 * keeps running from it while `current` moves.
 *
 * @module @deepseek-ai/dsh-updater/layout
 */

import { existsSync, readFileSync, readlinkSync, realpathSync } from 'node:fs'
import { basename, dirname, isAbsolute, join, resolve } from 'node:path'
import { VERSION_PATTERN } from './version.ts'

/** A version directory's name: the release version and the first 12 hex digits of its archive hash. */
const VERSION_DIRECTORY = new RegExp(`^(${VERSION_PATTERN.source.slice(1, -1)})-([a-f0-9]{12})$`)

/** The Windows pointer file, holding one version directory's name. */
export const CURRENT_POINTER = 'current.txt'

/** A release directory the installers or the updater created. */
export interface ManagedInstall {
  readonly kind: 'managed'
  /** Install root, holding `versions/` and `current`. */
  readonly root: string
  /** The running release's directory. */
  readonly running: string
  /** Name of the directory `current` names, when it names one. */
  readonly current: string | undefined
}

/** Where this process runs from, as far as updating is concerned. */
export type InstallLayout = ManagedInstall | { readonly kind: 'unmanaged'; readonly running: string }

/**
 * Parse a version directory's name.
 * @param name - a directory name under `versions/`.
 * @returns its version and hash prefix, or undefined for any other name.
 */
export function versionDirectory(name: string): { readonly version: string; readonly digest: string } | undefined {
  const [, version, digest] = VERSION_DIRECTORY.exec(name) ?? []
  return version === undefined || digest === undefined ? undefined : { version, digest }
}

/**
 * The version directory name an artifact installs to.
 * @param version - the release version.
 * @param sha256 - the archive's hash.
 * @returns `<version>-<first 12 hex digits>`, as the installers name it.
 */
export function directoryFor(version: string, sha256: string): string {
  return `${version}-${sha256.slice(0, 12)}`
}

/**
 * Classify the release this process runs from.
 *
 * Managed means the release sits directly in a `versions/` directory under a
 * name the installers give it. Anything else — a source checkout, or a copy
 * someone unpacked by hand — is unmanaged, and the updater leaves it alone.
 *
 * @param release - the running release's root directory, the one holding `apps/`.
 * @returns the layout.
 */
export function detectInstall(release: string): InstallLayout {
  let running: string
  try { running = realpathSync(release) } catch { return { kind: 'unmanaged', running: resolve(release) } }
  const versions = dirname(running)
  if (basename(versions) !== 'versions' || versionDirectory(basename(running)) === undefined) {
    return { kind: 'unmanaged', running }
  }
  const root = dirname(versions)
  return { kind: 'managed', root, running, current: currentOf(root) }
}

/**
 * Which version directory `current` names under `root`.
 * @param root - the install root.
 * @returns the directory name, or undefined when there is no valid pointer.
 */
export function currentOf(root: string): string | undefined {
  try {
    const pointer = join(root, CURRENT_POINTER)
    if (existsSync(pointer)) {
      const name = readFileSync(pointer, 'utf8').trim()
      return versionDirectory(name) === undefined ? undefined : name
    }
    const target = readlinkSync(join(root, 'current'))
    const name = basename(isAbsolute(target) ? target : resolve(root, target))
    return versionDirectory(name) === undefined ? undefined : name
  } catch {
    return undefined
  }
}
