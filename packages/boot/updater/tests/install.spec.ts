/** An update installs beside the running release and moves `current` last, or changes nothing. */
import { existsSync, lstatSync, mkdirSync, readdirSync, readFileSync, readlinkSync, rmSync, utimesSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'
import { afterEach, describe, expect, it } from 'vitest'
import {
  acquireLock, currentOf, detectInstall, installRelease, LAUNCH_MARKER, pointAt, PRUNE_AFTER_MS, windowsLauncher,
  type ManagedInstall, type ReleaseManifest,
} from '../src/index.ts'
import { managedInstall, ReleaseHost, releaseArchive, Scratch, signingKey } from './fixture.ts'

const scratches: Scratch[] = []
afterEach(() => { for (const scratch of scratches.splice(0)) scratch.dispose() })

/** Version 0.1.0 installed and running, and 0.2.0 published. */
function setup(published = '0.2.0') {
  const scratch = new Scratch()
  scratches.push(scratch)
  const key = signingKey()
  const host = new ReleaseHost()
  const installed = managedInstall(scratch, '0.1.0', releaseArchive(scratch, '0.1.0'))
  const manifest = host.publish('0.2.0', 'linux-x64', releaseArchive(scratch, published), key)
  const layout = detectInstall(installed.running) as ManagedInstall
  const install = (overrides: Partial<Parameters<typeof installRelease>[0]> = {}) => installRelease({
    base: host.base, keys: [key.publicKey], fetch: host.fetch, layout, manifest,
    artifact: manifest.artifacts['linux-x64']!, platform: 'linux', ...overrides,
  })
  return { scratch, host, key, manifest, layout, install, ...installed }
}

const target = (manifest: ReleaseManifest): string => `0.2.0-${manifest.artifacts['linux-x64']!.sha256.slice(0, 12)}`

describe.skipIf(process.platform === 'win32')('installRelease', () => {
  it('unpacks and starts the new release, then moves current to it and keeps the running one', async () => {
    const { install, root, running, manifest, layout } = setup()
    expect(layout).toMatchObject({ kind: 'managed', root, current: running.split('/').at(-1) })
    const result = await install()
    expect(result).toMatchObject({ directory: target(manifest), previous: running.split('/').at(-1), pruned: [] })
    expect(readlinkSync(join(root, 'current'))).toBe(join(root, 'versions', target(manifest)))
    expect(existsSync(join(running, 'apps/cli/lib/bin.js'))).toBe(true)
    expect(existsSync(join(root, '.staging'))).toBe(false)
    expect(existsSync(join(root, 'update.lock'))).toBe(false)
  })

  it('installs from GitHub releases, fetching the manifest from latest and the archive from its tag', async () => {
    const { install, root, host, manifest } = setup()
    const base = 'https://github.com/owner/bake/releases/latest/download'
    const fetch = (async (input: string) => {
      const url = String(input)
      const path = url === `${base}/latest.json` ? 'latest.json'
        : url.startsWith('https://github.com/owner/bake/releases/download/v0.2.0/') ? `releases/0.2.0/${url.split('/').at(-1)}` : undefined
      const body = path === undefined ? undefined : host.files.get(path)
      return body === undefined ? new Response('', { status: 404 }) : new Response(body)
    }) as unknown as typeof globalThis.fetch
    await install({ base, fetch })
    expect(readlinkSync(join(root, 'current'))).toBe(join(root, 'versions', target(manifest)))
  })

  it('leaves current where it was when the archive does not match its hash', async () => {
    const { install, root, running, host, manifest } = setup()
    const file = `releases/0.2.0/${manifest.artifacts['linux-x64']!.file}`
    const bytes = Buffer.from(host.files.get(file)!)
    bytes[bytes.length - 1] ^= 0xff
    host.files.set(file, bytes)
    await expect(install()).rejects.toThrow('did not match the release manifest\'s SHA-256')
    expect(readlinkSync(join(root, 'current'))).toBe(running)
    expect(readdirSync(join(root, 'versions'))).toEqual([running.split('/').at(-1)])
  })

  it('stops reading an archive that outgrows its stated size', async () => {
    const { install, host, manifest, root, running } = setup()
    const file = `releases/0.2.0/${manifest.artifacts['linux-x64']!.file}`
    host.files.set(file, Buffer.concat([host.files.get(file)!, Buffer.alloc(1024)]))
    await expect(install()).rejects.toThrow('larger than the release manifest says')
    expect(readlinkSync(join(root, 'current'))).toBe(running)
  })

  it('does not install a release whose command does not start as the version it claims', async () => {
    const { install, root, running } = setup('0.1.9')
    await expect(install()).rejects.toThrow('did not start; the current install is unchanged')
    expect(readlinkSync(join(root, 'current'))).toBe(running)
    expect(readdirSync(join(root, 'versions'))).toHaveLength(1)
  })

  it('clears what an interrupted update left in staging', async () => {
    const { install, root } = setup()
    mkdirSync(join(root, '.staging/abandoned/unpacked'), { recursive: true })
    writeFileSync(join(root, '.staging/abandoned/partial.tar.gz'), 'half')
    await install()
    expect(existsSync(join(root, '.staging'))).toBe(false)
  })

  it('replaces a damaged copy of the same release rather than trusting it', async () => {
    const { install, root, manifest } = setup()
    mkdirSync(join(root, 'versions', target(manifest), 'apps/cli/lib'), { recursive: true })
    writeFileSync(join(root, 'versions', target(manifest), 'apps/cli/lib/bin.js'), 'process.exit(1)\n')
    await install()
    expect(readFileSync(join(root, 'versions', target(manifest), 'apps/cli/lib/bin.js'), 'utf8')).toContain('0.2.0')
  })

  it('reuses an intact copy of the release without downloading it again', async () => {
    const { install, host } = setup()
    await install()
    const downloads = host.requests.length
    await install()
    expect(host.requests).toHaveLength(downloads)
  })

  it('removes releases nobody started for a week, and keeps current, previous, and recently started ones', async () => {
    const { install, root, running } = setup()
    const now = Date.now()
    for (const [name, age] of [['0.0.8-aaaaaaaaaaaa', PRUNE_AFTER_MS + 1000], ['0.0.9-bbbbbbbbbbbb', 1000]] as const) {
      mkdirSync(join(root, 'versions', name))
      writeFileSync(join(root, 'versions', name, LAUNCH_MARKER), '')
      const then = new Date(now - age)
      utimesSync(join(root, 'versions', name, LAUNCH_MARKER), then, then)
    }
    // The running release is old too, but this process runs from it.
    const then = new Date(now - PRUNE_AFTER_MS * 2)
    utimesSync(running, then, then)
    const result = await install({ now: () => now })
    expect(result.pruned).toEqual(['0.0.8-aaaaaaaaaaaa'])
    expect(readdirSync(join(root, 'versions')).sort()).toEqual(['0.0.9-bbbbbbbbbbbb', running.split('/').at(-1), expect.stringMatching(/^0\.2\.0-/)].sort())
  })

  it('refuses a current that is not a link it manages', async () => {
    const { install, root, running } = setup()
    rmSync(join(root, 'current'))
    mkdirSync(join(root, 'current'))
    await expect(install()).rejects.toThrow('is not a Bake-managed link')
    expect(lstatSync(join(root, 'current')).isDirectory()).toBe(true)
    expect(existsSync(running)).toBe(true)
  })
})

describe('acquireLock', () => {
  it('refuses while a live process holds the lock, and takes over one whose holder exited or that is too old', async () => {
    const scratch = new Scratch()
    scratches.push(scratch)
    const lock = join(scratch.root, 'update.lock')
    const release = await acquireLock(scratch.root)
    // The holder is this very process, so it is alive.
    await expect(acquireLock(scratch.root)).rejects.toThrow('Another Bake update is running')
    await release()
    writeFileSync(lock, JSON.stringify({ pid: 2 ** 22 + 12345, startedAt: Date.now() }))
    await (await acquireLock(scratch.root))()
    writeFileSync(lock, JSON.stringify({ pid: process.pid, startedAt: Date.now() - 31 * 60 * 1000 }))
    await (await acquireLock(scratch.root))()
    writeFileSync(lock, 'garbage')
    await (await acquireLock(scratch.root))()
    expect(existsSync(lock)).toBe(false)
  })
})

describe('detectInstall', () => {
  it('leaves a source checkout or a hand-unpacked copy alone', () => {
    const scratch = new Scratch()
    scratches.push(scratch)
    mkdirSync(join(scratch.root, 'checkout'))
    mkdirSync(join(scratch.root, 'versions/not-a-release'), { recursive: true })
    expect(detectInstall(join(scratch.root, 'checkout')).kind).toBe('unmanaged')
    expect(detectInstall(join(scratch.root, 'versions/not-a-release')).kind).toBe('unmanaged')
  })
})

describe('the Windows pointer', () => {
  it('names the current release in a file replaced whole', async () => {
    const scratch = new Scratch()
    scratches.push(scratch)
    await pointAt(scratch.root, '0.1.0-aaaaaaaaaaaa', 'win32')
    await pointAt(scratch.root, '0.2.0-bbbbbbbbbbbb', 'win32')
    expect(currentOf(scratch.root)).toBe('0.2.0-bbbbbbbbbbbb')
    expect(readdirSync(scratch.root)).toEqual(['current.txt'])
  })

  it('launches through the pointer, so an update never rewrites the running batch file', () => {
    const text = windowsLauncher('C:\\Users\\100% dev\\AppData\\Local\\Bake')
    expect(text.split('\r\n').every(line => !line.includes('\n'))).toBe(true)
    expect(text).toContain('set "BAKE_RELEASE_ROOT=C:\\Users\\100%% dev\\AppData\\Local\\Bake"')
    expect(text).toContain('set /p BAKE_CURRENT=<"%BAKE_RELEASE_ROOT%\\current.txt"')
    expect(text).toContain('if /I "%~1"=="update" goto raw')
    expect(text).not.toMatch(/versions\\\d/)
  })
})
