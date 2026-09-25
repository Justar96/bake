/**
 * The signed release manifest: what `latest.json` may say, and proof that
 * the release owner said it.
 * @module @deepseek-ai/dsh-updater/manifest
 */

import { createPublicKey, verify } from 'node:crypto'
import { isReleaseVersion } from './version.ts'

/** Platforms a Bake release can carry an archive for. */
export const RELEASE_TARGETS = ['darwin-arm64', 'darwin-x64', 'linux-arm64', 'linux-x64', 'win32-x64'] as const

/** One platform a release archive is built for. */
export type ReleaseTarget = typeof RELEASE_TARGETS[number]

/** One platform's archive, as the manifest describes it. */
export interface ReleaseArtifact {
  /** `bake-v<version>-<target>.tar.gz`, relative to `releases/<version>/`. */
  readonly file: string
  /** Lowercase hex SHA-256 of the archive bytes. */
  readonly sha256: string
  /** Archive size in bytes. */
  readonly size: number
}

/** The newest release and the archives it publishes. */
export interface ReleaseManifest {
  readonly version: string
  readonly artifacts: Partial<Record<ReleaseTarget, ReleaseArtifact>>
}

/** A failure the updater reports to the user as is. */
export class UpdateError extends Error {
  override readonly name = 'UpdateError'
}

/** Largest manifest or signature the updater reads; a real one is a few hundred bytes. */
const MAX_METADATA_BYTES = 64 * 1024

/**
 * Validate manifest text with the rules the installers apply.
 * @param text - the exact bytes of `latest.json`, decoded.
 * @returns the manifest.
 * @throws {UpdateError} when any field is missing or malformed.
 */
export function parseManifest(text: string): ReleaseManifest {
  let value: unknown
  try { value = JSON.parse(text) } catch { throw new UpdateError('The release manifest is not JSON') }
  if (typeof value !== 'object' || value === null) throw new UpdateError('The release manifest is not an object')
  const { version, artifacts } = value as { version?: unknown; artifacts?: unknown }
  if (!isReleaseVersion(version)) throw new UpdateError('The release manifest has an invalid version')
  if (typeof artifacts !== 'object' || artifacts === null || Array.isArray(artifacts)) {
    throw new UpdateError('The release manifest lists no artifacts')
  }
  const parsed: Partial<Record<ReleaseTarget, ReleaseArtifact>> = {}
  for (const [target, artifact] of Object.entries(artifacts as Record<string, unknown>)) {
    if (!(RELEASE_TARGETS as readonly string[]).includes(target)) throw new UpdateError(`The release manifest names an unsupported target: ${target}`)
    const { file, sha256, size } = (artifact ?? {}) as { file?: unknown; sha256?: unknown; size?: unknown }
    if (file !== `bake-v${version}-${target}.tar.gz` || typeof sha256 !== 'string' || !/^[a-f0-9]{64}$/.test(sha256)
      || typeof size !== 'number' || !Number.isSafeInteger(size) || size <= 0) {
      throw new UpdateError(`The release manifest has an invalid ${target} artifact`)
    }
    parsed[target as ReleaseTarget] = { file, sha256, size }
  }
  return { version, artifacts: parsed }
}

/**
 * Whether one of `keys` signed `bytes`.
 *
 * The signature is Ed25519 over the manifest's exact bytes, so a manifest
 * that was reformatted, however harmlessly, no longer verifies.
 *
 * @param bytes - the manifest as served.
 * @param signature - base64 signature, surrounding whitespace ignored.
 * @param keys - trusted public keys, base64 DER SubjectPublicKeyInfo.
 * @returns whether any key verifies the signature.
 */
export function verifySignature(bytes: Uint8Array, signature: string, keys: readonly string[]): boolean {
  const decoded = Buffer.from(signature.trim(), 'base64')
  if (decoded.length !== 64) return false
  return keys.some((key) => {
    try {
      const publicKey = createPublicKey({ key: Buffer.from(key, 'base64'), format: 'der', type: 'spki' })
      return publicKey.asymmetricKeyType === 'ed25519' && verify(null, bytes, publicKey, decoded)
    } catch {
      return false
    }
  })
}

/**
 * Where a release's archive lives on `base`.
 *
 * The download service keeps archives under `releases/<version>/`. A GitHub
 * repository serves the manifest as a release asset at
 * `https://github.com/<owner>/<repo>/releases/latest/download`, and each
 * archive as an asset of its own tagged release. The archive is fetched from
 * that tag rather than `latest`, so a release published between reading the
 * manifest and downloading cannot hand over another version's bytes. The
 * installers compute the same URL.
 *
 * @param base - host origin, as {@link ReleaseSource.base} names it.
 * @param version - the manifest's version.
 * @param file - the artifact's file name.
 * @returns the archive's URL.
 */
export function archiveUrl(base: string, version: string, file: string): string {
  const github = /^(https:\/\/github\.com\/[^/]+\/[^/]+\/releases)\/latest\/download$/.exec(base)
  return github === null ? `${base}/releases/${version}/${file}` : `${github[1]}/download/v${version}/${file}`
}

/** How to reach and trust a release host. */
export interface ReleaseSource {
  /** Host origin, without a trailing slash, as `BAKE_RELEASE_BASE_URL` names it. */
  readonly base: string
  /** Trusted public keys; see {@link verifySignature}. */
  readonly keys: readonly string[]
  readonly signal?: AbortSignal
  /** Replaces the global `fetch`, for tests. */
  readonly fetch?: typeof fetch
}

/**
 * Fetch `latest.json` and its signature, and return the manifest only when
 * a trusted key signed exactly those bytes.
 * @param source - where the release host is and which keys to trust.
 * @returns the verified manifest.
 * @throws {UpdateError} on a network failure, an unsigned or forged manifest, or a malformed one.
 */
export async function fetchRelease(source: ReleaseSource): Promise<ReleaseManifest> {
  const [bytes, signature] = await Promise.all([
    download(source, 'latest.json'),
    download(source, 'latest.json.sig'),
  ])
  if (!verifySignature(bytes, new TextDecoder().decode(signature), source.keys)) {
    throw new UpdateError('The release manifest signature did not verify; refusing to trust it')
  }
  return parseManifest(new TextDecoder().decode(bytes))
}

async function download(source: ReleaseSource, name: string): Promise<Uint8Array> {
  const url = `${source.base}/${name}`
  let response: Response
  try {
    response = await (source.fetch ?? fetch)(url, { cache: 'no-store', ...source.signal === undefined ? {} : { signal: source.signal } })
  } catch (error) {
    if (source.signal?.aborted === true) throw error
    throw new UpdateError(`Could not reach ${url}: ${error instanceof Error ? error.message : String(error)}`)
  }
  if (!response.ok) throw new UpdateError(`${url} answered ${response.status}`)
  const bytes = new Uint8Array(await response.arrayBuffer())
  if (bytes.byteLength > MAX_METADATA_BYTES) throw new UpdateError(`${url} is larger than a release manifest can be`)
  return bytes
}
