#!/usr/bin/env bun
/**
 * Check the deployed download service serves a native release:
 * `bun run release:verify-live <version> [--complete]`.
 *
 * Waits for `native/stable.json` to name the version, then verifies it the
 * way a client does: a signature from the committed native key alone, the
 * manifest's own rules, and every archive's size and hash, fetched through
 * the service, which redirects archives to the version's GitHub release.
 * Bake 0.4.0 publishes linux-x64 and linux-arm64 alone, so a live manifest
 * naming another platform is refused. The frozen 0.3 route must still verify
 * with its own key, so a deploy that dropped it fails here, and the
 * installers must answer. A deployment that reports success is not a
 * release until this passes.
 */

import { resolve } from 'node:path'
import { fetchRelease, RELEASE_PUBLIC_KEYS } from '../../packages/boot/updater/src/index.ts'
import { committedNativeKey, fetchNativeRelease } from './native-channel.ts'
import { NATIVE_RELEASES_DIR, type NativeManifest } from './native-manifest.ts'
import { isReleaseTarget, RELEASE_TARGETS } from './pack-rust.ts'

/** What {@link verifyLive} needs, injected so tests own the network, the keys, and the clock. */
export interface LiveOptions {
  readonly base: string
  readonly version: string
  /** Require an archive for every release platform. */
  readonly complete?: boolean
  /** Trusted native keys: the committed native key, which a throwaway CI key never replaces. */
  readonly keys: readonly string[]
  /** Trusted 0.3 keys; the 0.3 updater's own by default. */
  readonly legacyKeys?: readonly string[]
  readonly fetch?: typeof fetch
  /** How long to wait for the new deployment to serve the version. */
  readonly timeoutMs?: number
  readonly sleep?: (ms: number) => Promise<void>
}

/**
 * Wait for the host to serve `version`, then verify all of it.
 * @param options - the host, the expected version, and the effects to use.
 * @returns the verified native manifest.
 * @throws when the host never serves the version, or any check fails.
 */
export async function verifyLive(options: LiveOptions): Promise<NativeManifest> {
  const fetcher = options.fetch ?? fetch
  const sleep = options.sleep ?? (ms => new Promise(resolve => setTimeout(resolve, ms)))
  const deadline = Date.now() + (options.timeoutMs ?? 10 * 60 * 1000)
  const remaining = (): number => Math.max(1, Math.min(30_000, deadline - Date.now()))
  /** Retry `attempt` until it returns, or report its last failure at the deadline. */
  const until = async <T>(what: string, attempt: () => Promise<T>): Promise<T> => {
    let last = 'no answer yet'
    for (;;) {
      try {
        return await attempt()
      } catch (error) {
        last = error instanceof Error ? error.message : String(error)
      }
      if (Date.now() >= deadline) throw new Error(`${what}: ${last}`)
      await sleep(Math.min(10_000, Math.max(0, deadline - Date.now())))
    }
  }
  const manifest = await until(`${options.base} never served Bake ${options.version}`, async () => {
    const served = await fetchNativeRelease({ base: options.base, keys: options.keys, fetch: fetcher, timeoutMs: remaining() })
    if (served === undefined) throw new Error('it publishes no native release')
    if (served.version !== options.version) throw new Error(`it serves ${served.version}`)
    return served
  })
  const listed = Object.keys(manifest.artifacts)
  const refused = listed.filter(target => !isReleaseTarget(target))
  if (refused.length > 0) throw new Error(`The live release names ${refused.join(', ')}, which this release does not publish`)
  const missing = RELEASE_TARGETS.filter(target => manifest.artifacts[target] === undefined)
  if (options.complete === true && missing.length > 0) throw new Error(`The live release lacks ${missing.join(', ')}`)
  const download = async (url: string): Promise<Uint8Array> => until(`${url} did not become available`, async () => {
    const response = await fetcher(url, { cache: 'no-store', signal: AbortSignal.timeout(remaining()) })
    if (!response.ok) throw new Error(`answered ${response.status}`)
    return new Uint8Array(await response.arrayBuffer())
  })
  for (const [target, artifact] of Object.entries(manifest.artifacts)) {
    const bytes = await download(`${options.base}/${NATIVE_RELEASES_DIR}/${manifest.version}/${artifact.file}`)
    if (bytes.byteLength !== artifact.size) throw new Error(`${target}: served ${bytes.byteLength} bytes, not ${artifact.size}`)
    if (Bun.CryptoHasher.hash('sha256', bytes, 'hex') !== artifact.sha256) {
      throw new Error(`${target}: served bytes do not match the manifest`)
    }
  }
  // 0.3 installs keep updating from this route until they move to native.
  await until('The frozen 0.3 route no longer verifies', () => fetchRelease({
    base: options.base, keys: options.legacyKeys ?? RELEASE_PUBLIC_KEYS, fetch: fetcher, signal: AbortSignal.timeout(remaining()),
  }))
  for (const path of ['health', 'install.sh', 'install.ps1']) await download(`${options.base}/${path}`)
  return manifest
}

if (import.meta.main) {
  const version = process.argv[2]?.replace(/^v/, '')
  if (version === undefined) {
    console.error('Usage: bun run release:verify-live <version> [--complete]')
    process.exit(1)
  }
  const base = (process.env.BAKE_RELEASE_BASE_URL?.trim() || 'https://bake.justar.dev').replace(/\/+$/, '')
  try {
    const keys = [committedNativeKey(resolve(import.meta.dir, '../..'))]
    const manifest = await verifyLive({ base, version, keys, complete: process.argv.includes('--complete') })
    console.log(`Verified live Bake ${manifest.version} at ${base}: ${Object.keys(manifest.artifacts).join(', ')}, with the 0.3 route intact`)
  } catch (error) {
    console.error(error instanceof Error ? error.message : String(error))
    process.exit(1)
  }
}
