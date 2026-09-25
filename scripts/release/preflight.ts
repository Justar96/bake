#!/usr/bin/env bun
/**
 * Check a release tag before anything is built:
 * `bun run release:preflight --tag v<version> [--notes <file>] [--offline]`.
 *
 * The tag must name the workspace version, the changelog must say what the
 * release changes, and the version must be newer than the one the download
 * host serves, since an installed release is named by its version and
 * `bake update` offers only a newer one. `--notes` writes the changelog
 * section for the GitHub release. `--offline` skips the host, for a dry run.
 */

import { readFileSync, writeFileSync } from 'node:fs'
import { join, resolve } from 'node:path'
import { CHANGELOG_PLACEHOLDER, changelogSection, compareVersions, workspaceVersion } from './version.ts'

/** What {@link preflight} needs, injected so tests own the network. */
export interface PreflightOptions {
  readonly root: string
  /** The pushed tag, `v<version>`; absent for a dry run from a branch. */
  readonly tag?: string | undefined
  /** Release host origin; absent skips the published-version check. */
  readonly base?: string | undefined
  readonly fetch?: typeof fetch
}

/**
 * Check that the workspace is ready to release.
 * @param options - where the workspace is, the tag, and the host.
 * @returns the version and its changelog section.
 * @throws with every problem found, one per line.
 */
export async function preflight(options: PreflightOptions): Promise<{ readonly version: string; readonly notes: string }> {
  const problems: string[] = []
  const version = workspaceVersion(options.root)
  if (options.tag !== undefined && options.tag !== `v${version}`) problems.push(`Tag ${options.tag} does not name the workspace version ${version}; expected v${version}`)
  const section = changelogSection(readFileSync(join(options.root, 'CHANGELOG.md'), 'utf8'), version)
  const notes = section?.split('\n').slice(1).join('\n').trim() ?? ''
  if (section === undefined) problems.push(`CHANGELOG.md has no ${version} section`)
  else if (notes === '') problems.push(`CHANGELOG.md's ${version} section is empty`)
  else if (notes.includes(CHANGELOG_PLACEHOLDER)) problems.push(`CHANGELOG.md's ${version} section still holds the placeholder`)
  if (options.base !== undefined) {
    let published: string | undefined
    try {
      const response = await (options.fetch ?? fetch)(`${options.base}/latest.json`, { cache: 'no-store' })
      if (response.status === 404) published = undefined
      else if (!response.ok) throw new Error(`answered ${response.status}`)
      else published = (await response.json() as { version?: unknown }).version as string | undefined
    } catch (error) {
      problems.push(`Could not read the published release from ${options.base}: ${error instanceof Error ? error.message : String(error)}`)
    }
    if (typeof published === 'string' && compareVersions(version, published) <= 0) {
      problems.push(`${version} is not newer than the published ${published}; release a new version`)
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
  const offline = argv.includes('--offline')
  const base = (process.env.BAKE_RELEASE_BASE_URL?.trim() || 'https://bake.justar.dev').replace(/\/+$/, '')
  try {
    const { version, notes } = await preflight({ root: resolve(import.meta.dir, '../..'), tag: value('--tag'), base: offline ? undefined : base })
    const file = value('--notes')
    if (file !== undefined) writeFileSync(file, `${notes}\n`)
    console.log(`Bake ${version} is ready to release`)
    if (process.env.GITHUB_OUTPUT !== undefined) writeFileSync(process.env.GITHUB_OUTPUT, `version=${version}\n`, { flag: 'a' })
  } catch (error) {
    console.error(error instanceof Error ? error.message : String(error))
    process.exit(1)
  }
}
