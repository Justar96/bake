/**
 * The native channel's publication policy and release version, shared by the
 * TypeScript route's guard, the release scripts, and the native assembler.
 *
 * 0.3 bug and security maintenance ends when 0.4.0 ships, and only Rust Bake
 * is published afterwards
 * ([D13](../../docs/roadmap/rust-0.4/scope-00/support.md#decision-register)),
 * so a published native release of 0.4.0 or later closes the TypeScript route.
 * A key whose private half nobody holds must never sign: while a placeholder
 * marker sits beside the committed key, release preflight says so instead of
 * letting a release reach that failure later.
 */

import { existsSync, readFileSync } from 'node:fs'
import { join } from 'node:path'
import { verifySignature } from '../../packages/boot/updater/src/manifest.ts'
import { NATIVE_MANIFEST_PATH, NATIVE_SIGNATURE_PATH, parseNativeManifest, type NativeManifest } from './native-manifest.ts'
import { isNativeVersion } from './pack-rust.ts'
import { compareVersions } from './version.ts'

/** The native release that retires the TypeScript publication route. */
export const RETIREMENT_VERSION = '0.4.0'

/** The committed native public key, whose private half the release owner holds. */
export const NATIVE_KEY_FILE = 'distribution/host/native-release-key.pub'

/** The marker saying {@link NATIVE_KEY_FILE} is still a placeholder nobody can sign with. */
export const NATIVE_KEY_PLACEHOLDER = 'distribution/host/native-release-key.placeholder'

/**
 * The native release version the payload at `publicRoot` publishes.
 * @param publicRoot - the download service's `public/` directory.
 * @returns the version, or undefined when no native manifest is staged there.
 * @throws when a native manifest is staged but unreadable.
 */
export function publishedNativeVersion(publicRoot: string): string | undefined {
  const manifest = join(publicRoot, NATIVE_MANIFEST_PATH)
  if (!existsSync(manifest)) return undefined
  try {
    return parseNativeManifest(readFileSync(manifest, 'utf8')).version
  } catch (error) {
    throw new Error(`${manifest} is not a readable native release manifest, so 0.3 publication cannot be checked against it: ${error instanceof Error ? error.message : String(error)}`)
  }
}

/**
 * Check that the TypeScript route may still publish.
 * @param publicRoot - the download service's `public/` directory.
 * @throws when a native release of {@link RETIREMENT_VERSION} or later is published there.
 */
export function assertTypeScriptPublicationAllowed(publicRoot: string): void {
  const native = publishedNativeVersion(publicRoot)
  if (native !== undefined && compareVersions(native, RETIREMENT_VERSION) >= 0) {
    throw new Error(`Native Bake ${native} is published, so the TypeScript route is retired: 0.3 maintenance ends when ${RETIREMENT_VERSION} ships (D13)`)
  }
}

/**
 * Why a release cannot be signed with the committed native key yet.
 * @param root - repository root.
 * @returns the problem, or undefined when the owner's key is in place.
 */
export function nativeKeyProblem(root: string): string | undefined {
  if (!existsSync(join(root, NATIVE_KEY_PLACEHOLDER))) return undefined
  return `${NATIVE_KEY_FILE} is still a placeholder: the release owner must replace it with the native release key and delete ${NATIVE_KEY_PLACEHOLDER}`
}

/** The Cargo manifest whose `[workspace.package]` version every native crate, and so each native release, carries. */
export const NATIVE_VERSION_MANIFEST = 'rust/Cargo.toml'

/** Where the `version` line of `[workspace.package]` sits in the manifest's text. */
function workspacePackageVersion(text: string): { readonly start: number; readonly end: number; readonly version: string } {
  const table = /^\[workspace\.package\][^\n]*\n(?:(?!\[)[^\n]*(?:\n|(?![\s\S])))*/m.exec(text)
  const line = table === null ? null : /^version = "([^"\n]*)"[ \t]*$/m.exec(table[0])
  if (table === null || line === null) throw new Error(`${NATIVE_VERSION_MANIFEST} has no version in [workspace.package]`)
  const start = table.index + line.index
  return { start, end: start + line[0].length, version: line[1] ?? '' }
}

/**
 * Read the version the next native release carries.
 * @param root - repository root.
 * @returns the `[workspace.package]` version of {@link NATIVE_VERSION_MANIFEST}.
 * @throws when it has none, or it is not canonical SemVer.
 */
export function nativeVersion(root: string): string {
  const { version } = workspacePackageVersion(readFileSync(join(root, NATIVE_VERSION_MANIFEST), 'utf8'))
  if (!isNativeVersion(version)) throw new Error(`${NATIVE_VERSION_MANIFEST} names no native release version: ${version}`)
  return version
}

/**
 * Set the `[workspace.package]` version in a Cargo manifest's text, leaving
 * every other byte as it was.
 * @param text - the manifest.
 * @param version - the new version.
 * @returns the edited manifest.
 * @throws when the manifest has no workspace version to set.
 */
export function withNativeVersion(text: string, version: string): string {
  const { start, end } = workspacePackageVersion(text)
  return `${text.slice(0, start)}version = "${version}"${text.slice(end)}`
}

/** The native public key a client trusts, as {@link NATIVE_KEY_FILE} commits it. */
export function committedNativeKey(root: string): string {
  return readFileSync(join(root, NATIVE_KEY_FILE), 'utf8').trim()
}

/** How {@link fetchNativeRelease} reaches and trusts a host. */
export interface NativeReleaseSource {
  /** The host origin; the manifest is at `<base>/native/stable.json`. */
  readonly base: string
  /** Trusted base64 SPKI keys; a release check passes {@link committedNativeKey} alone. */
  readonly keys: readonly string[]
  readonly fetch?: typeof fetch | undefined
  readonly timeoutMs?: number | undefined
}

/**
 * Read the native release a host publishes, verified as a client verifies it:
 * the signature over the exact bytes, by a trusted key, then the manifest's
 * own rules.
 * @returns the manifest, or undefined when the host publishes none (404).
 * @throws when the host answers anything else, or refuses the signature, or
 * the manifest is not one.
 */
export async function fetchNativeRelease(source: NativeReleaseSource): Promise<NativeManifest | undefined> {
  const fetcher = source.fetch ?? fetch
  const get = (path: string): Promise<Response> => fetcher(`${source.base}/${path}`, { cache: 'no-store', signal: AbortSignal.timeout(source.timeoutMs ?? 15_000) })
  const response = await get(NATIVE_MANIFEST_PATH)
  if (response.status === 404) return undefined
  if (!response.ok) throw new Error(`${NATIVE_MANIFEST_PATH} answered ${response.status}`)
  const bytes = new Uint8Array(await response.arrayBuffer())
  const signature = await get(NATIVE_SIGNATURE_PATH)
  if (!signature.ok) throw new Error(`${NATIVE_SIGNATURE_PATH} answered ${signature.status}`)
  if (!verifySignature(bytes, await signature.text(), source.keys)) throw new Error('the native release manifest signature did not verify')
  return parseNativeManifest(new TextDecoder().decode(bytes))
}
