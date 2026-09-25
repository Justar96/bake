/** Release version checks shared by the release scripts. */

import { readFileSync } from 'node:fs'
import { join } from 'node:path'
import { changelogSection } from '../../apps/tui/packages/app/src/release.ts'
import { compareVersions, isReleaseVersion } from '../../packages/boot/updater/src/version.ts'

export { changelogSection, compareVersions, isReleaseVersion }

/** The automated channel publishes stable, canonical versions only. */
export function isPublishableVersion(value: unknown): value is string {
  return typeof value === 'string' && /^(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)$/.test(value)
}

/** The line `release:prepare` writes when there is no Unreleased entry to move; a tag with it still in place is refused. */
export const CHANGELOG_PLACEHOLDER = '- Describe the changes in this release.'

/** Manifests whose `version` is the release version, and must agree. */
export const VERSIONED_MANIFESTS = ['package.json', 'apps/cli/package.json'] as const

/**
 * Read the release version from the workspace.
 * @param root - repository root.
 * @returns the version both manifests carry.
 * @throws when they disagree or the version is not a release version.
 */
export function workspaceVersion(root: string): string {
  const versions = VERSIONED_MANIFESTS.map(file => (JSON.parse(readFileSync(join(root, file), 'utf8')) as { version?: unknown }).version)
  const [version] = versions
  if (versions.some(value => value !== version)) throw new Error(`Release versions differ: ${VERSIONED_MANIFESTS.map((file, index) => `${file} ${String(versions[index])}`).join(', ')}`)
  if (!isReleaseVersion(version)) throw new Error(`Not a release version: ${String(version)}`)
  return version
}
