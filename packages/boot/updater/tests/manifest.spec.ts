/** A manifest is trusted only when a known key signed its exact bytes, and only in the shape the installers accept. */
import { describe, expect, it } from 'vitest'
import { archiveUrl, compareVersions, fetchRelease, GITHUB_RELEASE_BASE_URL, parseManifest, releaseSource, RELEASE_PUBLIC_KEYS, UpdateError, verifySignature } from '../src/index.ts'
import { ReleaseHost, releaseArchive, Scratch, signingKey } from './fixture.ts'

describe('compareVersions', () => {
  it.each([
    ['0.1.0', '0.1.1', -1], ['0.2.0', '0.1.9', 1], ['1.0.0', '1.0.0', 0], ['0.10.0', '0.9.0', 1],
    ['1.0.0-alpha', '1.0.0', -1], ['1.0.0-alpha.2', '1.0.0-alpha.10', -1], ['1.0.0-alpha.1', '1.0.0-alpha.beta', -1],
    ['1.0.0-beta', '1.0.0-alpha', 1], ['1.0.0-alpha', '1.0.0-alpha.1', -1],
  ] as const)('orders %s against %s', (left, right, expected) => {
    expect(compareVersions(left, right)).toBe(expected)
    expect(compareVersions(right, left)).toBe(-expected || 0)
  })
})

describe('parseManifest', () => {
  const valid = { version: '0.2.0', artifacts: { 'linux-x64': { file: 'bake-v0.2.0-linux-x64.tar.gz', sha256: 'a'.repeat(64), size: 10 } } }

  it('accepts the shape the installers accept', () => {
    expect(parseManifest(JSON.stringify(valid))).toEqual(valid)
  })

  it.each([
    ['not JSON', '{'],
    ['a bad version', JSON.stringify({ ...valid, version: '../x' })],
    ['no artifacts', JSON.stringify({ version: '0.2.0' })],
    ['an unknown target', JSON.stringify({ ...valid, artifacts: { 'plan9-x64': valid.artifacts['linux-x64'] } })],
    ['a file outside its release', JSON.stringify({ ...valid, artifacts: { 'linux-x64': { ...valid.artifacts['linux-x64'], file: '../evil.tar.gz' } } })],
    ['a short hash', JSON.stringify({ ...valid, artifacts: { 'linux-x64': { ...valid.artifacts['linux-x64'], sha256: 'abc' } } })],
    ['an empty archive', JSON.stringify({ ...valid, artifacts: { 'linux-x64': { ...valid.artifacts['linux-x64'], size: 0 } } })],
  ])('rejects %s', (_, text) => {
    expect(() => parseManifest(text)).toThrow(UpdateError)
  })
})

describe('verifySignature', () => {
  const key = signingKey()
  const bytes = Buffer.from('{"version":"0.2.0"}\n')

  it('verifies exact bytes signed by a trusted key', () => {
    expect(verifySignature(bytes, key.sign(bytes), [key.publicKey])).toBe(true)
    // A rotated-in key sits beside the old one.
    expect(verifySignature(bytes, key.sign(bytes), [signingKey().publicKey, key.publicKey])).toBe(true)
  })

  it('rejects changed bytes, another key, and malformed signatures', () => {
    const signature = key.sign(bytes)
    expect(verifySignature(Buffer.from('{"version":"9.9.9"}\n'), signature, [key.publicKey])).toBe(false)
    // Reformatting is a change: the signature covers bytes, not meaning.
    expect(verifySignature(Buffer.from('{ "version": "0.2.0" }\n'), signature, [key.publicKey])).toBe(false)
    expect(verifySignature(bytes, signingKey().sign(bytes), [key.publicKey])).toBe(false)
    expect(verifySignature(bytes, 'not a signature', [key.publicKey])).toBe(false)
    expect(verifySignature(bytes, signature, ['not a key'])).toBe(false)
  })
})

describe('fetchRelease', () => {
  it('returns a manifest only when its signature verifies', async () => {
    const scratch = new Scratch()
    try {
      const key = signingKey()
      const host = new ReleaseHost()
      const manifest = host.publish('0.2.0', 'linux-x64', releaseArchive(scratch, '0.2.0'), key)
      await expect(fetchRelease({ base: host.base, keys: [key.publicKey], fetch: host.fetch })).resolves.toEqual(manifest)
      await expect(fetchRelease({ base: host.base, keys: [signingKey().publicKey], fetch: host.fetch }))
        .rejects.toThrow('signature did not verify')
      host.files.delete('latest.json.sig')
      await expect(fetchRelease({ base: host.base, keys: [key.publicKey], fetch: host.fetch })).rejects.toThrow('answered 404')
    } finally {
      scratch.dispose()
    }
  })

  it('reports an unreachable host as an update error', async () => {
    const fetch = (async () => { throw new TypeError('fetch failed') }) as typeof globalThis.fetch
    await expect(fetchRelease({ base: 'https://down.test', keys: RELEASE_PUBLIC_KEYS, fetch })).rejects.toThrow('Could not reach https://down.test/latest.json')
  })
})

describe('archiveUrl', () => {
  it('finds an archive under its version on the download service, and under its own tag on GitHub', () => {
    expect(archiveUrl('https://bake.justar.dev', '0.2.0', 'bake-v0.2.0-linux-x64.tar.gz'))
      .toBe('https://bake.justar.dev/releases/0.2.0/bake-v0.2.0-linux-x64.tar.gz')
    // Pinned to the tag, not `latest`: a release published in between cannot swap the bytes.
    expect(archiveUrl(GITHUB_RELEASE_BASE_URL, '0.2.0', 'bake-v0.2.0-linux-x64.tar.gz'))
      .toBe('https://github.com/Justar96/bake/releases/download/v0.2.0/bake-v0.2.0-linux-x64.tar.gz')
  })
})

describe('releaseSource', () => {
  it('defaults to the published host and key, and takes one extra key from the environment', () => {
    expect(releaseSource({})).toEqual({ base: 'https://bake.justar.dev', keys: RELEASE_PUBLIC_KEYS })
    expect(releaseSource({ BAKE_RELEASE_BASE_URL: 'http://127.0.0.1:8080/', BAKE_RELEASE_PUBLIC_KEY: 'KEY' }))
      .toEqual({ base: 'http://127.0.0.1:8080', keys: [...RELEASE_PUBLIC_KEYS, 'KEY'] })
  })
})
