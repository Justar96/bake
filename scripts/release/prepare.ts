#!/usr/bin/env bun
/**
 * Prepare a release commit: `bun run release:prepare <version>`.
 *
 * A release from `develop` is native, so this sets the version every native
 * crate carries, `[workspace.package]` in `rust/Cargo.toml`, refreshes both
 * Cargo lockfiles, and gives the changelog a section for it: the
 * `[Unreleased]` entry moves under the new heading, or a placeholder the
 * preflight refuses marks where to write one. It prints the steps that follow.
 */

import { readFileSync, writeFileSync } from 'node:fs'
import { join, resolve } from 'node:path'
import { NATIVE_VERSION_MANIFEST, nativeVersion, withNativeVersion } from './native-channel.ts'
import { CHANGELOG_PLACEHOLDER, changelogSection, compareVersions, isPublishableVersion } from './version.ts'

/** The Cargo workspaces whose lockfiles record the native crates' version. */
export const CARGO_WORKSPACES = ['rust', 'rust/fuzz'] as const

/** What {@link prepareRelease} changed, for the summary it prints. */
export interface Prepared {
  readonly from: string
  readonly to: string
  /** Whether the changelog section still needs writing. */
  readonly placeholder: boolean
}

/**
 * Move the native workspace to `version`.
 * @param root - repository root.
 * @param version - the new release version, newer than the current one.
 * @param today - release date for the changelog heading, `YYYY-MM-DD`.
 * @returns what changed; the lockfiles are the caller's to refresh.
 */
export function prepareRelease(root: string, version: string, today: string): Prepared {
  if (!isPublishableVersion(version)) throw new Error(`Not a stable release version: ${version}`)
  const date = new Date(`${today}T00:00:00Z`)
  if (!/^\d{4}-\d{2}-\d{2}$/.test(today) || Number.isNaN(date.getTime()) || date.toISOString().slice(0, 10) !== today) throw new Error(`Invalid release date: ${today}`)
  const from = nativeVersion(root)
  if (compareVersions(version, from) <= 0) throw new Error(`${version} is not newer than the current ${from}`)
  const path = join(root, 'CHANGELOG.md')
  const changelog = readFileSync(path, 'utf8')
  if (changelogSection(changelog, version) !== undefined) throw new Error(`CHANGELOG.md already has a ${version} section`)
  // Every edit is made before any file is written, so a refused one leaves
  // the workspace as it was.
  const manifest = join(root, NATIVE_VERSION_MANIFEST)
  const cargo = withNativeVersion(readFileSync(manifest, 'utf8'), version)
  const heading = `## [${version}] - ${today}`
  const unreleased = /^## \[Unreleased\][^\n]*\n([\s\S]*?)(?=^## |(?![\s\S]))/m.exec(changelog)
  let next: string
  let placeholder = false
  const pending = unreleased?.[1]?.trim() ?? ''
  if (unreleased !== null && pending !== '') {
    // A replacer function, so a `$` in an entry is not read as a pattern.
    next = changelog.replace(unreleased[0], () => `## [Unreleased]\n\n${heading}\n\n${pending}\n\n`)
  } else {
    placeholder = true
    const first = /^## /m.exec(changelog)
    const section = `${heading}\n\n${CHANGELOG_PLACEHOLDER}\n\n`
    next = unreleased !== null
      ? changelog.replace(unreleased[0], () => `## [Unreleased]\n\n${section}`)
      : first === null ? `${changelog.trimEnd()}\n\n${section}` : `${changelog.slice(0, first.index)}${section}${changelog.slice(first.index)}`
  }
  writeFileSync(manifest, cargo)
  writeFileSync(path, `${next.trimEnd()}\n`)
  return { from, to: version, placeholder }
}

if (import.meta.main) {
  const version = process.argv[2]?.replace(/^v/, '')
  if (version === undefined) {
    console.error('Usage: bun run release:prepare <version>')
    process.exit(1)
  }
  const root = resolve(import.meta.dir, '../..')
  const prepared = prepareRelease(root, version, new Date().toISOString().slice(0, 10))
  // Only the workspace's own crates move; no dependency is resolved again.
  for (const workspace of CARGO_WORKSPACES) {
    const update = Bun.spawnSync(['cargo', 'update', '--workspace', '--offline'], { cwd: join(root, workspace), stdout: 'inherit', stderr: 'inherit' })
    if (update.exitCode !== 0) throw new Error(`cargo update could not refresh ${workspace}/Cargo.lock`)
  }
  console.log(`\nPrepared Bake ${prepared.from} → ${prepared.to}.`)
  if (prepared.placeholder) console.log(`Write the ${prepared.to} section in CHANGELOG.md; the release refuses the placeholder.`)
  console.log(`Then run bun run release:preflight --offline --tag v${prepared.to}, commit as "release: ${prepared.to}", push develop, and open the pull request into main; tag its merge commit v${prepared.to} once it merges.`)
}
