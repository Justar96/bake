/** The running release's version and changelog entry. */
import { afterEach, describe, expect, it } from 'bun:test'
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { pathToFileURL } from 'node:url'
import { bakeVersion, changelogFor, changelogSection } from '../src/release.ts'

const roots: string[] = []
afterEach(() => { for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true }) })

/** A temporary root holding the given files, as a directory URL. */
function rootWith(files: Record<string, string>): URL {
  const root = mkdtempSync(join(tmpdir(), 'bake-release-'))
  roots.push(root)
  for (const [name, text] of Object.entries(files)) writeFileSync(join(root, name), text)
  return pathToFileURL(`${root}/`)
}

const CHANGELOG = `# Changelog

Intro text.

## [Unreleased]

- Upcoming.

## [1.2.0] - 2026-01-02

- Added a thing.
- Fixed another.

## v1.1.0

- Older.
`

describe('bakeVersion', () => {
  it('reads the repository manifest by default', () => {
    const manifest = JSON.parse(readFileSync(join(import.meta.dir, '../../../../../package.json'), 'utf8')) as { version: string }
    expect(bakeVersion()).toBe(manifest.version)
  })

  it('falls back when the manifest is missing or has no version', () => {
    expect(bakeVersion(rootWith({}))).toBe('0.0.0')
    expect(bakeVersion(rootWith({ 'package.json': '{"name":"bake"}' }))).toBe('0.0.0')
    expect(bakeVersion(rootWith({ 'package.json': 'not json' }))).toBe('0.0.0')
  })
})

describe('changelogSection', () => {
  it('returns one version up to the next level-two heading', () => {
    expect(changelogSection(CHANGELOG, '1.2.0')).toBe('## [1.2.0] - 2026-01-02\n\n- Added a thing.\n- Fixed another.')
  })

  it('accepts unbracketed and v-prefixed headings, including the last section', () => {
    expect(changelogSection(CHANGELOG, '1.1.0')).toBe('## v1.1.0\n\n- Older.')
  })

  it('does not match a version that only shares a prefix', () => {
    expect(changelogSection(CHANGELOG, '1.2')).toBeUndefined()
    expect(changelogSection(CHANGELOG, '9.9.9')).toBeUndefined()
  })
})

describe('changelogFor', () => {
  it('reads the entry from the root changelog', async () => {
    const root = rootWith({ 'CHANGELOG.md': CHANGELOG })
    expect(await changelogFor('1.2.0', new AbortController().signal, root)).toContain('- Added a thing.')
  })

  it('reports a missing file as no entry', async () => {
    expect(await changelogFor('1.2.0', new AbortController().signal, rootWith({}))).toBeUndefined()
  })

  it('has an entry for the version this checkout ships', async () => {
    expect(await changelogFor(bakeVersion(), new AbortController().signal)).toStartWith(`## [${bakeVersion()}]`)
  })
})
