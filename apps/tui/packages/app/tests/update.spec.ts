/**
 * The status line names a newer release from the cache at once, from the
 * background check once it answers, and again whenever a later check finds
 * one; `/update` installs it and the line then asks for a restart.
 */
import { createHash, generateKeyPairSync, sign } from 'node:crypto'
import { execFileSync } from 'node:child_process'
import { mkdirSync, mkdtempSync, readFileSync, readlinkSync, rmSync, symlinkSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { CHECK_CACHE, CHECK_INTERVAL_MS, hostTarget } from '@deepseek-ai/dsh-updater'
import { dictionaries } from '@dsh-tui/ui/copy.ts'
import { progressText, Updates } from '../src/update.ts'

const copy = dictionaries.en
const roots: string[] = []
afterEach(() => { for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true }) })

/**
 * A managed 0.1.0 install, a Bake home, and a host whose `publish` puts a
 * release up, signed by a throwaway key, with a real archive whose command
 * prints its version.
 */
function fixture(published: string) {
  const root = mkdtempSync(join(tmpdir(), 'bake-update-notice-'))
  roots.push(root)
  const release = join(root, 'install/versions/0.1.0-aaaaaaaaaaaa')
  mkdirSync(release, { recursive: true })
  symlinkSync(release, join(root, 'install/current'))
  const pair = generateKeyPairSync('ed25519')
  const target = hostTarget()!
  const files = new Map<string, Buffer<ArrayBuffer>>()
  const publish = (version: string): void => {
    const tree = join(root, `tree-${version}`)
    mkdirSync(join(tree, 'apps/cli/lib'), { recursive: true })
    writeFileSync(join(tree, 'apps/cli/lib/bin.js'), `console.log(${JSON.stringify(version)})\n`)
    const packed = join(root, `${version}.tar.gz`)
    execFileSync('tar', ['-czf', packed, '-C', tree, '.'])
    const archive = readFileSync(packed)
    const file = `bake-v${version}-${target}.tar.gz`
    const manifest = Buffer.from(JSON.stringify({ version, artifacts: {
      [target]: { file, sha256: createHash('sha256').update(archive).digest('hex'), size: archive.byteLength },
    } }))
    files.set('latest.json', manifest)
    files.set('latest.json.sig', Buffer.from(sign(null, manifest, pair.privateKey).toString('base64')))
    files.set(`releases/${version}/${file}`, archive)
  }
  publish(published)
  const fetch = vi.fn(async (url: string) => {
    const body = files.get(String(url).replace('https://releases.test/', ''))
    return body === undefined ? new Response('', { status: 404 }) : new Response(body)
  }) as unknown as typeof globalThis.fetch
  const home = join(root, 'home')
  const env = {
    DSH_HOME: home, BAKE_RELEASE_BASE_URL: 'https://releases.test',
    BAKE_RELEASE_PUBLIC_KEY: pair.publicKey.export({ type: 'spki', format: 'der' }).toString('base64'),
  }
  return { root, release, home, env, fetch, publish }
}

describe.skipIf(process.platform === 'win32')('Updates', () => {
  it('names a newer release once the background check answers, then from the cache at the next launch', async () => {
    const { release, env, fetch } = fixture('0.2.0')
    const abort = new AbortController()
    const first = new Updates({ running: '0.1.0', release, env, fetch })
    const changed = vi.fn()
    first.start(abort.signal, changed)
    expect(first.state).toBeUndefined()
    await vi.waitFor(() => expect(first.state).toEqual({ version: '0.2.0', installed: false }))
    expect(changed).toHaveBeenCalledOnce()
    abort.abort()
    await first.drain()
    // The next launch knows before its first frame, and asks nobody.
    const calls = vi.mocked(fetch).mock.calls.length
    const next = new Updates({ running: '0.1.0', release, env, fetch })
    const ended = new AbortController()
    next.start(ended.signal, vi.fn())
    expect(next.state).toEqual({ version: '0.2.0', installed: false })
    ended.abort()
    await next.drain()
    expect(vi.mocked(fetch).mock.calls).toHaveLength(calls)
  })

  it('finds a release published while the terminal stays open', async () => {
    const { release, env, fetch, publish } = fixture('0.1.0')
    let now = 1_000_000
    const abort = new AbortController()
    const updates = new Updates({ running: '0.1.0', release, env, fetch, now: () => now, pollMs: 5 })
    updates.start(abort.signal, vi.fn())
    await vi.waitFor(() => expect(fetch).toHaveBeenCalled())
    expect(updates.state).toBeUndefined()
    publish('0.2.0')
    now += CHECK_INTERVAL_MS
    await vi.waitFor(() => expect(updates.state).toEqual({ version: '0.2.0', installed: false }))
    abort.abort()
    await updates.drain()
  })

  it('says nothing, and asks nothing, for a source checkout or with checks turned off', async () => {
    const { root, release, env, fetch } = fixture('0.2.0')
    mkdirSync(join(root, 'checkout'))
    for (const updates of [
      new Updates({ running: '0.1.0', release: join(root, 'checkout'), env, fetch }),
      new Updates({ running: '0.1.0', release, env: { ...env, BAKE_NO_UPDATE_CHECK: '1' }, fetch }),
    ]) {
      updates.start(new AbortController().signal, vi.fn())
      await updates.drain()
      expect(updates.state).toBeUndefined()
    }
    expect(fetch).not.toHaveBeenCalled()
  })

  it('makes no late change after the application has ended', async () => {
    const { release, env, fetch, home } = fixture('0.2.0')
    mkdirSync(home, { recursive: true })
    writeFileSync(join(home, CHECK_CACHE), JSON.stringify({ checkedAt: 0 }))
    const abort = new AbortController()
    const changed = vi.fn()
    const updates = new Updates({ running: '0.1.0', release, env, fetch })
    updates.start(abort.signal, changed)
    abort.abort()
    await updates.drain()
    expect(changed).not.toHaveBeenCalled()
  })

  it('installs from /update with progress, then asks for a restart', async () => {
    const { root, release, env, fetch } = fixture('0.2.0')
    const abort = new AbortController()
    const updates = new Updates({ running: '0.1.0', release, env, fetch })
    const changed = vi.fn()
    updates.start(abort.signal, changed)
    const notices: (string | undefined)[] = []
    const result = await updates.update(copy, text => { notices.push(text) }, new AbortController().signal)
    expect(result).toEqual({ kind: 'success', text: `${copy.updateInstalled}: v0.1.0 → v0.2.0` })
    expect(readlinkSync(join(root, 'install/current'))).toContain('/versions/0.2.0-')
    expect(notices[0]).toBe(copy.updateChecking)
    expect(notices).toContain(`${copy.updateUnpacking} v0.2.0…`)
    expect(notices.filter(text => text?.startsWith(copy.updateDownloading)).length).toBeLessThanOrEqual(101)
    expect(notices.at(-1)).toBeUndefined()
    expect(updates.state).toEqual({ version: '0.2.0', installed: true })
    expect(changed).toHaveBeenCalled()
    abort.abort()
    await updates.drain()
  })

  it('reports that the running release is current', async () => {
    const { release, env, fetch } = fixture('0.1.0')
    const updates = new Updates({ running: '0.1.0', release, env, fetch })
    await expect(updates.update(copy, () => {}, new AbortController().signal))
      .resolves.toEqual({ kind: 'success', text: `${copy.updateCurrent}: v0.1.0` })
  })

  it('refuses a source checkout without asking the host', async () => {
    const { root, env, fetch } = fixture('0.2.0')
    mkdirSync(join(root, 'checkout'))
    const updates = new Updates({ running: '0.1.0', release: join(root, 'checkout'), env, fetch })
    const result = await updates.update(copy, () => {}, new AbortController().signal)
    expect(result).toMatchObject({ kind: 'error' })
    expect(result.kind === 'error' && result.text).toContain(copy.updateUnmanaged)
    expect(fetch).not.toHaveBeenCalled()
  })
})

describe('progressText', () => {
  it('names the step and, while downloading, how far it has got', () => {
    expect(progressText(copy, '0.2.0', { phase: 'download', received: 2_500_000, total: 10_000_000 }))
      .toBe(`${copy.updateDownloading} v0.2.0… 25% (2.5 / 10.0 MB)`)
    expect(progressText(copy, '0.2.0', { phase: 'verify' })).toBe(`${copy.updateVerifying}: v0.2.0…`)
  })
})
