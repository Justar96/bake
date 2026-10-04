/** A rollback returns `current` to the newest earlier release installed, after that release's check, or changes nothing. */
import { existsSync, mkdirSync, readdirSync, readFileSync, readlinkSync, rmSync, symlinkSync, utimesSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'
import { afterEach, describe, expect, it } from 'vitest'
import {
  acquireLock, currentOf, detectInstall, LAUNCH_MARKER, rollbackRelease, UpdateError, type InstallProgress, type ManagedInstall,
} from '../src/index.ts'
import { releaseTree, Scratch, selfCheckingCommand } from './fixture.ts'

const scratches: Scratch[] = []
afterEach(() => { for (const scratch of scratches.splice(0)) scratch.dispose() })

/** A command that prints `version` for whichever check runs. */
const printing = (version: string): string => `console.log(${JSON.stringify(version)})\n`

/**
 * An install root holding `releases`, each a version directory and its command,
 * or null for a directory without one, with `current` naming `current`.
 */
function install(releases: Record<string, string | null>, current: string, platform: 'linux' | 'win32' = 'linux') {
  const scratch = new Scratch()
  scratches.push(scratch)
  const root = join(scratch.root, 'install')
  for (const [name, command] of Object.entries(releases)) {
    if (command === null) mkdirSync(join(root, 'versions', name), { recursive: true })
    else releaseTree(join(root, 'versions', name), command)
  }
  if (platform === 'win32') writeFileSync(join(root, 'current.txt'), `${current}\r\n`)
  else symlinkSync(join(root, 'versions', current), join(root, 'current'))
  const layout = detectInstall(join(root, 'versions', current)) as ManagedInstall
  const events: string[] = []
  const rollback = () => rollbackRelease({
    layout, platform,
    onFound: (from, to) => events.push(`found ${from} -> ${to}`),
    onProgress: (progress: InstallProgress) => events.push(progress.phase),
  })
  return { root, layout, rollback, events }
}

const A = '0.1.0-aaaaaaaaaaaa'
const B = '0.1.5-bbbbbbbbbbbb'
const C = '0.1.9-cccccccccccc'
const D = '0.2.0-dddddddddddd'
const E = '0.2.1-eeeeeeeeeeee'

describe.skipIf(process.platform === 'win32')('rollbackRelease', () => {
  it('returns current to the newest earlier release, after its check, and removes nothing', async () => {
    // C has no command, and E is newer than current: neither is a target.
    const { root, rollback, events } = install({ [A]: printing('0.1.0'), [B]: printing('0.1.5'), [C]: null, [D]: printing('0.2.0'), [E]: printing('0.2.1') }, D)
    await expect(rollback()).resolves.toEqual({ kind: 'rolled-back', from: D, to: B })
    expect(readlinkSync(join(root, 'current'))).toBe(join(root, 'versions', B))
    expect(events).toEqual([`found ${D} -> ${B}`, 'verify'])
    expect(readdirSync(join(root, 'versions')).sort()).toEqual([A, B, C, D, E])
    expect(readdirSync(root).sort()).toEqual(['current', 'versions'])
  })

  it('goes one release further back each time, then says nothing is left and leaves current alone', async () => {
    const { root, rollback } = install({ [A]: printing('0.1.0'), [B]: printing('0.1.5'), [D]: printing('0.2.0') }, D)
    await expect(rollback()).resolves.toMatchObject({ to: B })
    await expect(rollback()).resolves.toEqual({ kind: 'rolled-back', from: B, to: A })
    await expect(rollback()).resolves.toEqual({ kind: 'none', current: A })
    expect(readlinkSync(join(root, 'current'))).toBe(join(root, 'versions', A))
    expect(existsSync(join(root, 'update.lock'))).toBe(false)
  })

  it('returns to the copy of a version started most recently', async () => {
    const older = '0.1.0-111111111111'
    const newer = '0.1.0-222222222222'
    const { root, rollback } = install({ [older]: printing('0.1.0'), [newer]: printing('0.1.0'), [D]: printing('0.2.0') }, D)
    const now = Date.now()
    for (const [name, age] of [[older, 1000], [newer, 10]] as const) {
      writeFileSync(join(root, 'versions', name, LAUNCH_MARKER), '')
      utimesSync(join(root, 'versions', name, LAUNCH_MARKER), new Date(now - age * 1000), new Date(now - age * 1000))
    }
    await expect(rollback()).resolves.toMatchObject({ to: newer })
  })

  it('leaves current where it was when the earlier release fails its check, and says why', async () => {
    const { root, rollback } = install({ [A]: 'console.error(\'Error: boom\'); process.exitCode = 1\n', [D]: printing('0.2.0') }, D)
    const failure = rollback()
    await expect(failure).rejects.toBeInstanceOf(UpdateError)
    await expect(failure).rejects.toThrow('The earlier Bake 0.1.0 did not start; the current install is unchanged: Error: boom')
    expect(readlinkSync(join(root, 'current'))).toBe(join(root, 'versions', D))
    expect(existsSync(join(root, 'update.lock'))).toBe(false)
  })

  it('runs --self-check on an earlier release that has it', async () => {
    const { rollback } = install({ '0.4.0-aaaaaaaaaaaa': selfCheckingCommand('0.4.0'), '0.5.0-bbbbbbbbbbbb': printing('0.5.0') }, '0.5.0-bbbbbbbbbbbb')
    await expect(rollback()).resolves.toMatchObject({ kind: 'rolled-back', to: '0.4.0-aaaaaaaaaaaa' })
  })

  it('refuses while another update holds the install lock', async () => {
    const { root, rollback } = install({ [A]: printing('0.1.0'), [D]: printing('0.2.0') }, D)
    const release = await acquireLock(root)
    try {
      await expect(rollback()).rejects.toThrow('Another Bake update is running')
      expect(readlinkSync(join(root, 'current'))).toBe(join(root, 'versions', D))
    } finally {
      await release()
    }
  })

  it('refuses an install whose current names no release', async () => {
    const { root, rollback } = install({ [A]: printing('0.1.0'), [D]: printing('0.2.0') }, D)
    rmSync(join(root, 'current'))
    await expect(rollback()).rejects.toThrow('names no current release; run the installer again')
  })

  it('leaves a release the updater does not manage alone', async () => {
    await expect(rollbackRelease({ layout: { kind: 'unmanaged', running: '/src/bake' } }))
      .resolves.toEqual({ kind: 'unmanaged', running: '/src/bake' })
  })
})

describe('rollbackRelease on Windows', () => {
  it.skipIf(process.platform === 'win32')('replaces the pointer file whole', async () => {
    const { root, rollback } = install({ [A]: printing('0.1.0'), [D]: printing('0.2.0') }, D, 'win32')
    await expect(rollback()).resolves.toEqual({ kind: 'rolled-back', from: D, to: A })
    expect(currentOf(root)).toBe(A)
    expect(readFileSync(join(root, 'current.txt'), 'utf8')).toBe(`${A}\r\n`)
    expect(readdirSync(root).sort()).toEqual(['current.txt', 'versions'])
  })
})
