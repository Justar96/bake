/** A release is prepared in one command, and a tag that could not install as an update is refused before anything builds. */
import { afterEach, describe, expect, test } from 'bun:test'
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { preflight } from './preflight.ts'
import { prepareRelease } from './prepare.ts'
import { CHANGELOG_PLACEHOLDER } from './version.ts'

const roots: string[] = []
afterEach(() => { for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true }) })

/** A workspace at `version` with the given changelog. */
function workspace(version: string, changelog: string): string {
  const root = mkdtempSync(join(tmpdir(), 'bake-release-prepare-'))
  roots.push(root)
  mkdirSync(join(root, 'apps/cli'), { recursive: true })
  writeFileSync(join(root, 'package.json'), `{\n  "name": "bake",\n  "version": "${version}",\n  "private": true\n}\n`)
  writeFileSync(join(root, 'apps/cli/package.json'), `{\n  "name": "@deepseek-ai/dsh",\n  "version": "${version}"\n}\n`)
  writeFileSync(join(root, 'CHANGELOG.md'), changelog)
  return root
}

const HEADER = '# Changelog\n\nNotable changes to Bake.\n\n'

/** A release host whose manifest names `version`, or none. */
const host = (version: string | undefined): typeof fetch => (async () => version === undefined
  ? new Response('', { status: 404 })
  : new Response(JSON.stringify({ version, artifacts: {} }))) as unknown as typeof fetch

describe('prepareRelease', () => {
  test('moves both manifests and the Unreleased entry under the new version, keeping their layout', () => {
    const root = workspace('0.1.0', `${HEADER}## [Unreleased]\n\n- Added bake update.\n\n## [0.1.0]\n\n- First.\n`)
    expect(prepareRelease(root, '0.1.1', '2026-09-25')).toEqual({ from: '0.1.0', to: '0.1.1', placeholder: false })
    expect(readFileSync(join(root, 'package.json'), 'utf8')).toBe('{\n  "name": "bake",\n  "version": "0.1.1",\n  "private": true\n}\n')
    expect(readFileSync(join(root, 'apps/cli/package.json'), 'utf8')).toContain('"version": "0.1.1"')
    expect(readFileSync(join(root, 'CHANGELOG.md'), 'utf8'))
      .toBe(`${HEADER}## [Unreleased]\n\n## [0.1.1] - 2026-09-25\n\n- Added bake update.\n\n## [0.1.0]\n\n- First.\n`)
  })

  test('marks where to write the section when nothing is waiting under Unreleased', () => {
    const root = workspace('0.1.0', `${HEADER}## [0.1.0]\n\n- First.\n`)
    expect(prepareRelease(root, '0.2.0', '2026-09-25').placeholder).toBe(true)
    expect(readFileSync(join(root, 'CHANGELOG.md'), 'utf8'))
      .toBe(`${HEADER}## [0.2.0] - 2026-09-25\n\n${CHANGELOG_PLACEHOLDER}\n\n## [0.1.0]\n\n- First.\n`)
  })

  test.each([['0.1.0', 'not newer'], ['0.0.9', 'not newer'], ['next', 'Not a release version']])('refuses %s', (version, message) => {
    const root = workspace('0.1.0', `${HEADER}## [0.1.0]\n\n- First.\n`)
    expect(() => prepareRelease(root, version, '2026-09-25')).toThrow(message)
    expect(readFileSync(join(root, 'package.json'), 'utf8')).toContain('"version": "0.1.0"')
  })
})

describe('preflight', () => {
  test('passes a tag naming a new version with a written changelog, and returns its notes', async () => {
    const root = workspace('0.1.1', `${HEADER}## [0.1.1] - 2026-09-25\n\n- Added bake update.\n\n## [0.1.0]\n\n- First.\n`)
    await expect(preflight({ root, tag: 'v0.1.1', base: 'https://releases.test', fetch: host('0.1.0') }))
      .resolves.toEqual({ version: '0.1.1', notes: '- Added bake update.' })
    // A first release, with nothing published yet.
    await expect(preflight({ root, tag: 'v0.1.1', base: 'https://releases.test', fetch: host(undefined) })).resolves.toMatchObject({ version: '0.1.1' })
  })

  test('reports every problem at once', async () => {
    const root = workspace('0.1.0', `${HEADER}## [0.1.0]\n\n${CHANGELOG_PLACEHOLDER}\n`)
    const failure = preflight({ root, tag: 'v0.2.0', base: 'https://releases.test', fetch: host('0.1.0') })
    await expect(failure).rejects.toThrow('Tag v0.2.0 does not name the workspace version 0.1.0')
    await expect(failure).rejects.toThrow('still holds the placeholder')
    await expect(failure).rejects.toThrow('0.1.0 is not newer than the published 0.1.0')
  })

  test('refuses a version with no changelog section, and names a host it cannot read', async () => {
    const root = workspace('0.1.1', `${HEADER}## [0.1.0]\n\n- First.\n`)
    const down = (async () => { throw new TypeError('fetch failed') }) as unknown as typeof fetch
    const failure = preflight({ root, base: 'https://releases.test', fetch: down })
    await expect(failure).rejects.toThrow('CHANGELOG.md has no 0.1.1 section')
    await expect(failure).rejects.toThrow('Could not read the published release from https://releases.test: fetch failed')
  })
})
