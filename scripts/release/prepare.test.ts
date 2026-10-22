/** A release is prepared in one command, and a tag that could not install as an update is refused before anything builds. */
import { afterEach, describe, expect, test } from 'bun:test'
import { generateKeyPairSync, sign } from 'node:crypto'
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { nativeVersion, withNativeVersion } from './native-channel.ts'
import { nativeManifestBytes } from './native-manifest.ts'
import { archiveName } from './pack-rust.ts'
import { preflight } from './preflight.ts'
import { prepareRelease } from './prepare.ts'
import { CHANGELOG_PLACEHOLDER } from './version.ts'

const roots: string[] = []
afterEach(() => { for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true }) })

/** A native workspace manifest at `version`, with other tables that also hold a `version`. */
const cargo = (version: string): string => `[workspace]
members = ["crates/*"]
resolver = "3"

[workspace.package]
version = "${version}"
edition = "2024"

[workspace.dependencies]
regex = { version = "1.12" }

[workspace.metadata.decoy]
version = "9.9.9"
`

/** A workspace whose native crates are at `version`, with the given changelog. */
function workspace(version: string, changelog: string): string {
  const root = mkdtempSync(join(tmpdir(), 'bake-release-prepare-'))
  roots.push(root)
  mkdirSync(join(root, 'rust'), { recursive: true })
  writeFileSync(join(root, 'rust/Cargo.toml'), cargo(version))
  writeFileSync(join(root, 'CHANGELOG.md'), changelog)
  return root
}

const HEADER = '# Changelog\n\nNotable changes to Bake.\n\n'

const key = generateKeyPairSync('ed25519')
const keys = [key.publicKey.export({ type: 'spki', format: 'der' }).toString('base64')]

/** A release host whose signed native manifest names `version`, or none. */
const host = (version: string | undefined): typeof fetch => {
  const bytes = version === undefined ? new Uint8Array() : new Uint8Array(nativeManifestBytes({
    schema: 'bake-native-release/1', channel: 'stable', version, published: '2026-10-22T00:00:00Z',
    artifacts: { 'linux-x64': { file: archiveName(version, 'linux-x64'), sha256: 'a'.repeat(64), size: 1 } },
  }))
  return (async (url: string) => {
    if (version === undefined) return new Response('', { status: 404 })
    return new Response(String(url).endsWith('.sig') ? sign(null, bytes, key.privateKey).toString('base64') : bytes)
  }) as unknown as typeof fetch
}

describe('the native version', () => {
  test('is read from [workspace.package], and set there alone, leaving every other byte as it was', () => {
    const root = workspace('0.4.0-dev', HEADER)
    expect(nativeVersion(root)).toBe('0.4.0-dev')
    expect(withNativeVersion(cargo('0.4.0-dev'), '0.4.0')).toBe(cargo('0.4.0'))
  })

  test('is refused when the table has none, or it is not canonical SemVer', () => {
    const root = workspace('0.4.0-dev', HEADER)
    writeFileSync(join(root, 'rust/Cargo.toml'), cargo('0.4.0-dev').replace('version = "0.4.0-dev"\n', ''))
    expect(() => nativeVersion(root)).toThrow('rust/Cargo.toml has no version in [workspace.package]')
    writeFileSync(join(root, 'rust/Cargo.toml'), cargo('0.04.0'))
    expect(() => nativeVersion(root)).toThrow('rust/Cargo.toml names no native release version: 0.04.0')
  })
})

describe('prepareRelease', () => {
  test('moves the native version and the Unreleased entry under the new version, keeping their layout', () => {
    const root = workspace('0.1.0', `${HEADER}## [Unreleased]\n\n- Added bake update.\n\n## [0.1.0]\n\n- First.\n`)
    expect(prepareRelease(root, '0.1.1', '2026-09-25')).toEqual({ from: '0.1.0', to: '0.1.1', placeholder: false })
    expect(readFileSync(join(root, 'rust/Cargo.toml'), 'utf8')).toBe(cargo('0.1.1'))
    expect(readFileSync(join(root, 'CHANGELOG.md'), 'utf8'))
      .toBe(`${HEADER}## [Unreleased]\n\n## [0.1.1] - 2026-09-25\n\n- Added bake update.\n\n## [0.1.0]\n\n- First.\n`)
  })

  test('keeps an entry that quotes a dollar sign as written', () => {
    const entry = "- A command opens with `$`, and `$&` or `$'` stay as typed."
    const root = workspace('0.1.0', `${HEADER}## [Unreleased]\n\n${entry}\n\n## [0.1.0]\n\n- First.\n`)
    prepareRelease(root, '0.1.1', '2026-09-25')
    expect(readFileSync(join(root, 'CHANGELOG.md'), 'utf8'))
      .toBe(`${HEADER}## [Unreleased]\n\n## [0.1.1] - 2026-09-25\n\n${entry}\n\n## [0.1.0]\n\n- First.\n`)
  })

  test('marks where to write the section when nothing is waiting under Unreleased', () => {
    const root = workspace('0.1.0', `${HEADER}## [0.1.0]\n\n- First.\n`)
    expect(prepareRelease(root, '0.2.0', '2026-09-25').placeholder).toBe(true)
    expect(readFileSync(join(root, 'CHANGELOG.md'), 'utf8'))
      .toBe(`${HEADER}## [0.2.0] - 2026-09-25\n\n${CHANGELOG_PLACEHOLDER}\n\n## [0.1.0]\n\n- First.\n`)
  })

  test('releases the development version it follows', () => {
    const root = workspace('0.4.0-dev', `${HEADER}## [Unreleased]\n\n- The Rust agent.\n`)
    expect(prepareRelease(root, '0.4.0', '2026-10-22')).toEqual({ from: '0.4.0-dev', to: '0.4.0', placeholder: false })
    expect(nativeVersion(root)).toBe('0.4.0')
  })

  test.each([['0.1.0', 'not newer'], ['0.0.9', 'not newer'], ['next', 'Not a stable release version'], ['0.1.1-rc.1', 'Not a stable release version'], ['0.01.1', 'Not a stable release version']])('refuses %s', (version, message) => {
    const root = workspace('0.1.0', `${HEADER}## [0.1.0]\n\n- First.\n`)
    expect(() => prepareRelease(root, version, '2026-09-25')).toThrow(message)
    expect(readFileSync(join(root, 'rust/Cargo.toml'), 'utf8')).toBe(cargo('0.1.0'))
  })

  test('leaves every file untouched when the changelog already has the section', () => {
    const changelog = `${HEADER}## [0.1.1]\n\n- Already written.\n\n## [0.1.0]\n\n- First.\n`
    const root = workspace('0.1.0', changelog)
    expect(() => prepareRelease(root, '0.1.1', '2026-09-25')).toThrow('CHANGELOG.md already has a 0.1.1 section')
    expect(readFileSync(join(root, 'rust/Cargo.toml'), 'utf8')).toBe(cargo('0.1.0'))
    expect(readFileSync(join(root, 'CHANGELOG.md'), 'utf8')).toBe(changelog)
  })

  test('rejects an impossible release date before editing', () => {
    const root = workspace('0.1.0', `${HEADER}## [0.1.0]\n\n- First.\n`)
    expect(() => prepareRelease(root, '0.1.1', '2026-02-30')).toThrow('Invalid release date')
    expect(readFileSync(join(root, 'rust/Cargo.toml'), 'utf8')).toBe(cargo('0.1.0'))
  })
})

describe('preflight', () => {
  test('passes a tag naming a new version with a written changelog, and returns its notes', async () => {
    const root = workspace('0.4.1', `${HEADER}## [0.4.1] - 2026-10-22\n\n- Fixed updates.\n\n## [0.4.0]\n\n- First.\n`)
    await expect(preflight({ root, tag: 'v0.4.1', base: 'https://releases.test', fetch: host('0.4.0'), keys }))
      .resolves.toEqual({ version: '0.4.1', notes: '- Fixed updates.' })
    // A first native release, with nothing published on the native route yet.
    await expect(preflight({ root, tag: 'v0.4.1', base: 'https://releases.test', fetch: host(undefined), keys })).resolves.toMatchObject({ version: '0.4.1' })
  })

  test('reports every problem at once', async () => {
    const root = workspace('0.4.0', `${HEADER}## [0.4.0]\n\n${CHANGELOG_PLACEHOLDER}\n`)
    const failure = preflight({ root, tag: 'v0.5.0', base: 'https://releases.test', fetch: host('0.4.0'), keys })
    await expect(failure).rejects.toThrow('Tag v0.5.0 does not name the native workspace version 0.4.0')
    await expect(failure).rejects.toThrow('still holds the placeholder')
    await expect(failure).rejects.toThrow('0.4.0 is not newer than the published native 0.4.0')
  })

  test('refuses a development version', async () => {
    const root = workspace('0.4.0-dev', `${HEADER}## [Unreleased]\n\n- The Rust agent.\n`)
    const failure = preflight({ root, tag: 'v0.4.0-dev' })
    await expect(failure).rejects.toThrow('Automated releases require a stable, canonical version: 0.4.0-dev')
    await expect(failure).rejects.toThrow('CHANGELOG.md has no 0.4.0-dev section')
  })

  test('refuses a version with no changelog section, and names a host it cannot read', async () => {
    const root = workspace('0.4.1', `${HEADER}## [0.4.0]\n\n- First.\n`)
    const down = (async () => { throw new TypeError('fetch failed') }) as unknown as typeof fetch
    const failure = preflight({ root, base: 'https://releases.test', fetch: down, keys })
    await expect(failure).rejects.toThrow('CHANGELOG.md has no 0.4.1 section')
    await expect(failure).rejects.toThrow('Could not read the published native release from https://releases.test: fetch failed')
  })

  test('allows the already deployed version only on a retry, and never an older one', async () => {
    const root = workspace('0.4.1', `${HEADER}## [0.4.1]\n\n- Fixed updates.\n`)
    const options = { root, tag: 'v0.4.1', base: 'https://releases.test', fetch: host('0.4.1'), keys }
    await expect(preflight(options)).rejects.toThrow('not newer than the published native 0.4.1')
    await expect(preflight({ ...options, retry: true })).resolves.toMatchObject({ version: '0.4.1' })
    await expect(preflight({ ...options, fetch: host('0.4.2'), retry: true })).rejects.toThrow('0.4.1 is not newer than the published native 0.4.2')
  })

  test('rejects an unsigned, misrouted, or malformed published native release', async () => {
    const root = workspace('0.4.1', `${HEADER}## [0.4.1]\n\n- Fixed updates.\n`)
    const unsigned = (async (url: string) => String(url).endsWith('.sig') ? new Response('', { status: 404 }) : host('0.4.0')(url)) as unknown as typeof fetch
    await expect(preflight({ root, base: 'https://releases.test', fetch: unsigned, keys })).rejects.toThrow('native/stable.json.sig answered 404')
    const other = [generateKeyPairSync('ed25519').publicKey.export({ type: 'spki', format: 'der' }).toString('base64')]
    await expect(preflight({ root, base: 'https://releases.test', fetch: host('0.4.0'), keys: other })).rejects.toThrow('signature did not verify')
    const legacy = (async (url: string) => {
      const bytes = Buffer.from(JSON.stringify({ version: '0.3.8', artifacts: {} }))
      return new Response(String(url).endsWith('.sig') ? sign(null, bytes, key.privateKey).toString('base64') : bytes)
    }) as unknown as typeof fetch
    await expect(preflight({ root, base: 'https://releases.test', fetch: legacy, keys })).rejects.toThrow('native release manifest')
  })

  test('reads the native route alone', async () => {
    const root = workspace('0.4.1', `${HEADER}## [0.4.1]\n\n- Fixed updates.\n`)
    const requested: string[] = []
    const fetch = (async (url: string) => { requested.push(String(url)); return host('0.4.0')(url) }) as unknown as typeof globalThis.fetch
    await preflight({ root, base: 'https://releases.test', fetch, keys })
    expect(requested).toEqual(['https://releases.test/native/stable.json', 'https://releases.test/native/stable.json.sig'])
  })
})
