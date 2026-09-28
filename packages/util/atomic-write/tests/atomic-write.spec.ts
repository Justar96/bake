import { spawn } from 'node:child_process'
import type { ChildProcessByStdio } from 'node:child_process'
import { once } from 'node:events'
import { lstat, mkdtemp, readFile, readdir, rm, stat, symlink, utimes, writeFile } from 'node:fs/promises'
import { hostname, tmpdir } from 'node:os'
import { dirname, join } from 'node:path'
import type { Readable, Writable } from 'node:stream'
import { fileURLToPath } from 'node:url'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { withFileLock, writeFileAtomic } from '../src/index.ts'

const state = vi.hoisted(() => ({
  flockBusy: 0,
  flockGate: undefined as { reached: () => void; release: Promise<unknown> } | undefined,
  flockUnavailable: false,
  lockPermissionFailures: 0,
  releaseLockBeforeProbe: false,
  renameAttempts: 0,
  renameFailures: [] as string[],
}))

vi.mock('@deepseek-ai/node-addon-system/flock', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@deepseek-ai/node-addon-system/flock')>()
  return {
    ...actual,
    tryLockExclusive: async (fd: number) => {
      if (state.flockUnavailable) {
        throw Object.assign(new Error('flock is not supported on this host'), { code: 'ERR_FLOCK_UNSUPPORTED_PLATFORM' })
      }
      if (state.flockBusy > 0) {
        state.flockBusy -= 1
        throw Object.assign(new Error('EAGAIN: injected flock contention'), { code: 'EAGAIN' })
      }
      const gate = state.flockGate
      state.flockGate = undefined
      if (gate !== undefined) {
        gate.reached()
        await gate.release
      }
      return actual.tryLockExclusive(fd)
    },
  }
})

vi.mock('node:fs/promises', async (importOriginal) => {
  const actual = await importOriginal<typeof import('node:fs/promises')>()
  return {
    ...actual,
    open: (async (...args: Parameters<typeof actual.open>) => {
      const [path, flags] = args
      if (state.lockPermissionFailures > 0 && String(path).endsWith('.lock') && flags === 'wx') {
        state.lockPermissionFailures -= 1
        if (state.releaseLockBeforeProbe) await actual.rm(String(path))
        throw Object.assign(new Error('EPERM: injected exclusive-create failure'), { code: 'EPERM' })
      }
      return actual.open(...args)
    }),
    rename: (async (...args: Parameters<typeof actual.rename>) => {
      state.renameAttempts += 1
      const code = state.renameFailures.shift()
      if (code !== undefined) {
        if (code === 'NO_CODE') throw new Error('injected rename failure without a code')
        throw Object.assign(new Error(`${code}: injected rename failure`), { code })
      }
      return actual.rename(...args)
    }),
  }
})

const scratchDirs: string[] = []

/** A lock-holding child process and its exit, which teardown awaits. */
interface Holder {
  child: ChildProcessByStdio<Writable, Readable, null>
  exited: Promise<unknown>
}

const holders: Holder[] = []
const REPO_ROOT = fileURLToPath(new URL('../../../../', import.meta.url))
const HOLDER_SCRIPT = fileURLToPath(new URL('./fixtures/lock-holder.ts', import.meta.url))
const TSX_LOADER = import.meta.resolve('tsx/esm')

afterEach(async () => {
  vi.useRealTimers()
  vi.restoreAllMocks()
  await Promise.all(holders.splice(0).map(async ({ child, exited }) => {
    if (child.exitCode === null && child.signalCode === null) child.kill('SIGKILL')
    await exited
  }))
  state.flockBusy = 0
  state.flockGate = undefined
  state.flockUnavailable = false
  state.lockPermissionFailures = 0
  state.releaseLockBeforeProbe = false
  state.renameAttempts = 0
  state.renameFailures.length = 0
  await Promise.all(scratchDirs.splice(0).map(dir => rm(dir, {
    force: true,
    maxRetries: 10,
    recursive: true,
    retryDelay: 20,
  })))
})

async function scratch(): Promise<string> {
  const dir = await mkdtemp(join(tmpdir(), 'dsh-atomic-write-'))
  scratchDirs.push(dir)
  return dir
}

/** Resolve once the lockfile exists, so contention is measured against a held lock. */
async function waitForLock(lockPath: string): Promise<void> {
  for (;;) {
    try {
      await stat(lockPath)
      return
    } catch {
      await new Promise(resolve => setTimeout(resolve, 5))
    }
  }
}

/**
 * Start a child process that holds the writer lock for `target` through the
 * named protocol, resolving once it reports holding. Closing its stdin makes
 * it release and exit; teardown kills any holder still running.
 */
async function spawnHolder(target: string, protocol: 'current' | 'created'): Promise<Holder> {
  const child = spawn(process.execPath, ['--import', TSX_LOADER, HOLDER_SCRIPT, target, protocol], {
    cwd: REPO_ROOT,
    env: { ...process.env, TSX_TSCONFIG_PATH: join(REPO_ROOT, 'tsconfig.json') },
    stdio: ['pipe', 'pipe', 'inherit'],
  })
  const holder: Holder = { child, exited: once(child, 'exit') }
  holders.push(holder)
  let output = ''
  await new Promise<void>((resolve, reject) => {
    child.stdout.setEncoding('utf8').on('data', (chunk: string) => {
      output += chunk
      if (output.includes('holding\n')) resolve()
    })
    child.once('exit', (code, signal) => {
      reject(new Error(`lock holder exited before holding the lock (${signal ?? code})`))
    })
    child.once('error', reject)
  })
  return holder
}

/** Kill a holder without letting it release, as a crash would. */
async function crash(holder: Holder): Promise<void> {
  holder.child.kill('SIGKILL')
  await holder.exited
}

/**
 * Hold the next `flock` call until the returned release runs, resolving
 * `reached` once a creator is waiting between exclusive create and lock.
 */
function gateNextFlock(): { reached: Promise<unknown>; release: () => void } {
  const reached = Promise.withResolvers<undefined>()
  const release = Promise.withResolvers<undefined>()
  state.flockGate = { reached: () => { reached.resolve(undefined) }, release: release.promise }
  return { reached: reached.promise, release: () => { release.resolve(undefined) } }
}

/** The PID of a process that has already exited and been reaped. */
async function exitedPid(): Promise<number> {
  const child = spawn(process.execPath, ['-e', ''], { stdio: 'ignore' })
  await once(child, 'exit')
  return child.pid!
}

/** Names left in `dir` other than the target document and its lock. */
async function strayEntries(dir: string): Promise<string[]> {
  return (await readdir(dir)).filter(name => name !== 'document' && name !== 'document.lock')
}

describe('writeFileAtomic', () => {
  it('creates the file and its parents with exactly the stated mode', async () => {
    const dir = await scratch()
    const target = join(dir, 'nested', 'deep', 'doc.yaml')
    await writeFileAtomic(target, 'a: 1\n', { dirMode: 0o700, mode: 0o600 })
    expect(await readFile(target, 'utf8')).toBe('a: 1\n')
    if (process.platform !== 'win32') {
      expect((await stat(dirname(target))).mode & 0o777).toBe(0o700)
      expect((await stat(target)).mode & 0o777).toBe(0o600)
    }
  })

  it('replaces existing content and narrows a wider-permission file to the stated mode', async () => {
    const dir = await scratch()
    const target = join(dir, 'doc.yaml')
    await writeFile(target, 'old', { mode: 0o644 })
    await writeFileAtomic(target, 'new', { mode: 0o600 })
    expect(await readFile(target, 'utf8')).toBe('new')
    if (process.platform !== 'win32') expect((await stat(target)).mode & 0o777).toBe(0o600)
  })

  it('replaces a symlinked target itself without writing through to the referent', async () => {
    const dir = await scratch()
    const victim = join(dir, 'victim')
    await writeFile(victim, 'victim-content')
    const target = join(dir, 'doc.yaml')
    await symlink(victim, target)
    await writeFileAtomic(target, 'replaced', { mode: 0o600 })
    expect((await lstat(target)).isSymbolicLink()).toBe(false)
    expect(await readFile(target, 'utf8')).toBe('replaced')
    expect(await readFile(victim, 'utf8')).toBe('victim-content')
  })

  it('retries transient Windows rename interference and commits the replacement', async () => {
    vi.spyOn(process, 'platform', 'get').mockReturnValue('win32')
    vi.useFakeTimers()
    const dir = await scratch()
    const target = join(dir, 'document')
    await writeFile(target, 'old')
    state.renameFailures.push('EACCES', 'EBUSY', 'EPERM')

    const replacement = writeFileAtomic(target, 'new', { mode: 0o600 })
    await vi.waitFor(() => { expect(state.renameAttempts).toBeGreaterThan(0) })
    await vi.runAllTimersAsync()
    await replacement

    expect(state.renameAttempts).toBe(4)
    expect(await readFile(target, 'utf8')).toBe('new')
    expect((await readdir(dir)).filter(entry => entry.includes('.tmp'))).toEqual([])
  })

  it('leaves no temp sibling after bounded Windows rename retries expire', async () => {
    vi.spyOn(process, 'platform', 'get').mockReturnValue('win32')
    vi.useFakeTimers()
    const dir = await scratch()
    const target = join(dir, 'document')
    await writeFile(target, 'old')
    state.renameFailures.push(...Array.from({ length: 9 }, () => 'EPERM'))

    const replacement = writeFileAtomic(target, 'new', { mode: 0o600 })
    await vi.waitFor(() => { expect(state.renameAttempts).toBeGreaterThan(0) })
    await vi.runAllTimersAsync()
    await expect(replacement).rejects.toMatchObject({ code: 'EPERM' })

    expect(state.renameAttempts).toBe(9)
    expect(await readFile(target, 'utf8')).toBe('old')
    expect((await readdir(dir)).filter(entry => entry.includes('.tmp'))).toEqual([])
  })

  it('does not retry a Windows rename failure without a transient code', async () => {
    vi.spyOn(process, 'platform', 'get').mockReturnValue('win32')
    const dir = await scratch()
    const target = join(dir, 'document')
    state.renameFailures.push('NO_CODE')

    await expect(writeFileAtomic(target, 'new', { mode: 0o600 })).rejects.toThrow(/without a code/)
    expect(state.renameAttempts).toBe(1)
    expect((await readdir(dir)).filter(entry => entry.includes('.tmp'))).toEqual([])
  })

  it('does not retry rename permission failures outside Windows', async () => {
    vi.spyOn(process, 'platform', 'get').mockReturnValue('linux')
    const dir = await scratch()
    const target = join(dir, 'document')
    state.renameFailures.push('EPERM')

    await expect(writeFileAtomic(target, 'new', { mode: 0o600 })).rejects.toMatchObject({ code: 'EPERM' })
    expect(state.renameAttempts).toBe(1)
  })
})

describe('withFileLock', () => {
  it.each(['kernel-held', 'PID-only'] as const)('rejects an invalid parent hierarchy for a %s lock', async (form) => {
    state.flockUnavailable = form === 'PID-only'
    const dir = await scratch()
    const parent = join(dir, 'not-a-directory')
    await writeFile(parent, 'occupied')
    let called = false

    await expect(withFileLock(join(parent, 'document'), async () => {
      called = true
    })).rejects.toThrow(/ENOENT|ENOTDIR|not a directory/i)
    expect(called).toBe(false)
  })

  it('waits for the caller-stated limit rather than the protocol default', async () => {
    // An operation whose work includes a network round trip legitimately holds
    // the lock far longer than the render-and-rename the default was sized
    // for. The limit is per call so one such operation cannot fail every other
    // writer of the same file, and a caller that states a short one still
    // fails fast.
    const dir = await scratch()
    const target = join(dir, 'document')
    let release = (): void => {}
    const held = new Promise<void>((resolve) => { release = resolve })
    const holder = withFileLock(target, () => held)
    // The holder owns the lock once its lockfile exists; contending before
    // that would measure nothing.
    await waitForLock(`${target}.lock`)

    // Elapsed time is the assertion that distinguishes a honoured limit from
    // the ignored argument: without it the contender simply waits out the
    // protocol default and fails with the same message.
    const startedAt = Date.now()
    await expect(withFileLock(target, async () => 'impatient', { waitMs: 50 }))
      .rejects.toThrow(/timed out waiting for the writer lock/)
    expect(Date.now() - startedAt).toBeLessThan(1_000)

    const patient = withFileLock(target, async () => 'patient', { waitMs: 10_000 })
    release()
    await holder
    expect(await patient).toBe('patient')
  })

  it('fails a contender after the two-second default with the unchanged timeout error', async () => {
    const dir = await scratch()
    const target = join(dir, 'document')
    let release = (): void => {}
    const held = new Promise<void>((resolve) => { release = resolve })
    const holder = withFileLock(target, () => held)
    await waitForLock(`${target}.lock`)

    const startedAt = Date.now()
    const failure = await withFileLock(target, async () => 'impatient').then(() => undefined, (error: unknown) => error)
    // Only a lower bound: a loaded machine can make the contender later, never earlier.
    expect(Date.now() - startedAt).toBeGreaterThanOrEqual(2_000)
    expect(failure).toBeInstanceOf(Error)
    expect(failure).not.toHaveProperty('code')
    expect((failure as Error).message).toBe(`atomic-write: timed out waiting for the writer lock at ${target}.lock`)
    release()
    await holder
  })
})

describe('withFileLock created-file protocol', () => {
  it('retries EPERM only when the lock path currently exists', async () => {
    state.flockUnavailable = true
    const dir = await scratch()
    const target = join(dir, 'document')
    const lockPath = `${target}.lock`
    await writeFile(lockPath, 'holder\n')
    const release = setTimeout(() => { void rm(lockPath, { force: true }) }, 50)
    state.lockPermissionFailures = 1
    let called = false

    try {
      await withFileLock(target, async () => { called = true })
    } finally {
      clearTimeout(release)
    }
    expect(called).toBe(true)
    expect(state.lockPermissionFailures).toBe(0)
  })

  it.each(['win32', 'linux'] as const)('preserves persistent EPERM on %s when no lock path exists', async (platform) => {
    vi.spyOn(process, 'platform', 'get').mockReturnValue(platform)
    state.flockUnavailable = true
    const dir = await scratch()
    const operation = vi.fn(async () => {})
    state.lockPermissionFailures = 2

    await expect(withFileLock(join(dir, 'document'), operation)).rejects.toMatchObject({ code: 'EPERM' })
    expect(operation).not.toHaveBeenCalled()
  })

  it('acquires the Windows lock when its holder releases before the contention probe', async () => {
    vi.spyOn(process, 'platform', 'get').mockReturnValue('win32')
    const dir = await scratch()
    const target = join(dir, 'document')
    await writeFile(`${target}.lock`, 'holder\n')
    state.lockPermissionFailures = 1
    state.releaseLockBeforeProbe = true
    const operation = vi.fn(async () => 'acquired')

    await expect(withFileLock(target, operation)).resolves.toBe('acquired')
    expect(operation).toHaveBeenCalledOnce()
    expect(await readdir(dir)).toEqual([])
  })

  it('keeps the PID record when the native binding is unavailable', async () => {
    state.flockUnavailable = true
    const dir = await scratch()
    const target = join(dir, 'document')

    const record = await withFileLock(target, () => readFile(`${target}.lock`, 'utf8'))
    expect(record).toBe(`${process.pid}\n`)
    expect(await readdir(dir)).toEqual([])
  })

  it('never removes a stale lock, as earlier releases did not', async () => {
    state.flockUnavailable = true
    const dir = await scratch()
    const target = join(dir, 'document')
    const lockPath = `${target}.lock`
    await writeFile(lockPath, `${await exitedPid()}\n`)
    const operation = vi.fn(async () => {})

    await expect(withFileLock(target, operation, { waitMs: 50 })).rejects.toThrow(/timed out waiting for the writer lock/)
    expect(operation).not.toHaveBeenCalled()
    expect(await readdir(dir)).toEqual(['document.lock'])
  })
})

describe.skipIf(process.platform === 'win32')('withFileLock kernel-held protocol', () => {
  it('holds a kernel-held record that an earlier release cannot displace', async () => {
    const dir = await scratch()
    const target = join(dir, 'document')
    const lockPath = `${target}.lock`

    await withFileLock(target, async () => {
      expect(JSON.parse(await readFile(lockPath, 'utf8'))).toEqual({ flock: true, pid: process.pid, host: hostname() })
      // A release before the kernel-held protocol takes the lock by exclusive create only.
      await expect(writeFile(lockPath, `${process.pid}\n`, { flag: 'wx' })).rejects.toMatchObject({ code: 'EEXIST' })
      expect(await strayEntries(dir)).toEqual([])
    })
    expect(await readdir(dir)).toEqual([])
  })

  it('excludes a contender while another process holds the lock, then admits it on release', async () => {
    const dir = await scratch()
    const target = join(dir, 'document')
    const holder = await spawnHolder(target, 'current')
    const operation = vi.fn(async () => 'acquired')

    await expect(withFileLock(target, operation, { waitMs: 200 })).rejects.toThrow(/timed out waiting for the writer lock/)
    expect(operation).not.toHaveBeenCalled()
    expect(await strayEntries(dir)).toEqual([])

    holder.child.stdin.end()
    expect(await holder.exited).toEqual([0, null])
    await expect(withFileLock(target, operation, { waitMs: 0 })).resolves.toBe('acquired')
  })

  it('recovers at once from a holder process killed while holding the lock', async () => {
    const dir = await scratch()
    const target = join(dir, 'document')
    const lockPath = `${target}.lock`
    const holder = await spawnHolder(target, 'current')

    await crash(holder)
    // The killed holder never released: its record is still at the lock path.
    expect(JSON.parse(await readFile(lockPath, 'utf8'))).toMatchObject({ flock: true, pid: holder.child.pid })
    // No wait at all: the kernel dropped the dead holder's flock, which proves it gone.
    await expect(withFileLock(target, async () => 'recovered', { waitMs: 0 })).resolves.toBe('recovered')
    expect(await readdir(dir)).toEqual([])
  })

  it('respects a live created-file holder and recovers its lock once that process is killed', async () => {
    const dir = await scratch()
    const target = join(dir, 'document')
    const lockPath = `${target}.lock`
    const holder = await spawnHolder(target, 'created')
    const operation = vi.fn(async () => 'recovered')

    await expect(withFileLock(target, operation, { waitMs: 200 })).rejects.toThrow(/timed out waiting for the writer lock/)
    expect(operation).not.toHaveBeenCalled()
    expect(await readFile(lockPath, 'utf8')).toBe(`${holder.child.pid}\n`)

    await crash(holder)
    await expect(withFileLock(target, operation, { waitMs: 0 })).resolves.toBe('recovered')
    expect(await readdir(dir)).toEqual([])
  })

  it('keeps a lock whose live creator has not locked it yet', async () => {
    const dir = await scratch()
    const target = join(dir, 'document')
    const lockPath = `${target}.lock`
    const gate = gateNextFlock()
    const holder = withFileLock(target, () => readFile(lockPath, 'utf8'))
    await gate.reached

    // The unlocked file carries the creator's PID record, which a contender judges by PID.
    expect(await readFile(lockPath, 'utf8')).toBe(`${process.pid}\n`)
    await expect(withFileLock(target, async () => {}, { waitMs: 50 })).rejects.toThrow(/timed out waiting for the writer lock/)
    gate.release()
    expect(JSON.parse(await holder)).toEqual({ flock: true, pid: process.pid, host: hostname() })
  })

  it('retries its own flock while a contender inspects the file it created', async () => {
    const dir = await scratch()
    const target = join(dir, 'document')
    state.flockBusy = 2

    const record = await withFileLock(target, () => readFile(`${target}.lock`, 'utf8'))
    expect(JSON.parse(record)).toMatchObject({ flock: true, pid: process.pid })
    expect(state.flockBusy).toBe(0)
  })

  it('acquires again when its created file was removed before it was locked', async () => {
    const dir = await scratch()
    const target = join(dir, 'document')
    const lockPath = `${target}.lock`
    const gate = gateNextFlock()
    const holder = withFileLock(target, () => readFile(lockPath, 'utf8'))
    await gate.reached

    // A contender in another PID namespace could misjudge the PID record and remove it.
    await rm(lockPath)
    gate.release()
    expect(JSON.parse(await holder)).toMatchObject({ flock: true, pid: process.pid })
    expect(await readdir(dir)).toEqual([])
  })

  it('recovers a same-host kernel-held record by its free flock, not its PID', async () => {
    const dir = await scratch()
    const target = join(dir, 'document')
    // This process is alive, but no descriptor holds the record's flock.
    await writeFile(`${target}.lock`, `${JSON.stringify({ flock: true, pid: process.pid, host: hostname() })}\n`)

    await expect(withFileLock(target, async () => 'recovered', { waitMs: 0 })).resolves.toBe('recovered')
  })

  it.each([
    ['a record from another host', JSON.stringify({ flock: true, pid: 1, host: `not-${hostname()}` })],
    ['a PID record whose process lives', `${process.pid}\n`],
    ['an empty file', ''],
    ['an unterminated PID record', '4194305'],
    ['content naming no owner', 'holder\n'],
  ])('never removes %s, however old', async (_kind, content) => {
    const dir = await scratch()
    const target = join(dir, 'document')
    const lockPath = `${target}.lock`
    await writeFile(lockPath, content)
    const past = (Date.now() - 3_600_000) / 1000
    await utimes(lockPath, past, past)
    const operation = vi.fn(async () => {})

    await expect(withFileLock(target, operation, { waitMs: 50 })).rejects.toThrow(/timed out waiting for the writer lock/)
    expect(operation).not.toHaveBeenCalled()
    expect(await readFile(lockPath, 'utf8')).toBe(content)
    expect(await strayEntries(dir)).toEqual([])
  })

  it('never follows a symlink at the lock path', async () => {
    const dir = await scratch()
    const target = join(dir, 'document')
    const referent = join(dir, 'referent')
    await writeFile(referent, `${JSON.stringify({ flock: true, pid: process.pid, host: hostname() })}\n`)
    await symlink(referent, `${target}.lock`)

    await expect(withFileLock(target, async () => {}, { waitMs: 50 })).rejects.toThrow(/timed out waiting for the writer lock/)
    expect((await lstat(`${target}.lock`)).isSymbolicLink()).toBe(true)
  })

  it('leaves a lock another writer published after this holder lost its path', async () => {
    const dir = await scratch()
    const target = join(dir, 'document')
    const lockPath = `${target}.lock`

    await withFileLock(target, async () => {
      await rm(lockPath)
      await writeFile(lockPath, 'successor\n')
    })
    expect(await readFile(lockPath, 'utf8')).toBe('successor\n')
  })
})
