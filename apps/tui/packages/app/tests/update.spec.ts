/** The status line names a newer release from the cache at once, and from the background check once it answers. */
import { generateKeyPairSync, sign } from 'node:crypto'
import { mkdirSync, mkdtempSync, rmSync, symlinkSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { CHECK_CACHE, hostTarget } from '@deepseek-ai/dsh-updater'
import { UpdateNotice } from '../src/update.ts'

const roots: string[] = []
afterEach(() => { for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true }) })

/** A managed 0.1.0 install, a Bake home, and a host publishing `published`, signed by a throwaway key. */
function fixture(published: string) {
  const root = mkdtempSync(join(tmpdir(), 'bake-update-notice-'))
  roots.push(root)
  const release = join(root, 'install/versions/0.1.0-aaaaaaaaaaaa')
  mkdirSync(release, { recursive: true })
  symlinkSync(release, join(root, 'install/current'))
  const pair = generateKeyPairSync('ed25519')
  const target = hostTarget()!
  const manifest = Buffer.from(JSON.stringify({ version: published, artifacts: {
    [target]: { file: `bake-v${published}-${target}.tar.gz`, sha256: 'a'.repeat(64), size: 1 },
  } }))
  const files = new Map([['latest.json', manifest], ['latest.json.sig', Buffer.from(sign(null, manifest, pair.privateKey).toString('base64'))]])
  const fetch = vi.fn(async (url: string) => {
    const body = files.get(String(url).replace('https://releases.test/', ''))
    return body === undefined ? new Response('', { status: 404 }) : new Response(body)
  }) as unknown as typeof globalThis.fetch
  const home = join(root, 'home')
  const env = {
    DSH_HOME: home, BAKE_RELEASE_BASE_URL: 'https://releases.test',
    BAKE_RELEASE_PUBLIC_KEY: pair.publicKey.export({ type: 'spki', format: 'der' }).toString('base64'),
  }
  return { root, release, home, env, fetch }
}

describe.skipIf(process.platform === 'win32')('UpdateNotice', () => {
  it('names a newer release once the background check answers, then from the cache at the next launch', async () => {
    const { release, env, fetch } = fixture('0.2.0')
    const first = new UpdateNotice({ running: '0.1.0', release, env, fetch })
    const changed = vi.fn()
    first.start(new AbortController().signal, changed)
    expect(first.version).toBeUndefined()
    await first.drain()
    expect(first.version).toBe('0.2.0')
    expect(changed).toHaveBeenCalledOnce()
    // The next launch knows before its first frame, and asks nobody.
    const calls = vi.mocked(fetch).mock.calls.length
    const next = new UpdateNotice({ running: '0.1.0', release, env, fetch })
    next.start(new AbortController().signal, vi.fn())
    expect(next.version).toBe('0.2.0')
    await next.drain()
    expect(vi.mocked(fetch).mock.calls).toHaveLength(calls)
  })

  it('says nothing, and asks nothing, for a source checkout or with checks turned off', async () => {
    const { root, release, env, fetch } = fixture('0.2.0')
    mkdirSync(join(root, 'checkout'))
    for (const notice of [
      new UpdateNotice({ running: '0.1.0', release: join(root, 'checkout'), env, fetch }),
      new UpdateNotice({ running: '0.1.0', release, env: { ...env, BAKE_NO_UPDATE_CHECK: '1' }, fetch }),
    ]) {
      notice.start(new AbortController().signal, vi.fn())
      await notice.drain()
      expect(notice.version).toBeUndefined()
    }
    expect(fetch).not.toHaveBeenCalled()
  })

  it('makes no late change after the application has ended', async () => {
    const { release, env, fetch, home } = fixture('0.2.0')
    mkdirSync(home, { recursive: true })
    writeFileSync(join(home, CHECK_CACHE), JSON.stringify({ checkedAt: 0 }))
    const abort = new AbortController()
    const changed = vi.fn()
    const notice = new UpdateNotice({ running: '0.1.0', release, env, fetch })
    notice.start(abort.signal, changed)
    abort.abort()
    await notice.drain()
    expect(changed).not.toHaveBeenCalled()
  })
})
