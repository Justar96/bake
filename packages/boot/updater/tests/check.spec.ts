/** The notice's answer comes from a cache refreshed hourly, sooner after a failure, and only names an update this host can install. */
import { readFileSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'
import { afterEach, describe, expect, it } from 'vitest'
import {
  cachedUpdate, CHECK_CACHE, CHECK_INTERVAL_MS, checksDisabled, FAILED_CHECK_RETRY_MS, hostTarget, recordCheck, refreshCheck,
} from '../src/index.ts'
import { ReleaseHost, releaseArchive, Scratch, signingKey } from './fixture.ts'

const scratches: Scratch[] = []
afterEach(() => { for (const scratch of scratches.splice(0)) scratch.dispose() })

function setup() {
  const scratch = new Scratch()
  scratches.push(scratch)
  const key = signingKey()
  const host = new ReleaseHost()
  host.publish('0.2.0', 'linux-x64', releaseArchive(scratch, '0.2.0'), key)
  let now = 1_000_000
  const refresh = (running = '0.1.0', target = 'linux-x64' as const) => refreshCheck({
    base: host.base, keys: [key.publicKey], fetch: host.fetch, home: scratch.root, running, target, now: () => now,
  })
  return { scratch, host, refresh, advance: (ms: number) => { now += ms } }
}

describe('refreshCheck', () => {
  it('records a newer release and answers from the cache until the check interval has passed', async () => {
    const { scratch, host, refresh, advance } = setup()
    await expect(refresh()).resolves.toBe('0.2.0')
    expect(cachedUpdate(scratch.root, '0.1.0')).toBe('0.2.0')
    const asked = host.requests.length
    advance(CHECK_INTERVAL_MS - 1)
    await expect(refresh()).resolves.toBe('0.2.0')
    expect(host.requests).toHaveLength(asked)
    advance(1)
    await refresh()
    expect(host.requests.length).toBeGreaterThan(asked)
  })

  it('names nothing once the running version has caught up', async () => {
    const { scratch, refresh } = setup()
    await refresh()
    expect(cachedUpdate(scratch.root, '0.2.0')).toBeUndefined()
  })

  it('does not offer a release published without an archive for this platform', async () => {
    const { scratch, refresh } = setup()
    await expect(refresh('0.1.0', 'darwin-arm64' as never)).resolves.toBeUndefined()
    expect(cachedUpdate(scratch.root, '0.1.0')).toBeUndefined()
  })

  it('records a failed check, and asks again sooner than after a successful one', async () => {
    const { scratch, host, refresh, advance } = setup()
    host.files.delete('latest.json.sig')
    await expect(refresh()).resolves.toBeUndefined()
    expect(JSON.parse(readFileSync(join(scratch.root, CHECK_CACHE), 'utf8'))).toEqual({ checkedAt: 1_000_000, failed: true })
    const asked = host.requests.length
    advance(FAILED_CHECK_RETRY_MS - 1)
    await refresh()
    expect(host.requests).toHaveLength(asked)
    // A release that was still publishing is found on the retry, not a day later.
    const key = signingKey()
    host.publish('0.3.0', 'linux-x64', releaseArchive(scratch, '0.3.0'), key)
    advance(1)
    await expect(refreshCheck({
      base: host.base, keys: [key.publicKey], fetch: host.fetch, home: scratch.root, running: '0.1.0', target: 'linux-x64',
      now: () => 1_000_000 + FAILED_CHECK_RETRY_MS,
    })).resolves.toBe('0.3.0')
    expect(FAILED_CHECK_RETRY_MS).toBeLessThan(CHECK_INTERVAL_MS)
  })

  it('shares an answer another command recorded', async () => {
    const { scratch, host, refresh } = setup()
    await recordCheck(scratch.root, { kind: 'current', version: '0.1.0' }, 1_000_000)
    await expect(refresh()).resolves.toBeUndefined()
    expect(host.requests).toHaveLength(0)
  })

  it('treats an unreadable cache as no answer', async () => {
    const { scratch } = setup()
    writeFileSync(join(scratch.root, CHECK_CACHE), '{')
    expect(cachedUpdate(scratch.root, '0.1.0')).toBeUndefined()
  })
})

describe('checksDisabled', () => {
  it.each([[undefined, false], ['', false], ['0', false], ['false', false], ['1', true], ['yes', true]] as const)(
    'BAKE_NO_UPDATE_CHECK=%s disables checks: %s', (value, disabled) => {
      expect(checksDisabled(value === undefined ? {} : { BAKE_NO_UPDATE_CHECK: value })).toBe(disabled)
    })
})

describe('hostTarget', () => {
  it('names only platforms a release can carry', () => {
    expect(hostTarget('linux', 'x64')).toBe('linux-x64')
    expect(hostTarget('win32', 'arm64')).toBeUndefined()
  })
})
