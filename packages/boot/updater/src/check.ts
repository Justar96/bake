/**
 * Whether a newer release exists: answered from a daily cache so a launch
 * never waits on the network, and refreshed in the background.
 * @module @deepseek-ai/dsh-updater/check
 */

import { randomBytes } from 'node:crypto'
import { readFileSync } from 'node:fs'
import { mkdir, rename, rm, writeFile } from 'node:fs/promises'
import { dirname, join } from 'node:path'
import { fetchRelease, RELEASE_TARGETS, type ReleaseArtifact, type ReleaseManifest, type ReleaseSource, type ReleaseTarget } from './manifest.ts'
import { compareVersions, isReleaseVersion } from './version.ts'

/** How long one answer stands before the next launch asks again. */
export const CHECK_INTERVAL_MS = 24 * 60 * 60 * 1000

/** The cache file, under the Bake home. */
export const CHECK_CACHE = 'update-check.json'

/** What the last check found. */
interface CheckCache {
  readonly checkedAt: number
  /** The newest release with an archive for this platform, when the check succeeded. */
  readonly version?: string
}

/** How a release compares with the running one, for this platform. */
export type ReleaseStatus =
  | { readonly kind: 'newer'; readonly manifest: ReleaseManifest; readonly artifact: ReleaseArtifact }
  | { readonly kind: 'current'; readonly version: string }
  /** Newer, but published without an archive for this platform yet. */
  | { readonly kind: 'unavailable'; readonly version: string; readonly target: string }

/**
 * This process's release target, when Bake publishes one for it.
 * @returns `<platform>-<arch>`, or undefined for an unsupported host.
 */
export function hostTarget(platform: string = process.platform, arch: string = process.arch): ReleaseTarget | undefined {
  const target = `${platform}-${arch}`
  return (RELEASE_TARGETS as readonly string[]).includes(target) ? target as ReleaseTarget : undefined
}

/**
 * Compare a verified manifest with the running version.
 * @param manifest - the verified release manifest.
 * @param running - the running Bake version.
 * @param target - this host's release target.
 * @returns whether there is an update this host can install.
 */
export function statusOf(manifest: ReleaseManifest, running: string, target: string): ReleaseStatus {
  if (compareVersions(manifest.version, running) <= 0) return { kind: 'current', version: running }
  const artifact = manifest.artifacts[target as ReleaseTarget]
  return artifact === undefined ? { kind: 'unavailable', version: manifest.version, target } : { kind: 'newer', manifest, artifact }
}

/**
 * Whether the update check is turned off: `BAKE_NO_UPDATE_CHECK` set to
 * anything but empty, `0`, or `false`.
 * @param env - the process environment.
 * @returns whether to skip checking.
 */
export function checksDisabled(env: Record<string, string | undefined> = process.env): boolean {
  const value = env['BAKE_NO_UPDATE_CHECK']?.trim().toLowerCase()
  return value !== undefined && value !== '' && value !== '0' && value !== 'false'
}

/**
 * The newer version the last check found, read synchronously at launch.
 * @param home - the Bake home.
 * @param running - the running version.
 * @returns the newer version, or undefined when none is known.
 */
export function cachedUpdate(home: string, running: string): string | undefined {
  const cache = readCache(home)
  return cache?.version !== undefined && compareVersions(cache.version, running) > 0 ? cache.version : undefined
}

/** Options for {@link refreshCheck}. */
export interface RefreshOptions extends ReleaseSource {
  readonly home: string
  readonly running: string
  readonly target: ReleaseTarget
  readonly now?: () => number
}

/**
 * Ask the release host again, unless the last answer is still fresh.
 *
 * A failed check is recorded too, without a version, so a host that is down
 * is asked once a day rather than on every launch. Only a verified manifest
 * with an archive for this platform records a version, so the notice never
 * offers an update this host cannot install.
 *
 * @param options - the release source, the cache's home, and the running version.
 * @returns the newer version, or undefined when there is none or the check failed.
 */
export async function refreshCheck(options: RefreshOptions): Promise<string | undefined> {
  const now = (options.now ?? Date.now)()
  const cache = readCache(options.home)
  if (cache !== undefined && now - cache.checkedAt < CHECK_INTERVAL_MS && now >= cache.checkedAt) {
    return cache.version !== undefined && compareVersions(cache.version, options.running) > 0 ? cache.version : undefined
  }
  let version: string | undefined
  try {
    const status = statusOf(await fetchRelease(options), options.running, options.target)
    version = status.kind === 'newer' ? status.manifest.version : undefined
  } catch (error) {
    if (options.signal?.aborted === true) throw error
  }
  await writeCache(options.home, { checkedAt: now, ...version === undefined ? {} : { version } })
  return version
}

function readCache(home: string): CheckCache | undefined {
  try {
    const value = JSON.parse(readFileSync(join(home, CHECK_CACHE), 'utf8')) as { checkedAt?: unknown; version?: unknown }
    if (typeof value.checkedAt !== 'number') return undefined
    return { checkedAt: value.checkedAt, ...isReleaseVersion(value.version) ? { version: value.version } : {} }
  } catch {
    return undefined
  }
}

async function writeCache(home: string, cache: CheckCache): Promise<void> {
  const path = join(home, CHECK_CACHE)
  const temp = `${path}.${randomBytes(6).toString('hex')}.tmp`
  try {
    await mkdir(dirname(path), { recursive: true })
    await writeFile(temp, `${JSON.stringify(cache)}\n`, { flag: 'wx', mode: 0o600 })
    await rename(temp, path)
  } catch {
    // The cache only saves a request; losing it costs one more check.
    await rm(temp, { force: true }).catch(() => {})
  }
}
