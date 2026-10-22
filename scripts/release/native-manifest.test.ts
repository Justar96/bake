/** The native manifest is its own schema on its own key: the host's gate holds it to those rules, and a 0.3 updater refuses it. */
import { afterEach, describe, expect, test } from 'bun:test'
import { createHash, generateKeyPairSync, sign } from 'node:crypto'
import { copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { RELEASE_PUBLIC_KEYS } from '../../packages/boot/updater/src/keys.ts'
import { parseManifest, verifySignature } from '../../packages/boot/updater/src/manifest.ts'
import { NATIVE_KEY_FILE } from './native-channel.ts'
import {
  nativeManifestBytes, parseNativeManifest, publishedAt, type NativeManifest,
} from './native-manifest.ts'
import { archiveName } from './pack-rust.ts'

const ROOT = resolve(import.meta.dir, '../..')
const roots: string[] = []
afterEach(() => { for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true }) })

const PAYLOAD = 'native release bytes'
const SHA256 = createHash('sha256').update(PAYLOAD).digest('hex')

function manifest(overrides: Partial<NativeManifest> = {}): NativeManifest {
  return {
    schema: 'bake-native-release/1', channel: 'stable', version: '0.4.0', published: '2026-10-20T00:00:00Z',
    artifacts: { 'linux-x64': { file: archiveName('0.4.0', 'linux-x64'), sha256: SHA256, size: PAYLOAD.length } },
    ...overrides,
  }
}

describe('canonical bytes', () => {
  test('are the contract\'s members in order, with a trailing newline', () => {
    expect(nativeManifestBytes(manifest()).toString('utf8')).toBe(
      `{"schema":"bake-native-release/1","channel":"stable","version":"0.4.0","published":"2026-10-20T00:00:00Z","artifacts":{"linux-x64":{"file":"bake-v0.4.0-linux-x64.tar.gz","sha256":"${SHA256}","size":20}}}\n`,
    )
  })

  test('order the artifacts by platform, however the manifest was built', () => {
    const artifacts = {
      'win32-x64': { file: archiveName('0.4.0', 'win32-x64'), sha256: SHA256, size: 1 },
      'darwin-arm64': { file: archiveName('0.4.0', 'darwin-arm64'), sha256: SHA256, size: 1 },
    }
    expect(nativeManifestBytes(manifest({ artifacts })).toString('utf8')).toContain('"artifacts":{"darwin-arm64":{')
  })

  test('name the publication time in UTC with second precision', () => {
    expect(publishedAt(new Date('2026-10-20T12:34:56.789Z'))).toBe('2026-10-20T12:34:56Z')
  })
})

describe('parsing', () => {
  test('accepts the manifest it writes', () => {
    expect(parseNativeManifest(nativeManifestBytes(manifest()).toString('utf8'))).toEqual(manifest())
  })

  test.each([
    ['an unknown member', { note: 'extra' }, /unknown members: note/],
    ['another schema', { schema: 'bake-native-release/2' }, /schema is "bake-native-release\/2"/],
    ['another channel', { channel: 'beta' }, /channel is "beta"/],
    ['an invalid version', { version: '../outside' }, /version is invalid/],
    ['a pre-release with a leading zero', { version: '0.4.0-rc.01' }, /version is invalid: "0\.4\.0-rc\.01"/],
    ['a pre-release with an empty identifier', { version: '0.4.0-rc..1' }, /version is invalid: "0\.4\.0-rc\.\.1"/],
    ['a fractional publication time', { published: '2026-10-20T00:00:00.000Z' }, /published time is invalid/],
    ['an impossible publication time', { published: '2026-02-30T00:00:00Z' }, /published time is invalid/],
    ['no artifacts', { artifacts: {} }, /lists no artifacts/],
    ['an unsupported target', { artifacts: { 'freebsd-x64': { file: 'bake-v0.4.0-freebsd-x64.tar.gz', sha256: SHA256, size: 1 } } }, /unsupported target: freebsd-x64/],
    ['an archive name of another version', { artifacts: { 'linux-x64': { file: 'bake-v0.4.1-linux-x64.tar.gz', sha256: SHA256, size: 1 } } }, /linux-x64 file is "bake-v0\.4\.1-linux-x64\.tar\.gz"/],
    ['an uppercase hash', { artifacts: { 'linux-x64': { file: archiveName('0.4.0', 'linux-x64'), sha256: SHA256.toUpperCase(), size: 1 } } }, /linux-x64 sha256 is invalid/],
    ['a zero size', { artifacts: { 'linux-x64': { file: archiveName('0.4.0', 'linux-x64'), sha256: SHA256, size: 0 } } }, /linux-x64 size is invalid/],
    ['an unknown artifact member', { artifacts: { 'linux-x64': { file: archiveName('0.4.0', 'linux-x64'), sha256: SHA256, size: 1, url: 'http://elsewhere' } } }, /linux-x64 artifact has unknown members: url/],
  ])('refuses %s', (_what, overrides, message) => {
    expect(() => parseNativeManifest(JSON.stringify({ ...manifest(), ...overrides }))).toThrow(message)
  })

  test('refuses text that is not a manifest object', () => {
    expect(() => parseNativeManifest('[]')).toThrow(/body is not an object/)
    expect(() => parseNativeManifest('{')).toThrow(/is not JSON/)
    expect(() => parseNativeManifest('{"schema":"bake-native-release/1"}')).toThrow(/lacks channel, version, published, artifacts/)
  })
})

describe('the native key', () => {
  test('is not the key 0.3 updaters and installers trust', () => {
    const native = readFileSync(join(ROOT, NATIVE_KEY_FILE), 'utf8').trim()
    expect(native).toMatch(/^MCowBQYDK2Vw[A-Za-z0-9+/=]+$/)
    expect(RELEASE_PUBLIC_KEYS).not.toContain(native)
    expect(native).not.toBe(readFileSync(join(ROOT, 'distribution/host/release-key.pub'), 'utf8').trim())
  })

  test("is the owner's, with no placeholder marker beside it", () => {
    expect(existsSync(join(ROOT, 'distribution/host/native-release-key.placeholder'))).toBe(false)
  })
})

describe('a 0.3 updater reading the native route', () => {
  const bytes = nativeManifestBytes(manifest())

  test('cannot tell the native manifest apart by its artifact names', () => {
    // The frozen updater's own parser, reused read-only. It reads neither
    // schema nor channel, and both routes name an archive
    // bake-v<version>-<target>.tar.gz, so this manifest's shape passes: the
    // native route and the native key are what keep a 0.3 updater off it.
    expect(parseManifest(bytes.toString('utf8')).version).toBe('0.4.0')
  })

  test('refuses a native signature, however it was signed', () => {
    const key = generateKeyPairSync('ed25519')
    expect(verifySignature(bytes, sign(null, bytes, key.privateKey).toString('base64'), RELEASE_PUBLIC_KEYS)).toBe(false)
    const native = readFileSync(join(ROOT, NATIVE_KEY_FILE), 'utf8').trim()
    expect(RELEASE_PUBLIC_KEYS).not.toContain(native)
  })
})

describe('the host gate', () => {
  /** A throwaway signing key, trusted through the environment as a local check trusts one. */
  const key = generateKeyPairSync('ed25519')
  const testKey = key.publicKey.export({ type: 'spki', format: 'der' }).toString('base64')

  function save(root: string, bytes: Buffer, signer = key.privateKey): void {
    writeFileSync(join(root, 'public/native/stable.json'), bytes)
    writeFileSync(join(root, 'public/native/stable.json.sig'), `${sign(null, bytes, signer).toString('base64')}\n`)
  }

  function fixture(platforms = ['linux-x64'], payload = PAYLOAD): { root: string; manifest: NativeManifest } {
    const root = mkdtempSync(join(tmpdir(), 'bake-native-manifest-'))
    roots.push(root)
    copyFileSync(join(ROOT, 'distribution/host/verify-native-manifest.mjs'), join(root, 'verify-native-manifest.mjs'))
    copyFileSync(join(ROOT, NATIVE_KEY_FILE), join(root, 'native-release-key.pub'))
    mkdirSync(join(root, 'public/native/releases/0.4.0'), { recursive: true })
    const artifacts: Record<string, { file: string; sha256: string; size: number }> = {}
    for (const target of platforms) {
      const file = `bake-v0.4.0-${target}.tar.gz`
      writeFileSync(join(root, 'public/native/releases/0.4.0', file), payload)
      artifacts[target] = { file, sha256: createHash('sha256').update(payload).digest('hex'), size: Buffer.byteLength(payload) }
    }
    const written = manifest({ artifacts }) as NativeManifest
    save(root, nativeManifestBytes(written))
    return { root, manifest: written }
  }

  function verify(root: string, flags: string[] = [], trusted: string | null = testKey): { code: number; stdout: string; stderr: string } {
    const child = Bun.spawnSync(['node', join(root, 'verify-native-manifest.mjs'), ...flags], {
      stdout: 'pipe', stderr: 'pipe', timeout: 10_000, env: { ...process.env, BAKE_NATIVE_RELEASE_PUBLIC_KEY: trusted ?? '' },
    })
    return { code: child.exitCode, stdout: child.stdout.toString(), stderr: child.stderr.toString() }
  }

  const targets = ['darwin-arm64', 'darwin-x64', 'linux-arm64', 'linux-x64', 'win32-x64']
  const releaseTargets = ['linux-arm64', 'linux-x64']

  test('accepts a verified single-platform release', () => {
    const result = verify(fixture().root)
    expect(result.code).toBe(0)
    expect(result.stdout).toContain('Verified native Bake 0.4.0 release: linux-x64')
  })

  test('complete mode requires exactly linux-arm64 and linux-x64', () => {
    const complete = verify(fixture(releaseTargets).root, ['--complete'])
    expect(complete.code).toBe(0)
    expect(complete.stdout).toContain('Verified native Bake 0.4.0 release: linux-arm64, linux-x64')
    expect(verify(fixture(['linux-x64']).root, ['--complete']).stderr).toContain('Missing or invalid linux-arm64 artifact')
    // A platform 0.4.0 does not release is refused by name, though the grammar reads it.
    const deferred = verify(fixture(targets).root, ['--complete'])
    expect(deferred.code).not.toBe(0)
    expect(deferred.stderr).toContain('Bake 0.4.0 releases linux-arm64 and linux-x64 only; native darwin-arm64 releases arrive in Bake 0.4.x')
    expect(verify(fixture([...releaseTargets, 'win32-x64']).root, ['--complete']).stderr).toContain('native win32-x64 releases arrive in Bake 0.4.x')
  })

  test('a macOS or Windows archive is refused by name whatever the flags, as the image build runs the gate too', () => {
    for (const target of ['darwin-arm64', 'darwin-x64', 'win32-x64']) {
      for (const flags of [[], ['--allow-missing-archives'], ['--required']]) {
        const refused = verify(fixture([...releaseTargets, target]).root, flags)
        expect(refused.code).not.toBe(0)
        expect(refused.stderr).toContain(`Bake 0.4.0 releases linux-arm64 and linux-x64 only; native ${target} releases arrive in Bake 0.4.x`)
      }
    }
    expect(verify(fixture(['win32-x64']).root, ['--allow-missing-archives']).stderr).toContain('native win32-x64 releases arrive in Bake 0.4.x')
  })

  test('an archive held elsewhere is allowed, and one it holds is still checked', () => {
    const { root } = fixture(releaseTargets)
    rmSync(join(root, 'public/native/releases/0.4.0/bake-v0.4.0-linux-x64.tar.gz'))
    expect(verify(root, ['--complete']).code).not.toBe(0)
    const allowed = verify(root, ['--complete', '--allow-missing-archives'])
    expect(allowed.code).toBe(0)
    expect(allowed.stdout).toContain('archives held elsewhere: linux-x64')
    writeFileSync(join(root, 'public/native/releases/0.4.0/bake-v0.4.0-linux-arm64.tar.gz'), `${PAYLOAD}!`)
    expect(verify(root, ['--complete', '--allow-missing-archives']).stderr).toContain('Size mismatch for linux-arm64')
  })

  test('reports no native release when none is staged, unless one is required', () => {
    const { root } = fixture()
    rmSync(join(root, 'public/native'), { recursive: true })
    expect(verify(root)).toMatchObject({ code: 0, stderr: '' })
    expect(verify(root).stdout).toContain('No native release manifest')
    expect(verify(root, ['--required']).stderr).toContain('Missing public/native/stable.json')
  })

  test('refuses a manifest while the committed key is a placeholder', () => {
    const { root } = fixture()
    writeFileSync(join(root, 'native-release-key.placeholder'), 'native-release-key.pub is a PLACEHOLDER.\n')
    expect(verify(root).stderr).toContain('still a placeholder')
  })

  test('refuses a manifest signed by a key it was not told to trust, changed after signing, or unsigned', () => {
    // The service's image build sets no key of its own, so a throwaway one fails there.
    expect(verify(fixture().root, [], null).stderr).toContain('Native release manifest signature does not verify')
    const { root } = fixture()
    writeFileSync(join(root, 'public/native/stable.json'), readFileSync(join(root, 'public/native/stable.json'), 'utf8').replace('0.4.0', '0.4.1'))
    expect(verify(root).stderr).toContain('Native release manifest signature does not verify')
    rmSync(join(root, 'public/native/stable.json.sig'))
    expect(verify(root).stderr).toContain('Missing native release manifest signature')
  })

  test.each([
    ['an unknown member', (m: Record<string, unknown>) => { m.channels = ['stable'] }, 'Unknown native manifest members: channels'],
    ['another schema', (m: Record<string, unknown>) => { m.schema = 'bake-native-release/2' }, 'Unsupported native manifest schema'],
    ['another channel', (m: Record<string, unknown>) => { m.channel = 'beta' }, 'Unsupported native release channel'],
    ['an invalid version', (m: Record<string, unknown>) => { m.version = '../outside' }, 'Invalid native release version'],
    ['a pre-release with a leading zero', (m: Record<string, unknown>) => { m.version = '0.4.0-rc.01' }, 'Invalid native release version'],
    ['a pre-release with an empty identifier', (m: Record<string, unknown>) => { m.version = '0.4.0-rc..1' }, 'Invalid native release version'],
    ['a core number with a leading zero', (m: Record<string, unknown>) => { m.version = '0.04.0' }, 'Invalid native release version'],
    ['an invalid publication time', (m: Record<string, unknown>) => { m.published = 'yesterday' }, 'Invalid native release published time'],
    ['a rolled-over publication date', (m: Record<string, unknown>) => { m.published = '2026-02-30T00:00:00Z' }, 'Invalid native release published time'],
    ['no artifacts', (m: Record<string, unknown>) => { m.artifacts = {} }, 'No native release artifacts'],
    ['an unsupported target', (m: Record<string, unknown>) => { (m.artifacts as Record<string, unknown>)['freebsd-x64'] = { file: 'x', sha256: 'y', size: 1 } }, 'Unsupported native release target: freebsd-x64'],
    ['an archive name of another version', (m: Record<string, unknown>) => { ((m.artifacts as Record<string, Record<string, unknown>>)['linux-x64'] as Record<string, unknown>).file = 'bake-v0.4.1-linux-x64.tar.gz' }, 'Missing or invalid linux-x64 artifact'],
    ['an extra artifact member', (m: Record<string, unknown>) => { ((m.artifacts as Record<string, Record<string, unknown>>)['linux-x64'] as Record<string, unknown>).url = 'http://elsewhere' }, 'Unknown linux-x64 artifact members: url'],
    ['a wrong hash', (m: Record<string, unknown>) => { ((m.artifacts as Record<string, Record<string, unknown>>)['linux-x64'] as Record<string, unknown>).sha256 = '0'.repeat(64) }, 'SHA-256 mismatch for linux-x64'],
    ['a wrong size', (m: Record<string, unknown>) => { ((m.artifacts as Record<string, Record<string, unknown>>)['linux-x64'] as Record<string, unknown>).size = 7 }, 'Size mismatch for linux-x64'],
  ])('refuses %s', (_what, edit, message) => {
    const { root, manifest: written } = fixture()
    const value = JSON.parse(JSON.stringify(written)) as Record<string, unknown>
    edit(value)
    save(root, Buffer.from(`${JSON.stringify(value)}\n`))
    const result = verify(root)
    expect(result.code).not.toBe(0)
    expect(result.stderr).toContain(message)
  })

  test('expects the archive name the packager writes', () => {
    const source = readFileSync(join(ROOT, 'distribution/host/verify-native-manifest.mjs'), 'utf8')
    expect(source).toContain(`\`${archiveName('${manifest.version}', 'win32-x64').replace('win32-x64', '${target}')}\``)
  })
})
