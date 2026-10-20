/**
 * The native release manifest: what `native/stable.json` says, and the exact
 * bytes the native key signs.
 *
 * The native route is separate from the TypeScript `latest.json` route
 * ([D45](../../docs/roadmap/rust-0.4/scope-00/support.md#decision-register)),
 * and the native key, which no 0.3 updater trusts, signs it. Both routes name
 * an archive the same way, so a 0.3 updater is kept off this manifest by its
 * route and its signature, not by the shape of its artifact names. `bake`
 * parses these members; `distribution/host/verify-native-manifest.mjs`
 * applies the same rules in the download service's image build.
 */

import { archiveName, isNativeVersion, isRustTarget, RUST_TARGETS, type RustTarget } from './pack-rust.ts'

/** The manifest's `schema`; a reader that does not know it refuses the manifest. */
export const NATIVE_MANIFEST_SCHEMA = 'bake-native-release/1'

/** The only channel 0.4.0 publishes. */
export const NATIVE_CHANNEL = 'stable'

/** The manifest's path under `distribution/host/public/`, which is also its route. */
export const NATIVE_MANIFEST_PATH = 'native/stable.json'

/** The detached signature's path under `distribution/host/public/`. */
export const NATIVE_SIGNATURE_PATH = 'native/stable.json.sig'

/** Where the native archives live under `distribution/host/public/`, one directory per version. */
export const NATIVE_RELEASES_DIR = 'native/releases'

/** One platform's archive, as the manifest describes it. */
export interface NativeArtifact {
  /** `bake-v<version>-<target>.tar.gz`, relative to `native/releases/<version>/`. */
  readonly file: string
  /** Lowercase hex SHA-256 of the archive bytes. */
  readonly sha256: string
  /** Archive size in bytes. */
  readonly size: number
}

/** The published native release and its archives. */
export interface NativeManifest {
  readonly schema: typeof NATIVE_MANIFEST_SCHEMA
  readonly channel: typeof NATIVE_CHANNEL
  readonly version: string
  /** When it was published, UTC with second precision: `YYYY-MM-DDTHH:MM:SSZ`. */
  readonly published: string
  readonly artifacts: Partial<Record<RustTarget, NativeArtifact>>
}

/** The `published` form every manifest carries: UTC, second precision, no fraction. */
export const PUBLISHED_PATTERN = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$/

/** `at` as a manifest's `published` timestamp. */
export function publishedAt(at: Date): string {
  return `${at.toISOString().slice(0, 19)}Z`
}

/**
 * Whether `value` is a `published` timestamp.
 *
 * The round trip refuses a date that exists only by rollover, such as
 * `2026-02-30T00:00:00Z`, which `Date.parse` reads as March 2.
 */
export function isPublishedTime(value: unknown): value is string {
  if (typeof value !== 'string' || !PUBLISHED_PATTERN.test(value) || Number.isNaN(Date.parse(value))) return false
  return publishedAt(new Date(value)) === value
}

/**
 * The manifest's canonical bytes: its members in the order above, its
 * artifacts in platform order, and a trailing newline.
 *
 * The signature covers these exact bytes, so a reader verifies what it was
 * served rather than a re-serialization of it.
 */
export function nativeManifestBytes(manifest: NativeManifest): Buffer {
  const artifacts: Record<string, NativeArtifact> = {}
  for (const target of RUST_TARGETS) {
    const artifact = manifest.artifacts[target]
    if (artifact !== undefined) artifacts[target] = { file: artifact.file, sha256: artifact.sha256, size: artifact.size }
  }
  const ordered = {
    schema: manifest.schema, channel: manifest.channel, version: manifest.version,
    published: manifest.published, artifacts,
  }
  return Buffer.from(`${JSON.stringify(ordered)}\n`)
}

/** The members a manifest and an artifact may carry; any other is refused. */
const MANIFEST_MEMBERS = ['schema', 'channel', 'version', 'published', 'artifacts']
const ARTIFACT_MEMBERS = ['file', 'sha256', 'size']

function members(value: Record<string, unknown>, allowed: readonly string[], what: string): void {
  const unknown = Object.keys(value).filter(key => !allowed.includes(key))
  if (unknown.length > 0) throw new Error(`The native release manifest's ${what} has unknown members: ${unknown.sort().join(', ')}`)
  const missing = allowed.filter(key => !(key in value))
  if (missing.length > 0) throw new Error(`The native release manifest's ${what} lacks ${missing.join(', ')}`)
}

function object(value: unknown, what: string): Record<string, unknown> {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) throw new Error(`The native release manifest's ${what} is not an object`)
  return value as Record<string, unknown>
}

/**
 * Validate native manifest text, with the rules `bake` and the host's
 * verifier apply.
 * @param text - the exact bytes of `native/stable.json`, decoded.
 * @returns the manifest.
 * @throws naming the first member that is missing, unknown, or malformed.
 */
export function parseNativeManifest(text: string): NativeManifest {
  let value: unknown
  try { value = JSON.parse(text) } catch { throw new Error('The native release manifest is not JSON') }
  const manifest = object(value, 'body')
  members(manifest, MANIFEST_MEMBERS, 'body')
  if (manifest.schema !== NATIVE_MANIFEST_SCHEMA) throw new Error(`The native release manifest's schema is ${JSON.stringify(manifest.schema)}, not ${NATIVE_MANIFEST_SCHEMA}`)
  if (manifest.channel !== NATIVE_CHANNEL) throw new Error(`The native release manifest's channel is ${JSON.stringify(manifest.channel)}, not ${NATIVE_CHANNEL}`)
  const { version, published } = manifest
  if (!isNativeVersion(version)) throw new Error(`The native release manifest's version is invalid: ${JSON.stringify(version)}`)
  if (!isPublishedTime(published)) throw new Error(`The native release manifest's published time is invalid: ${JSON.stringify(published)}`)
  const listed = object(manifest.artifacts, 'artifacts')
  const entries = Object.entries(listed)
  if (entries.length === 0) throw new Error('The native release manifest lists no artifacts')
  const artifacts: Partial<Record<RustTarget, NativeArtifact>> = {}
  for (const [target, value] of entries) {
    if (!isRustTarget(target)) throw new Error(`The native release manifest names an unsupported target: ${target}`)
    const artifact = object(value, `${target} artifact`)
    members(artifact, ARTIFACT_MEMBERS, `${target} artifact`)
    const { file, sha256, size } = artifact
    if (file !== archiveName(version, target)) throw new Error(`The native release manifest's ${target} file is ${JSON.stringify(file)}, not ${archiveName(version, target)}`)
    if (typeof sha256 !== 'string' || !/^[a-f0-9]{64}$/.test(sha256)) throw new Error(`The native release manifest's ${target} sha256 is invalid`)
    if (typeof size !== 'number' || !Number.isSafeInteger(size) || size <= 0) throw new Error(`The native release manifest's ${target} size is invalid`)
    artifacts[target] = { file, sha256, size }
  }
  return { schema: NATIVE_MANIFEST_SCHEMA, channel: NATIVE_CHANNEL, version, published, artifacts }
}
