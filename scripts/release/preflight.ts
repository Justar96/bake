#!/usr/bin/env bun
/**
 * Check a release tag before anything is built:
 * `bun run release:preflight --tag v<version> [--notes <file>] [--offline] [--retry]`.
 *
 * The tag must name a stable workspace version with written changelog notes.
 * A new version must follow the one the download host serves; `--retry` may
 * accept the same version after checking its archives with
 * `--expected-manifest`. `--notes` writes the GitHub release text. `--offline`
 * skips the download host; `--github-repository` checks GitHub instead.
 */

import { readFileSync, writeFileSync } from 'node:fs'
import { join, resolve } from 'node:path'
import { RELEASE_PUBLIC_KEYS } from '../../packages/boot/updater/src/keys.ts'
import { parseManifest, type ReleaseManifest, verifySignature } from '../../packages/boot/updater/src/manifest.ts'
import { CHANGELOG_PLACEHOLDER, changelogSection, compareVersions, isPublishableVersion, workspaceVersion } from './version.ts'

/** The deployed 0.1.0 manifest predates signing; only this baseline may lack a signature. */
const LEGACY_UNSIGNED_VERSION = '0.1.0'

/** What {@link preflight} needs, injected so tests own the network. */
export interface PreflightOptions {
  readonly root: string
  /** The pushed tag, `v<version>`; absent for a dry run from a branch. */
  readonly tag?: string | undefined
  /** Release host origin; absent skips the published-version check. */
  readonly base?: string | undefined
  readonly fetch?: typeof fetch
  /** Trusted keys; tests supply a private fixture key. */
  readonly keys?: readonly string[]
  /** A retry may see the version this run already deployed. */
  readonly retry?: boolean
  /** On a retry, the already published version must have these exact artifacts. */
  readonly expectedManifest?: ReleaseManifest
  /** Check GitHub's latest published release when Railway is disabled. */
  readonly githubRepository?: string
  readonly githubToken?: string
}

function sameArtifacts(left: ReleaseManifest, right: ReleaseManifest): boolean {
  const targets = Object.keys(left.artifacts)
  return left.version === right.version && targets.length === Object.keys(right.artifacts).length
    && targets.every((target) => {
      const a = left.artifacts[target as keyof ReleaseManifest['artifacts']]
      const b = right.artifacts[target as keyof ReleaseManifest['artifacts']]
      return a?.file === b?.file && a?.sha256 === b?.sha256 && a?.size === b?.size
    })
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
  if (!isPublishableVersion(version)) problems.push(`Automated releases require a stable, canonical version: ${version}`)
  if (options.tag !== undefined && options.tag !== `v${version}`) problems.push(`Tag ${options.tag} does not name the workspace version ${version}; expected v${version}`)
  const section = changelogSection(readFileSync(join(options.root, 'CHANGELOG.md'), 'utf8'), version)
  const notes = section?.split('\n').slice(1).join('\n').trim() ?? ''
  if (section === undefined) problems.push(`CHANGELOG.md has no ${version} section`)
  else if (notes === '') problems.push(`CHANGELOG.md's ${version} section is empty`)
  else if (notes.includes(CHANGELOG_PLACEHOLDER)) problems.push(`CHANGELOG.md's ${version} section still holds the placeholder`)
  if (options.base !== undefined) {
    let published: ReleaseManifest | undefined
    try {
      const fetcher = options.fetch ?? fetch
      const response = await fetcher(`${options.base}/latest.json`, { cache: 'no-store', signal: AbortSignal.timeout(15_000) })
      if (response.status === 404) published = undefined
      else if (!response.ok) throw new Error(`answered ${response.status}`)
      else {
        const bytes = new Uint8Array(await response.arrayBuffer())
        const parsed = parseManifest(new TextDecoder().decode(bytes))
        const signature = await fetcher(`${options.base}/latest.json.sig`, { cache: 'no-store', signal: AbortSignal.timeout(15_000) })
        if (signature.status === 404 && parsed.version === LEGACY_UNSIGNED_VERSION) published = parsed
        else {
          if (!signature.ok) throw new Error(`latest.json.sig answered ${signature.status}`)
          if (!verifySignature(bytes, await signature.text(), options.keys ?? RELEASE_PUBLIC_KEYS)) throw new Error('the published release signature did not verify')
          published = parsed
        }
      }
    } catch (error) {
      problems.push(`Could not read the published release from ${options.base}: ${error instanceof Error ? error.message : String(error)}`)
    }
    if (published !== undefined) {
      const order = compareVersions(version, published.version)
      if (order < 0 || (order === 0 && options.retry !== true)) {
        problems.push(`${version} is not newer than the published ${published.version}; release a new version`)
      } else if (order === 0 && options.expectedManifest !== undefined && !sameArtifacts(published, options.expectedManifest)) {
        problems.push(`Published Bake ${version} has different archives; a version cannot be republished with different bytes`)
      }
    }
  }
  if (options.githubRepository !== undefined) {
    try {
      const response = await (options.fetch ?? fetch)(`https://api.github.com/repos/${options.githubRepository}/releases/latest`, {
        headers: { Accept: 'application/vnd.github+json', ...(options.githubToken === undefined ? {} : { Authorization: `Bearer ${options.githubToken}` }) },
        signal: AbortSignal.timeout(15_000),
      })
      if (response.status !== 404) {
        if (!response.ok) throw new Error(`answered ${response.status}`)
        const tag = (await response.json() as { tag_name?: unknown }).tag_name
        if (typeof tag !== 'string' || !tag.startsWith('v') || !isPublishableVersion(tag.slice(1))) throw new Error(`invalid latest release tag: ${String(tag)}`)
        if (compareVersions(version, tag.slice(1)) <= 0) problems.push(`${version} is not newer than the GitHub release ${tag}; release a new version`)
      }
    } catch (error) {
      problems.push(`Could not read the latest GitHub release: ${error instanceof Error ? error.message : String(error)}`)
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
    const expected = value('--expected-manifest')
    const expectedManifest = expected === undefined ? undefined : parseManifest(readFileSync(expected, 'utf8'))
    const { version, notes } = await preflight({ root: resolve(import.meta.dir, '../..'), tag: value('--tag'), base: offline ? undefined : base, retry: argv.includes('--retry'), expectedManifest,
      githubRepository: value('--github-repository'), githubToken: process.env.GITHUB_TOKEN })
    const file = value('--notes')
    if (file !== undefined) writeFileSync(file, `${notes}\n`)
    console.log(`Bake ${version} is ready to release`)
    if (process.env.GITHUB_OUTPUT !== undefined) writeFileSync(process.env.GITHUB_OUTPUT, `version=${version}\n`, { flag: 'a' })
  } catch (error) {
    console.error(error instanceof Error ? error.message : String(error))
    process.exit(1)
  }
}
