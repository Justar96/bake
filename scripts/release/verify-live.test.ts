/**
 * A deployment counts as a native release only once the public host serves it,
 * signed by the committed native key, byte for byte, with the 0.3 route intact.
 */
import { expect, test } from 'bun:test'
import { createHash, generateKeyPairSync, sign, type KeyObject } from 'node:crypto'
import { nativeManifestBytes } from './native-manifest.ts'
import { archiveName, type RustTarget } from './pack-rust.ts'
import { verifyLive } from './verify-live.ts'

const BASE = 'https://releases.test'

/** A throwaway Ed25519 pair, with its public half as a client trusts it. */
function keyPair(): { readonly privateKey: KeyObject; readonly publicKey: string } {
  const pair = generateKeyPairSync('ed25519')
  return { privateKey: pair.privateKey, publicKey: pair.publicKey.export({ type: 'spki', format: 'der' }).toString('base64') }
}

const native = keyPair()
const legacy = keyPair()
const keys = [native.publicKey]
const legacyKeys = [legacy.publicKey]

/**
 * A download service serving `versions` in turn, one per native manifest
 * request, each signed by `key`, beside a frozen 0.3 `latest.json`.
 */
function host(versions: string[], options: { key?: KeyObject; targets?: readonly RustTarget[]; archive?: Buffer } = {}) {
  const archive = options.archive ?? Buffer.from('release bytes')
  const files = new Map<string, Uint8Array<ArrayBuffer>>([['health', Buffer.from('ok')], ['install.sh', Buffer.from('')], ['install.ps1', Buffer.from('')]])
  const frozen = Buffer.from(JSON.stringify({ version: '0.3.8', artifacts: { 'linux-x64': {
    file: 'bake-v0.3.8-linux-x64.tar.gz', sha256: 'a'.repeat(64), size: 1,
  } } }))
  files.set('latest.json', frozen)
  files.set('latest.json.sig', Buffer.from(sign(null, frozen, legacy.privateKey).toString('base64')))
  const publish = (version: string): void => {
    const artifacts: Partial<Record<RustTarget, { file: string; sha256: string; size: number }>> = {}
    for (const target of options.targets ?? ['linux-arm64', 'linux-x64']) {
      const file = archiveName(version, target)
      artifacts[target] = { file, sha256: createHash('sha256').update(archive).digest('hex'), size: archive.byteLength }
      files.set(`native/releases/${version}/${file}`, new Uint8Array(archive))
    }
    const manifest = new Uint8Array(nativeManifestBytes({ schema: 'bake-native-release/1', channel: 'stable', version, published: '2026-10-22T00:00:00Z', artifacts }))
    files.set('native/stable.json', manifest)
    files.set('native/stable.json.sig', Buffer.from(sign(null, manifest, options.key ?? native.privateKey).toString('base64')))
  }
  let served = 0
  const next = (): void => {
    const version = versions[Math.min(served, versions.length - 1)]
    served++
    if (version !== undefined) publish(version)
  }
  next()
  served = 0
  const fetch = (async (url: string) => {
    const path = String(url).replace(`${BASE}/`, '')
    if (path === 'native/stable.json' && served < versions.length) next()
    const body = files.get(path)
    return body === undefined ? new Response('', { status: 404 }) : new Response(body)
  }) as unknown as typeof globalThis.fetch
  return { files, fetch }
}

const noWait = async (): Promise<void> => {}
const verify = (fetch: typeof globalThis.fetch, extra: { complete?: boolean; timeoutMs?: number } = {}) =>
  verifyLive({ base: BASE, version: '0.4.1', keys, legacyKeys, fetch, sleep: noWait, ...extra })

test('waits for the new deployment, then verifies every archive it lists', async () => {
  const { fetch } = host(['0.4.0', '0.4.0', '0.4.1'])
  const manifest = await verify(fetch, { complete: true })
  expect(manifest.version).toBe('0.4.1')
  expect(Object.keys(manifest.artifacts)).toEqual(['linux-arm64', 'linux-x64'])
})

test('waits for an archive that appears after its manifest', async () => {
  const served = host(['0.4.1'])
  let attempts = 0
  const fetch = (async (url: string) => {
    if (String(url).endsWith('.tar.gz') && attempts++ === 0) return new Response('', { status: 404 })
    return served.fetch(url)
  }) as unknown as typeof globalThis.fetch
  await expect(verify(fetch)).resolves.toMatchObject({ version: '0.4.1' })
  expect(attempts).toBe(3)
})

test('refuses a native manifest no trusted key signed, however long it waits', async () => {
  const { fetch } = host(['0.4.1'], { key: keyPair().privateKey })
  await expect(verify(fetch, { timeoutMs: 0 })).rejects.toThrow('never served Bake 0.4.1: the native release manifest signature did not verify')
  // The 0.3 key signs no native manifest.
  await expect(verify(host(['0.4.1'], { key: legacy.privateKey }).fetch, { timeoutMs: 0 })).rejects.toThrow('signature did not verify')
})

test('refuses a host that publishes no native release', async () => {
  const served = host(['0.4.1'])
  const fetch = (async (url: string) => String(url).endsWith('/native/stable.json') ? new Response('', { status: 404 }) : served.fetch(url)) as unknown as typeof globalThis.fetch
  await expect(verify(fetch, { timeoutMs: 0 })).rejects.toThrow('never served Bake 0.4.1: it publishes no native release')
})

test('refuses served archive bytes that differ from the manifest', async () => {
  const served = host(['0.4.1'])
  const fetch = (async (url: string) => String(url).endsWith('.tar.gz') ? new Response(Buffer.from('release bytez')) : served.fetch(url)) as unknown as typeof globalThis.fetch
  await expect(verify(fetch)).rejects.toThrow('served bytes do not match')
})

test('refuses an incomplete release when both Linux platforms are required, and any other platform always', async () => {
  await expect(verify(host(['0.4.1'], { targets: ['linux-x64'] }).fetch)).resolves.toMatchObject({ version: '0.4.1' })
  await expect(verify(host(['0.4.1'], { targets: ['linux-x64'] }).fetch, { complete: true })).rejects.toThrow('The live release lacks linux-arm64')
  await expect(verify(host(['0.4.1'], { targets: ['darwin-arm64', 'linux-arm64', 'linux-x64'] }).fetch))
    .rejects.toThrow('The live release names darwin-arm64, which this release does not publish')
})

test('refuses a deployment that dropped or broke the frozen 0.3 route', async () => {
  const dropped = host(['0.4.1'])
  dropped.files.delete('latest.json')
  await expect(verify(dropped.fetch, { timeoutMs: 0 })).rejects.toThrow('The frozen 0.3 route no longer verifies')
  const resigned = host(['0.4.1'])
  const latest = resigned.files.get('latest.json') ?? Buffer.alloc(0)
  resigned.files.set('latest.json.sig', Buffer.from(sign(null, latest, native.privateKey).toString('base64')))
  await expect(verify(resigned.fetch, { timeoutMs: 0 })).rejects.toThrow('The frozen 0.3 route no longer verifies')
})

test('requires the installers and the health check', async () => {
  const { files, fetch } = host(['0.4.1'])
  files.delete('install.sh')
  await expect(verify(fetch, { timeoutMs: 0 })).rejects.toThrow(`${BASE}/install.sh did not become available: answered 404`)
})
