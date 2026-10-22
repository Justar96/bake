/** The native channel's policy: no 0.3 publication once native 0.4.0 is out, and no release while the native key is a placeholder. */
import { afterEach, describe, expect, test } from 'bun:test'
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import {
  assertTypeScriptPublicationAllowed, NATIVE_KEY_PLACEHOLDER, nativeKeyProblem, publishedNativeVersion,
} from './native-channel.ts'
import { nativeManifestBytes, type NativeManifest } from './native-manifest.ts'
import { archiveName } from './pack-rust.ts'
import { preflight } from './preflight.ts'

const ROOT = resolve(import.meta.dir, '../..')
const roots: string[] = []
afterEach(() => { for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true }) })

function scratch(): string {
  const root = mkdtempSync(join(tmpdir(), 'bake-native-channel-'))
  roots.push(root)
  return root
}

/** A service payload whose native channel publishes `version`, or none. */
function publicRoot(version?: string): string {
  const root = scratch()
  const dir = join(root, 'public')
  mkdirSync(join(dir, 'native'), { recursive: true })
  if (version !== undefined) {
    const manifest: NativeManifest = {
      schema: 'bake-native-release/1', channel: 'stable', version, published: '2026-10-20T00:00:00Z',
      artifacts: { 'linux-x64': { file: archiveName(version, 'linux-x64'), sha256: 'a'.repeat(64), size: 42 } },
    }
    writeFileSync(join(dir, 'native/stable.json'), nativeManifestBytes(manifest))
  }
  return dir
}

describe('the published native version', () => {
  test('is read from the staged manifest, and is none without one', () => {
    expect(publishedNativeVersion(publicRoot('0.4.1'))).toBe('0.4.1')
    expect(publishedNativeVersion(publicRoot())).toBeUndefined()
  })

  test('is refused when the staged manifest cannot be read', () => {
    const dir = publicRoot('0.4.0')
    writeFileSync(join(dir, 'native/stable.json'), '{"schema":"bake-native-release/1"}')
    expect(() => publishedNativeVersion(dir)).toThrow(/is not a readable native release manifest/)
  })
})

describe('0.3 publication', () => {
  test('is allowed while no native release is published', () => {
    expect(() => { assertTypeScriptPublicationAllowed(publicRoot()) }).not.toThrow()
  })

  test('is allowed beside a native prerelease of 0.4.0', () => {
    expect(() => { assertTypeScriptPublicationAllowed(publicRoot('0.4.0-rc.1')) }).not.toThrow()
  })

  test.each(['0.4.0', '0.4.2', '1.0.0'])('is refused once native %s is published', (version) => {
    expect(() => { assertTypeScriptPublicationAllowed(publicRoot(version)) })
      .toThrow(/the TypeScript route is retired: 0\.3 maintenance ends when 0\.4\.0 ships \(D13\)/)
  })
})

describe('the 0.3 assembler', () => {
  // It writes into this repository's own payload, so its wiring is read here
  // and its guards are exercised above.
  const source = readFileSync(join(ROOT, 'scripts/release/assemble.ts'), 'utf8')

  test('checks the native channel before staging anything', () => {
    expect(source).toContain("import { assertTypeScriptPublicationAllowed } from './native-channel.ts'")
    expect(source).toContain('assertTypeScriptPublicationAllowed(publicRoot)')
  })

  test('reads every archive it publishes, so a native archive cannot pass as one of its own', () => {
    expect(source).toContain("import { assertTypeScriptArchive, readArchiveLayout } from './native-archive.ts'")
    expect(source).toContain('assertTypeScriptArchive(file, await readArchiveLayout(path))')
  })
})

describe('the native signing key', () => {
  test('is reported as a placeholder while the marker is committed', () => {
    expect(nativeKeyProblem(ROOT)).toContain('still a placeholder')
  })

  test('is accepted once the owner replaces it and deletes the marker', () => {
    expect(nativeKeyProblem(scratch())).toBeUndefined()
  })
})

describe('release preflight', () => {
  const HEADER = '# Changelog\n\nNotable changes to Bake.\n\n'

  /** A workspace ready to release 0.4.0, with or without the placeholder marker. */
  function workspace(placeholder: boolean): string {
    const root = scratch()
    mkdirSync(join(root, 'rust'), { recursive: true })
    writeFileSync(join(root, 'rust/Cargo.toml'), '[workspace.package]\nversion = "0.4.0"\n')
    writeFileSync(join(root, 'CHANGELOG.md'), `${HEADER}## [0.4.0] - 2026-10-20\n\n- Added the native route.\n`)
    if (placeholder) {
      mkdirSync(join(root, 'distribution/host'), { recursive: true })
      writeFileSync(join(root, NATIVE_KEY_PLACEHOLDER), 'placeholder\n')
    }
    return root
  }

  test('fails while the native key is a placeholder, and passes once it is not', async () => {
    await expect(preflight({ root: workspace(true), tag: 'v0.4.0' })).rejects.toThrow(/native-release-key\.pub is still a placeholder/)
    await expect(preflight({ root: workspace(false), tag: 'v0.4.0' })).resolves.toMatchObject({ version: '0.4.0' })
  })

  test('fails in this repository, which still carries the placeholder', async () => {
    const run = Bun.spawnSync(['bun', 'run', 'release:preflight', '--offline', '--tag', 'v0.4.0'], { cwd: ROOT, stdout: 'pipe', stderr: 'pipe', timeout: 60_000 })
    expect(run.exitCode).toBe(1)
    expect(`${run.stdout.toString()}${run.stderr.toString()}`).toContain('native-release-key.pub is still a placeholder')
  })
})
