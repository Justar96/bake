#!/usr/bin/env bun
/**
 * Copy checked native archives into the download service payload and sign the
 * native manifest that lists them:
 * `bun run release:assemble:rust <version> [--ephemeral-key] [--key-file <pem>] [--offline]`.
 *
 * The native route is `native/stable.json`, `native/stable.json.sig`, and
 * `native/releases/<version>/` under `distribution/host/public/`, separate
 * from the TypeScript `latest.json` route it never writes
 * ([D45](../../docs/roadmap/rust-0.4/scope-00/support.md#decision-register)).
 * The signing key is Ed25519 PKCS#8 PEM, read from the file `--key-file` or
 * `BAKE_NATIVE_RELEASE_SIGNING_KEY_FILE` names; it never enters the
 * repository, and its public half must be
 * `distribution/host/native-release-key.pub`. `--ephemeral-key` signs a local
 * check with a throwaway key instead and leaves its public half beside the
 * archives, where a check passes it to the clients.
 *
 * See distribution/README.md, "Native release".
 */

import { createPrivateKey, createPublicKey, generateKeyPairSync, sign, type KeyObject } from 'node:crypto'
import { cpSync, existsSync, mkdirSync, readdirSync, readFileSync, writeFileSync } from 'node:fs'
import { join, resolve } from 'node:path'
import { parseArgs } from 'node:util'
import { assertNativeArchive, readArchiveLayout } from './native-archive.ts'
import { NATIVE_KEY_FILE, nativeKeyProblem } from './native-channel.ts'
import {
  NATIVE_CHANNEL, NATIVE_MANIFEST_PATH, NATIVE_MANIFEST_SCHEMA, NATIVE_RELEASES_DIR, NATIVE_SIGNATURE_PATH,
  nativeManifestBytes, parseNativeManifest, publishedAt, type NativeArtifact, type NativeManifest,
} from './native-manifest.ts'
import { archiveName, isNativeVersion, RUST_TARGETS, type RustTarget } from './pack-rust.ts'
import { compareVersions } from './version.ts'

/** Where `--ephemeral-key` leaves the throwaway public key, beside the archives. */
export const EPHEMERAL_PUBLIC_KEY = 'ephemeral-native-release-key.pub'

/** The default archive directory: `release:pack:rust`'s output for this version. */
export function nativeSourceDir(root: string, version: string): string {
  return join(root, '.artifacts/bake-native-release', version)
}

/** What {@link assembleNativeRelease} needs; tests own the key, the clock, and the network. */
export interface AssembleNativeOptions {
  readonly root: string
  readonly version: string
  /** The archives to publish; {@link nativeSourceDir} by default. */
  readonly sourceDir?: string
  /** The service payload; `distribution/host/public` by default. */
  readonly publicRoot?: string
  readonly signingKey: KeyObject
  /** The manifest's `published` time; now by default. */
  readonly now?: Date
  /** The live host origin, checked for a newer published version; absent is `--offline`. */
  readonly base?: string | undefined
  readonly fetch?: typeof fetch
}

export interface NativeReleaseResult {
  readonly manifest: NativeManifest
  /** The signed bytes, as written to `native/stable.json`. */
  readonly bytes: Buffer
  /** Base64 signature, as written to `native/stable.json.sig`. */
  readonly signature: string
  /** The published archive names, in platform order. */
  readonly files: readonly string[]
}

/**
 * The native release version already published, as the local payload and the
 * live host state it.
 * @returns each published version found, by where it was read.
 * @throws when a manifest is present but unreadable, or the host answers
 * anything but the manifest or 404.
 */
async function publishedVersions(options: AssembleNativeOptions, publicRoot: string): Promise<{ where: string; version: string }[]> {
  const found: { where: string; version: string }[] = []
  const local = join(publicRoot, NATIVE_MANIFEST_PATH)
  if (existsSync(local)) found.push({ where: local, version: parseNativeManifest(readFileSync(local, 'utf8')).version })
  if (options.base !== undefined) {
    const url = `${options.base}/${NATIVE_MANIFEST_PATH}`
    const response = await (options.fetch ?? fetch)(url, { cache: 'no-store', signal: AbortSignal.timeout(15_000) })
    if (response.status !== 404) {
      if (!response.ok) throw new Error(`${url} answered ${response.status}; pass --offline to publish without checking the live channel`)
      found.push({ where: url, version: parseNativeManifest(await response.text()).version })
    }
  }
  return found
}

/**
 * Stage and sign a native release.
 *
 * Every archive in the source directory must be this version's archive for a
 * release platform and must hold the native payload; the version must be
 * strictly newer than every published native release, local and live, because
 * a channel never moves backward. Nothing here writes `latest.json` or
 * `releases/`, the TypeScript route.
 *
 * @param options - the version, where its archives are, and the signing key.
 * @returns the manifest, its exact bytes, its signature, and the files staged.
 * @throws before writing anything when the version, an archive, or the
 * channel order is refused.
 */
export async function assembleNativeRelease(options: AssembleNativeOptions): Promise<NativeReleaseResult> {
  const { root, version } = options
  if (!isNativeVersion(version)) throw new Error(`Not a native release version: ${version}`)
  const source = options.sourceDir ?? nativeSourceDir(root, version)
  const publicRoot = options.publicRoot ?? join(root, 'distribution/host/public')
  if (!existsSync(source)) throw new Error(`No archives at ${source}; run bun run release:pack:rust on each platform first`)
  for (const published of await publishedVersions(options, publicRoot)) {
    if (compareVersions(version, published.version) <= 0) {
      throw new Error(`${version} is not newer than the native release ${published.version} published at ${published.where}; a channel never moves backward`)
    }
  }
  const artifacts: Partial<Record<RustTarget, NativeArtifact>> = {}
  const staged: { readonly target: RustTarget; readonly path: string; readonly file: string }[] = []
  for (const file of readdirSync(source).sort()) {
    if (file === EPHEMERAL_PUBLIC_KEY) continue
    const target = RUST_TARGETS.find(candidate => file === archiveName(version, candidate))
    if (target === undefined) {
      throw new Error(`${file} is not a native archive of ${version} for a release platform; expected ${archiveName(version, 'linux-x64')} and its siblings`)
    }
    const path = join(source, file)
    assertNativeArchive(await readArchiveLayout(path))
    const archive = Bun.file(path)
    const size = archive.size
    if (size === 0) throw new Error(`Empty native archive: ${file}`)
    const hash = new Bun.CryptoHasher('sha256')
    for await (const chunk of archive.stream()) hash.update(chunk)
    artifacts[target] = { file, sha256: hash.digest('hex'), size }
    staged.push({ target, path, file })
  }
  if (staged.length === 0) throw new Error(`No native archives found in ${source}`)
  const manifest: NativeManifest = {
    schema: NATIVE_MANIFEST_SCHEMA, channel: NATIVE_CHANNEL, version,
    published: publishedAt(options.now ?? new Date()), artifacts,
  }
  // Parsed back as a client reads it, so nothing unreadable is ever signed.
  const bytes = nativeManifestBytes(manifest)
  parseNativeManifest(bytes.toString('utf8'))
  const destination = join(publicRoot, NATIVE_RELEASES_DIR, version)
  mkdirSync(destination, { recursive: true })
  for (const entry of staged) cpSync(entry.path, join(destination, entry.file))
  const signature = sign(null, bytes, options.signingKey).toString('base64')
  mkdirSync(join(publicRoot, 'native'), { recursive: true })
  writeFileSync(join(publicRoot, NATIVE_MANIFEST_PATH), bytes)
  writeFileSync(join(publicRoot, NATIVE_SIGNATURE_PATH), `${signature}\n`)
  return { manifest, bytes, signature, files: staged.map(entry => entry.file) }
}

/** How {@link nativeSigningKey} is asked for a key. */
export interface SigningKeyRequest {
  readonly root: string
  /** Sign a local check with a throwaway key, written to `sourceDir`. */
  readonly ephemeral: boolean
  /** The PKCS#8 PEM file holding the release key. */
  readonly keyFile?: string | undefined
  readonly sourceDir: string
}

/**
 * Resolve the key that signs this run's manifest.
 * @throws when no key is named, the key is not Ed25519, its public half is not
 * the committed one, or the committed key is still a placeholder.
 */
export function nativeSigningKey(request: SigningKeyRequest): KeyObject {
  if (request.ephemeral) {
    const pair = generateKeyPairSync('ed25519')
    mkdirSync(request.sourceDir, { recursive: true })
    writeFileSync(join(request.sourceDir, EPHEMERAL_PUBLIC_KEY), `${pair.publicKey.export({ type: 'spki', format: 'der' }).toString('base64')}\n`)
    return pair.privateKey
  }
  const placeholder = nativeKeyProblem(request.root)
  if (placeholder !== undefined) throw new Error(`${placeholder}, before a native release is signed`)
  const path = request.keyFile
  if (path === undefined || path === '') {
    throw new Error('Name the native release signing key with --key-file or BAKE_NATIVE_RELEASE_SIGNING_KEY_FILE, or pass --ephemeral-key for a local check')
  }
  const key = createPrivateKey(readFileSync(path))
  if (key.asymmetricKeyType !== 'ed25519') throw new Error('The native release signing key is not Ed25519')
  const published = readFileSync(join(request.root, NATIVE_KEY_FILE), 'utf8').trim()
  if (createPublicKey(key).export({ type: 'spki', format: 'der' }).toString('base64') !== published) {
    throw new Error(`The signing key does not match ${NATIVE_KEY_FILE}`)
  }
  return key
}

/** The live native channel this run checks, unless `--offline`. */
export const DEFAULT_NATIVE_BASE_URL = 'https://bake.justar.dev'

async function main(): Promise<void> {
  const { values, positionals } = parseArgs({
    options: {
      'ephemeral-key': { type: 'boolean', default: false },
      'key-file': { type: 'string' },
      offline: { type: 'boolean', default: false },
    },
    allowPositionals: true, strict: true,
  })
  const [version, ...extra] = positionals
  if (version === undefined || extra.length > 0) throw new Error('Usage: bun run release:assemble:rust <version> [--ephemeral-key] [--key-file <pem>] [--offline]')
  const root = resolve(import.meta.dir, '../..')
  const sourceDir = nativeSourceDir(root, version)
  const signingKey = nativeSigningKey({
    root, ephemeral: values['ephemeral-key'], sourceDir,
    keyFile: values['key-file'] ?? process.env.BAKE_NATIVE_RELEASE_SIGNING_KEY_FILE,
  })
  const base = (process.env.BAKE_NATIVE_RELEASE_BASE_URL?.trim() || DEFAULT_NATIVE_BASE_URL).replace(/\/+$/, '')
  const result = await assembleNativeRelease({ root, version, sourceDir, signingKey, base: values.offline ? undefined : base })
  console.log(`Staged and signed ${Object.keys(result.manifest.artifacts).join(', ')} for native Bake ${version}`)
}

if (import.meta.main) {
  await main().catch((error: unknown) => {
    console.error(`release:assemble:rust: ${error instanceof Error ? error.message : String(error)}`)
    process.exit(1)
  })
}
