#!/usr/bin/env bun
/**
 * Check the deployed download service serves a release:
 * `bun run release:verify-live <version> [--complete]`.
 *
 * Waits for `latest.json` and its listed archives to become available, then
 * verifies them the way a client does: a signature from a committed key
 * alone, whatever `BAKE_RELEASE_PUBLIC_KEY` says, and every archive's size
 * and hash. A deployment that reports success is not a release until this
 * passes.
 */

import { createHash } from 'node:crypto'
import { archiveUrl, fetchRelease, RELEASE_PUBLIC_KEYS, RELEASE_TARGETS, type ReleaseManifest } from '../../packages/boot/updater/src/index.ts'

/** What {@link verifyLive} needs, injected so tests own the network and the clock. */
export interface LiveOptions {
  readonly base: string
  readonly version: string
  /** Require an archive for every release target. */
  readonly complete?: boolean
  readonly fetch?: typeof fetch
  /** How long to wait for the new deployment to serve the version. */
  readonly timeoutMs?: number
  readonly sleep?: (ms: number) => Promise<void>
}

/**
 * Wait for the host to serve `version`, then verify all of it.
 * @param options - the host, the expected version, and the effects to use.
 * @returns the verified manifest.
 * @throws when the host never serves the version, or any check fails.
 */
export async function verifyLive(options: LiveOptions): Promise<ReleaseManifest> {
  const fetcher = options.fetch ?? fetch
  const sleep = options.sleep ?? (ms => new Promise(resolve => setTimeout(resolve, ms)))
  const deadline = Date.now() + (options.timeoutMs ?? 10 * 60 * 1000)
  const request = (url: string): Promise<Response> => fetcher(url, {
    cache: 'no-store',
    signal: AbortSignal.timeout(Math.max(1, Math.min(30_000, deadline - Date.now()))),
  })
  const wait = async (): Promise<void> => { await sleep(Math.min(10_000, Math.max(0, deadline - Date.now()))) }
  let manifest: ReleaseManifest | undefined
  let last = 'no answer yet'
  for (;;) {
    try {
      // Only the committed keys: a throwaway CI key must never pass here.
      manifest = await fetchRelease({ base: options.base, keys: RELEASE_PUBLIC_KEYS, fetch: fetcher,
        signal: AbortSignal.timeout(Math.max(1, Math.min(30_000, deadline - Date.now()))) })
      if (manifest.version === options.version) break
      last = `serves ${manifest.version}`
    } catch (error) {
      last = error instanceof Error ? error.message : String(error)
    }
    if (Date.now() >= deadline) throw new Error(`${options.base} never served Bake ${options.version}: ${last}`)
    await wait()
  }
  const missing = RELEASE_TARGETS.filter(target => manifest.artifacts[target] === undefined)
  if (options.complete === true && missing.length > 0) throw new Error(`The live release lacks ${missing.join(', ')}`)
  const available = async (url: string): Promise<Uint8Array> => {
    let last = 'no answer yet'
    for (;;) {
      try {
        const response = await request(url)
        if (!response.ok) throw new Error(`answered ${response.status}`)
        return new Uint8Array(await response.arrayBuffer())
      } catch (error) {
        last = error instanceof Error ? error.message : String(error)
      }
      if (Date.now() >= deadline) throw new Error(`${url} did not become available: ${last}`)
      await wait()
    }
  }
  for (const [target, artifact] of Object.entries(manifest.artifacts)) {
    const url = archiveUrl(options.base, manifest.version, artifact.file)
    const bytes = await available(url)
    if (bytes.byteLength !== artifact.size) throw new Error(`${target}: served ${bytes.byteLength} bytes, not ${artifact.size}`)
    if (createHash('sha256').update(bytes).digest('hex') !== artifact.sha256) throw new Error(`${target}: served bytes do not match the manifest`)
  }
  // The download service also serves the installers; a GitHub release does not.
  for (const path of options.base.startsWith('https://github.com/') ? [] : ['health', 'install.sh', 'install.ps1']) {
    await available(`${options.base}/${path}`)
  }
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
    const manifest = await verifyLive({ base, version, complete: process.argv.includes('--complete') })
    console.log(`Verified live Bake ${manifest.version} at ${base}: ${Object.keys(manifest.artifacts).join(', ')}`)
  } catch (error) {
    console.error(error instanceof Error ? error.message : String(error))
    process.exit(1)
  }
}
