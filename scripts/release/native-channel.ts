/**
 * The native channel's publication policy, shared by the TypeScript route's
 * guard, release preflight, and the native assembler.
 *
 * 0.3 bug and security maintenance ends when 0.4.0 ships, and only Rust Bake
 * is published afterwards
 * ([D13](../../docs/roadmap/rust-0.4/scope-00/support.md#decision-register)),
 * so a published native release of 0.4.0 or later closes the TypeScript route.
 * Until the release owner replaces the committed placeholder key, nothing can
 * sign a native manifest, and release preflight says so instead of letting a
 * release reach that failure later.
 */

import { existsSync, readFileSync } from 'node:fs'
import { join } from 'node:path'
import { NATIVE_MANIFEST_PATH, parseNativeManifest } from './native-manifest.ts'
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
