/** Release version checks shared by the release scripts. */

import { changelogSection } from '../../apps/tui/packages/app/src/release.ts'
import { compareVersions, isReleaseVersion } from '../../packages/boot/updater/src/version.ts'

export { changelogSection, compareVersions, isReleaseVersion }

/** The automated channel publishes stable, canonical versions only. */
export function isPublishableVersion(value: unknown): value is string {
  return typeof value === 'string' && /^(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)$/.test(value)
}

/** The line `release:prepare` writes when there is no Unreleased entry to move; a tag with it still in place is refused. */
export const CHANGELOG_PLACEHOLDER = '- Describe the changes in this release.'

/** The TypeScript agent's manifests, whose `version` its 0.3 releases carry. */
export const VERSIONED_MANIFESTS = ['package.json', 'apps/cli/package.json'] as const
