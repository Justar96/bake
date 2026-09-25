#!/usr/bin/env bun
/**
 * Prepare a release commit: `bun run release:prepare <version>`.
 *
 * Sets the version in both release manifests and the lockfile, and gives the
 * changelog a section for it: the `[Unreleased]` entry moves under the new
 * heading, or a placeholder the preflight refuses marks where to write one.
 * Commit the result, then push the tag it prints.
 */

import { readFileSync, writeFileSync } from 'node:fs'
import { join, resolve } from 'node:path'
import { CHANGELOG_PLACEHOLDER, changelogSection, compareVersions, isReleaseVersion, VERSIONED_MANIFESTS, workspaceVersion } from './version.ts'

/** What {@link prepareRelease} changed, for the summary it prints. */
export interface Prepared {
  readonly from: string
  readonly to: string
  /** Whether the changelog section still needs writing. */
  readonly placeholder: boolean
}

/**
 * Move the workspace to `version`.
 * @param root - repository root.
 * @param version - the new release version, newer than the current one.
 * @param today - release date for the changelog heading, `YYYY-MM-DD`.
 * @returns what changed; the lockfile is the caller's to refresh.
 */
export function prepareRelease(root: string, version: string, today: string): Prepared {
  if (!isReleaseVersion(version)) throw new Error(`Not a release version: ${version}`)
  const from = workspaceVersion(root)
  if (compareVersions(version, from) <= 0) throw new Error(`${version} is not newer than the current ${from}`)
  const path = join(root, 'CHANGELOG.md')
  const changelog = readFileSync(path, 'utf8')
  if (changelogSection(changelog, version) !== undefined) throw new Error(`CHANGELOG.md already has a ${version} section`)
  // Edit the text, not a re-serialization, so the manifests keep their layout.
  for (const file of VERSIONED_MANIFESTS) {
    const manifest = join(root, file)
    const text = readFileSync(manifest, 'utf8')
    const field = `"version": "${from}"`
    if (!text.includes(field)) throw new Error(`${file} has no ${field}`)
    writeFileSync(manifest, text.replace(field, `"version": "${version}"`))
  }
  const heading = `## [${version}] - ${today}`
  const unreleased = /^## \[Unreleased\][^\n]*\n([\s\S]*?)(?=^## |(?![\s\S]))/m.exec(changelog)
  let next: string
  let placeholder = false
  const pending = unreleased?.[1]?.trim() ?? ''
  if (unreleased !== null && pending !== '') {
    next = changelog.replace(unreleased[0], `## [Unreleased]\n\n${heading}\n\n${pending}\n\n`)
  } else {
    placeholder = true
    const first = /^## /m.exec(changelog)
    const section = `${heading}\n\n${CHANGELOG_PLACEHOLDER}\n\n`
    next = unreleased !== null
      ? changelog.replace(unreleased[0], `## [Unreleased]\n\n${section}`)
      : first === null ? `${changelog.trimEnd()}\n\n${section}` : `${changelog.slice(0, first.index)}${section}${changelog.slice(first.index)}`
  }
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
  const install = Bun.spawnSync(['bun', 'install'], { cwd: root, stdout: 'inherit', stderr: 'inherit' })
  if (install.exitCode !== 0) throw new Error('bun install could not refresh bun.lock')
  console.log(`\nPrepared Bake ${prepared.from} → ${prepared.to}.`)
  if (prepared.placeholder) console.log(`Write the ${prepared.to} section in CHANGELOG.md; the release refuses the placeholder.`)
  console.log(`Then:\n  git commit -am "release: ${prepared.to}"\n  git tag v${prepared.to}\n  git push origin HEAD v${prepared.to}`)
}
