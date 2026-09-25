/** A deployment counts as a release only once the public host serves it, signed by a committed key, byte for byte. */
import { afterEach, expect, test } from 'bun:test'
import { createHash, generateKeyPairSync, sign, type KeyObject } from 'node:crypto'
import { verifyLive } from './verify-live.ts'
import { RELEASE_PUBLIC_KEYS } from '../../packages/boot/updater/src/index.ts'

const saved = [...RELEASE_PUBLIC_KEYS]
afterEach(() => { (RELEASE_PUBLIC_KEYS as string[]).splice(0, Infinity, ...saved) })

/** Trust a throwaway key as if it were committed, for the life of one test. */
function committedKey(): KeyObject {
  const pair = generateKeyPairSync('ed25519')
  ;(RELEASE_PUBLIC_KEYS as string[]).splice(0, Infinity, pair.publicKey.export({ type: 'spki', format: 'der' }).toString('base64'))
  return pair.privateKey
}

/** A host serving `versions` in turn, one per manifest request, each signed by `key`. */
function host(key: KeyObject, versions: string[], archive = Buffer.from('release bytes')) {
  const files = new Map<string, Uint8Array>([['health', Buffer.from('ok')], ['install.sh', Buffer.from('')], ['install.ps1', Buffer.from('')]])
  const publish = (version: string): void => {
    const manifest = Buffer.from(JSON.stringify({ version, artifacts: { 'linux-x64': {
      file: `bake-v${version}-linux-x64.tar.gz`, sha256: createHash('sha256').update(archive).digest('hex'), size: archive.byteLength,
    } } }))
    files.set('latest.json', manifest)
    files.set('latest.json.sig', Buffer.from(sign(null, manifest, key).toString('base64')))
    files.set(`releases/${version}/bake-v${version}-linux-x64.tar.gz`, archive)
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
    const path = String(url).replace('https://releases.test/', '')
    if (path === 'latest.json' && served < versions.length) next()
    const body = files.get(path)
    return body === undefined ? new Response('', { status: 404 }) : new Response(body)
  }) as unknown as typeof globalThis.fetch
  return { files, fetch }
}

const noWait = async (): Promise<void> => {}

test('waits for the new deployment, then verifies every archive it lists', async () => {
  const { fetch } = host(committedKey(), ['0.1.0', '0.1.0', '0.1.1'])
  const manifest = await verifyLive({ base: 'https://releases.test', version: '0.1.1', fetch, sleep: noWait })
  expect(manifest.version).toBe('0.1.1')
})

test('waits for an archive that appears after its manifest', async () => {
  const served = host(committedKey(), ['0.1.1'])
  let attempts = 0
  const fetch = (async (url: string) => {
    if (String(url).endsWith('.tar.gz') && attempts++ === 0) return new Response('', { status: 404 })
    return served.fetch(url)
  }) as unknown as typeof globalThis.fetch
  await expect(verifyLive({ base: 'https://releases.test', version: '0.1.1', fetch, sleep: noWait })).resolves.toMatchObject({ version: '0.1.1' })
  expect(attempts).toBe(2)
})

test('refuses a manifest no committed key signed, however long it waits', async () => {
  committedKey()
  const { fetch } = host(generateKeyPairSync('ed25519').privateKey, ['0.1.1'])
  await expect(verifyLive({ base: 'https://releases.test', version: '0.1.1', fetch, sleep: noWait, timeoutMs: 0 }))
    .rejects.toThrow('never served Bake 0.1.1: The release manifest signature did not verify')
})

test('refuses served archive bytes that differ from the manifest, and an incomplete release when all platforms are required', async () => {
  const key = committedKey()
  const tampered = host(key, ['0.1.1'])
  const original = tampered.fetch
  const fetch = (async (url: string) => String(url).endsWith('.tar.gz') ? new Response(Buffer.from('release bytez')) : original(url)) as unknown as typeof globalThis.fetch
  await expect(verifyLive({ base: 'https://releases.test', version: '0.1.1', fetch, sleep: noWait })).rejects.toThrow('served bytes do not match')
  await expect(verifyLive({ base: 'https://releases.test', version: '0.1.1', fetch: host(key, ['0.1.1']).fetch, sleep: noWait, complete: true }))
    .rejects.toThrow('The live release lacks darwin-arm64, darwin-x64, linux-arm64, win32-x64')
})
