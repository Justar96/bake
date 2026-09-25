/** Published subsets retain every archive integrity check and an explicit complete-release gate. */
import { afterEach, expect, test } from 'bun:test'
import { createHash, generateKeyPairSync, sign } from 'node:crypto'
import { copyFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'

const roots: string[] = []
const targets = ['darwin-arm64', 'darwin-x64', 'linux-arm64', 'linux-x64', 'win32-x64']
afterEach(() => { for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true }) })

interface Artifact { file: string; sha256: string; size: number }
interface Manifest { version: string; artifacts: Record<string, Artifact> }

/** A throwaway signing key, trusted through the environment as a local check trusts one. */
const key = generateKeyPairSync('ed25519')
const testKey = key.publicKey.export({ type: 'spki', format: 'der' }).toString('base64')

function save(root: string, manifest: Manifest): void {
  const bytes = Buffer.from(JSON.stringify(manifest))
  writeFileSync(join(root, 'public/latest.json'), bytes)
  writeFileSync(join(root, 'public/latest.json.sig'), sign(null, bytes, key.privateKey).toString('base64'))
}

function fixture(platforms = ['win32-x64'], payload = 'verified release bytes'): { root: string; manifest: Manifest } {
  const root = mkdtempSync(join(tmpdir(), 'bake-manifest-test-'))
  roots.push(root)
  copyFileSync(resolve(import.meta.dir, '../../distribution/host/verify-manifest.mjs'), join(root, 'verify-manifest.mjs'))
  copyFileSync(resolve(import.meta.dir, '../../distribution/host/release-key.pub'), join(root, 'release-key.pub'))
  mkdirSync(join(root, 'public/releases/0.1.0'), { recursive: true })
  const manifest: Manifest = { version: '0.1.0', artifacts: {} }
  for (const target of platforms) {
    const file = `bake-v0.1.0-${target}.tar.gz`
    writeFileSync(join(root, 'public/releases/0.1.0', file), payload)
    manifest.artifacts[target] = { file, sha256: createHash('sha256').update(payload).digest('hex'), size: Buffer.byteLength(payload) }
  }
  save(root, manifest)
  return { root, manifest }
}

function verify(root: string, complete = false, trusted: string | null = testKey): { code: number; stderr: string } {
  const env = { ...process.env, BAKE_RELEASE_PUBLIC_KEY: trusted ?? '' }
  const child = Bun.spawnSync(['node', join(root, 'verify-manifest.mjs'), ...(complete ? ['--complete'] : [])], {
    stdout: 'pipe', stderr: 'pipe', timeout: 10_000, env,
  })
  return { code: child.exitCode, stderr: child.stderr.toString() }
}

test('accepts a verified Windows-only release', () => {
  expect(verify(fixture().root)).toEqual({ code: 0, stderr: '' })
})

test('accepts all five verified platforms in complete mode', () => {
  expect(verify(fixture(targets).root, true)).toEqual({ code: 0, stderr: '' })
})

test('complete mode rejects missing platforms', () => {
  const result = verify(fixture().root, true)
  expect(result.code).not.toBe(0)
  expect(result.stderr).toContain('Missing or invalid darwin-arm64 artifact')
})

test('rejects an empty manifest', () => {
  expect(verify(fixture([]).root).stderr).toContain('No release artifacts')
})

test('rejects unsupported platforms', () => {
  expect(verify(fixture(['unknown-x64']).root).stderr).toContain('Unsupported release target: unknown-x64')
})

test('rejects empty archive payloads', () => {
  expect(verify(fixture(['win32-x64'], '').root).stderr).toContain('Invalid archive size for win32-x64')
})

test.each([
  ['size', 1, 'Size mismatch for win32-x64'],
  ['sha256', '0'.repeat(64), 'SHA-256 mismatch for win32-x64'],
  ['file', '../outside.tar.gz', 'Missing or invalid win32-x64 artifact'],
] as const)('rejects invalid %s metadata', (field, value, message) => {
  const { root, manifest } = fixture()
  for (const artifact of Object.values(manifest.artifacts)) Object.assign(artifact, { [field]: value })
  save(root, manifest)
  const result = verify(root)
  expect(result.code).not.toBe(0)
  expect(result.stderr).toContain(message)
})

test('rejects an invalid release version', () => {
  const { root, manifest } = fixture()
  manifest.version = '../outside'
  save(root, manifest)
  expect(verify(root).stderr).toContain('Invalid release version')
})

test('rejects a manifest signed by a key it was not told to trust', () => {
  // The service's image build sets no key of its own, so a throwaway one fails there.
  expect(verify(fixture().root, false, null).stderr).toContain('Release manifest signature does not verify')
})

test('rejects a manifest changed after signing, and a missing signature', () => {
  const { root } = fixture()
  writeFileSync(join(root, 'public/latest.json'), readFileSync(join(root, 'public/latest.json'), 'utf8').replace('0.1.0', '0.1.0 '))
  expect(verify(root).stderr).toContain('Release manifest signature does not verify')
  rmSync(join(root, 'public/latest.json.sig'))
  expect(verify(root).stderr).toContain('Missing release manifest signature')
})

test('every copy of the release public key is the one the updater trusts', async () => {
  const { RELEASE_PUBLIC_KEYS } = await import('../../packages/boot/updater/src/keys.ts')
  const [trusted] = RELEASE_PUBLIC_KEYS
  expect(readFileSync(resolve(import.meta.dir, '../../distribution/host/release-key.pub'), 'utf8').trim()).toBe(trusted)
  for (const installer of ['install.sh', 'install.ps1']) {
    expect(readFileSync(resolve(import.meta.dir, '../../distribution/host', installer), 'utf8')).toContain(`['${trusted}', process.env.BAKE_RELEASE_PUBLIC_KEY]`)
  }
})
