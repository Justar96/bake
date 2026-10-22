#!/usr/bin/env bun
/**
 * Check a release tag before anything is built:
 * `bun run release:preflight --tag v<version> [--notes <file>] [--offline] [--retry] [--dry-run]`.
 *
 * A release from `develop` is native. The tag must name the native workspace
 * version, stable, with written changelog notes, and the owner's native key
 * must be committed. A new version must follow the native release the
 * download host serves; `--retry` accepts the same version, which a rerun's
 * publish job takes from the version's published GitHub release instead of
 * signing it again. `--notes` writes the GitHub release text. `--offline`
 * skips the download host. `--dry-run` checks no tag and reports, without
 * failing, what would refuse one, so a manual workflow run can build whatever
 * version the workspace holds.
 */

import { readFileSync, writeFileSync } from 'node:fs'
import { join, resolve } from 'node:path'
import { committedNativeKey, fetchNativeRelease, nativeKeyProblem, nativeVersion } from './native-channel.ts'
import type { NativeManifest } from './native-manifest.ts'
import { CHANGELOG_PLACEHOLDER, changelogSection, compareVersions, isPublishableVersion } from './version.ts'

/** What {@link preflight} needs, injected so tests own the network. */
export interface PreflightOptions {
  readonly root: string
  /** The pushed tag, `v<version>`; absent for a dry run from a branch. */
  readonly tag?: string | undefined
  /** Release host origin; absent skips the published-version check. */
  readonly base?: string | undefined
  readonly fetch?: typeof fetch
  /** Trusted native keys; the committed native key by default, and tests supply a private fixture key. */
  readonly keys?: readonly string[]
  /** A retry may see the version this run already deployed. */
  readonly retry?: boolean
}

/**
 * Check that the workspace is ready to release.
 * @param options - where the workspace is, the tag, and the host.
 * @returns the version and its changelog section.
 * @throws with every problem found, one per line.
 */
export async function preflight(options: PreflightOptions): Promise<{ readonly version: string; readonly notes: string }> {
  const problems: string[] = []
  const nativeKey = nativeKeyProblem(options.root)
  if (nativeKey !== undefined) problems.push(nativeKey)
  const version = nativeVersion(options.root)
  if (!isPublishableVersion(version)) problems.push(`Automated releases require a stable, canonical version: ${version}`)
  if (options.tag !== undefined && options.tag !== `v${version}`) problems.push(`Tag ${options.tag} does not name the native workspace version ${version}; expected v${version}`)
  const section = changelogSection(readFileSync(join(options.root, 'CHANGELOG.md'), 'utf8'), version)
  const notes = section?.split('\n').slice(1).join('\n').trim() ?? ''
  if (section === undefined) problems.push(`CHANGELOG.md has no ${version} section`)
  else if (notes === '') problems.push(`CHANGELOG.md's ${version} section is empty`)
  else if (notes.includes(CHANGELOG_PLACEHOLDER)) problems.push(`CHANGELOG.md's ${version} section still holds the placeholder`)
  if (options.base !== undefined) {
    let published: NativeManifest | undefined
    try {
      const keys = options.keys ?? [committedNativeKey(options.root)]
      published = await fetchNativeRelease({ base: options.base, keys, fetch: options.fetch })
    } catch (error) {
      problems.push(`Could not read the published native release from ${options.base}: ${error instanceof Error ? error.message : String(error)}`)
    }
    if (published !== undefined) {
      const order = compareVersions(version, published.version)
      if (order < 0 || (order === 0 && options.retry !== true)) {
        problems.push(`${version} is not newer than the published native ${published.version}; release a new version`)
      }
    }
  }
  if (problems.length > 0) throw new Error(problems.join('\n'))
  return { version, notes }
}

if (import.meta.main) {
  const argv = process.argv.slice(2)
  const value = (flag: string): string | undefined => {
    const index = argv.indexOf(flag)
    return index === -1 ? undefined : argv[index + 1]
  }
  const dryRun = argv.includes('--dry-run')
  const offline = dryRun || argv.includes('--offline')
  const root = resolve(import.meta.dir, '../..')
  const base = (process.env.BAKE_RELEASE_BASE_URL?.trim() || 'https://bake.justar.dev').replace(/\/+$/, '')
  let version: string
  let notes = ''
  try {
    ({ version, notes } = await preflight({ root, tag: dryRun ? undefined : value('--tag'), base: offline ? undefined : base, retry: argv.includes('--retry') }))
    console.log(`Bake ${version} is ready to release`)
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error)
    if (!dryRun) {
      console.error(message)
      process.exit(1)
    }
    version = nativeVersion(root)
    console.log(`A dry run builds Bake ${version}; a release tag would be refused:\n${message}`)
  }
  const file = value('--notes')
  if (file !== undefined) writeFileSync(file, `${notes}\n`)
  if (process.env.GITHUB_OUTPUT !== undefined) writeFileSync(process.env.GITHUB_OUTPUT, `version=${version}\n`, { flag: 'a' })
}
