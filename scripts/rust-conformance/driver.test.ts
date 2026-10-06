import { afterEach, beforeEach, describe, expect, test } from 'bun:test'
import { mkdir, mkdtemp, readdir, readFile, readlink, realpath, rm, stat, symlink, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join, sep } from 'node:path'
import {
  ConformanceSetupError, launch, listFixtures, MAX_TIMEOUT_MS, MAX_WORKSPACE_ENTRIES, runConformance, snapshotWorkspace, summarize,
  typescriptArm, type ArmSpec, type ConformanceReport, type RunOptions,
} from './driver.ts'
import { MAX_TEXT_BYTES } from './fixture.ts'

const DRIVER = join(import.meta.dirname, 'driver.ts')
const FAULT_ARM = join(import.meta.dirname, 'test-fault-arm.ts')
const FIXTURES = join(import.meta.dirname, '..', '..', 'conformance', 'fixtures')
const ALLOW = join(FIXTURES, 'allow-write.json')
const DENY = join(FIXTURES, 'deny-write.json')
const REPOSITORY = join(import.meta.dirname, '..', '..')
const posix = process.platform !== 'win32'
// Permission bits do not stop root from reading or executing a file.
const permissionsApply = posix && process.getuid?.() !== 0
// Process-bound cases take the scripts-unit lane budget.
const BUDGET = 30_000

let root: string
let tempRoot: string
beforeEach(async () => {
  root = await mkdtemp(join(tmpdir(), 'bake-conformance-driver-'))
  tempRoot = join(root, 'runs')
  await mkdir(tempRoot)
}, BUDGET)
afterEach(async () => {
  await rm(root, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 })
}, BUDGET)

const fault = (name: string, ...extra: string[]): ArmSpec =>
  ({ name: `fault-${name}`, argv: [process.execPath, FAULT_ARM, name, ...extra] })

const run = (arms: ArmSpec[], fixtures = [ALLOW], extra: Partial<RunOptions> = {}) =>
  runConformance({ fixtures, arms, tempRoot, ...extra })

/** Comparator outcomes as `name: outcome` strings for one arm against expected. */
const versusExpected = (report: ConformanceReport, arm: string, fixture = 0): string[] =>
  report.fixtures[fixture]!.arms.find(entry => entry.arm === arm)!.expected.map(entry => `${entry.comparator}: ${entry.outcome}`)
const versusPair = (report: ConformanceReport, fixture = 0): string[] =>
  report.fixtures[fixture]!.pairs[0]!.comparisons.map(entry => `${entry.comparator}: ${entry.outcome}`)

const isAlive = (pid: number): boolean => {
  try {
    process.kill(pid, 0)
    return true
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === 'ESRCH') return false
    throw error
  }
}

async function waitForFile(path: string, pending: Promise<unknown>): Promise<string> {
  const controller = new AbortController()
  const polling = (async () => {
    while (!controller.signal.aborted) {
      const text = await readFile(path, 'utf8').catch(() => '')
      if (text !== '') return text
      await new Promise(resolve => setTimeout(resolve, 20))
    }
    return ''
  })()
  try {
    return await Promise.race([polling, pending.then(() => { throw new Error('child completed before writing its marker') })])
  } finally {
    controller.abort()
    await polling
  }
}

describe('runConformance', () => {
  test('the Bun runner passes every shared fixture against itself and records what ran', async () => {
    const reportPath = join(root, 'report', 'report.json')
    const fixtures = await listFixtures(FIXTURES)
    expect(fixtures).toContain(ALLOW)
    expect(fixtures).toContain(DENY)
    const { report, diagnostics } = await run([typescriptArm(), { ...typescriptArm(), name: 'again' }], fixtures, { reportPath })
    expect(summarize(report)).toEqual(report.fixtures.map(fixture => `pass ${fixture.id}`))
    expect(report.ok).toBe(true)
    expect(report.fixtures.map(fixture => fixture.path))
      .toEqual(fixtures.map(path => `conformance/fixtures/${path.split(/[\\/]/).at(-1)}`))
    expect(report.fixtures[0]!.sha256).toMatch(/^[0-9a-f]{64}$/)
    expect(report.arms[0]!.command).toEqual(['<external>/' + process.execPath.split(/[\\/]/).at(-1), 'scripts/rust-conformance/runner.ts'])
    expect(report.arms[0]!.artifacts.map(artifact => artifact.path)).toEqual(['scripts/rust-conformance/runner.ts', 'scripts/rust-conformance/fixture.ts'])
    expect(diagnostics.every(entry => entry.stderr === '')).toBe(true)
    const written = await readFile(reportPath, 'utf8')
    expect(JSON.parse(written)).toEqual(JSON.parse(JSON.stringify(report)))
    // Sanitized: no private roots, home, or prompt text.
    expect(written).not.toContain(root)
    expect(written).not.toContain(tmpdir())
    expect(written).not.toContain('trailing space')
    expect(await readdir(tempRoot)).toEqual([])
    expect(await readdir(join(root, 'report'))).toEqual(['report.json'])
  }, BUDGET)

  test('a failed setup removes earlier success and diagnostic files', async () => {
    const reportPath = join(root, 'report.json')
    const diagnosticsPath = join(root, 'diagnostics.json')
    await writeFile(reportPath, '{"ok":true}\n')
    await writeFile(diagnosticsPath, '[]\n')
    await expect(run([typescriptArm()], [], { reportPath, diagnosticsPath })).rejects.toThrow('no fixtures to run')
    expect(await stat(reportPath).catch(() => null)).toBeNull()
    expect(await stat(diagnosticsPath).catch(() => null)).toBeNull()
  })

  test('children get a minimal environment with private homes inside a removed root', async () => {
    const marker = join(root, 'env.json')
    let parent = tempRoot
    if (posix) {
      parent = join(root, 'linked-runs')
      await symlink(tempRoot, parent, 'dir')
    }
    const { report } = await run([fault('env', marker)], [ALLOW], { tempRoot: parent })
    expect(report.ok).toBe(true)
    const { env, cwd } = JSON.parse(await readFile(marker, 'utf8')) as { env: Record<string, string>; cwd: string }
    const allowed = ['PATH', 'HOME', 'BAKE_HOME', 'DSH_HOME', 'TMPDIR', 'TMP', 'TEMP', 'USERPROFILE', 'SystemRoot', 'windir', 'ComSpec', 'PATHEXT']
    expect(Object.keys(env).filter(key => !allowed.some(name => name.toLowerCase() === key.toLowerCase()))).toEqual([])
    expect(env.BAKE_HOME!.startsWith(`${parent}${sep}`)).toBe(true)
    expect(env.DSH_HOME).toBe(env.BAKE_HOME)
    // cwd resolves directory aliases, including macOS's /var -> /private/var.
    expect(cwd.startsWith(`${await realpath(parent)}${sep}`)).toBe(true)
    expect(await stat(cwd).catch(() => null)).toBeNull()
  }, BUDGET)

  // Each negative control fails exactly its named comparator, both against
  // expected values and against the real Bun runner.
  test.each([
    ['prompt-byte', 'prompt-bytes'],
    ['swap-events', 'event-order'],
    ['permission-outcome', 'permissions'],
    ['file-bytes', 'final-files'],
  ])('negative control %s fails only %s', async (name, comparator) => {
    const { report } = await run([typescriptArm(), fault(name)])
    const expected = ['prompt-bytes', 'event-order', 'permissions', 'final-files']
      .map(entry => `${entry}: ${entry === comparator ? 'fail' : 'pass'}`)
    expect(versusExpected(report, 'typescript')).toEqual(expected.map(entry => entry.replace('fail', 'pass')))
    expect(versusExpected(report, `fault-${name}`)).toEqual(expected)
    expect(versusPair(report)).toEqual(expected)
    const faulty = report.fixtures[0]!.arms[1]!
    expect({ exitCode: faulty.exitCode, observationError: faulty.observationError, protectedChanged: faulty.protectedChanged })
      .toEqual({ exitCode: 0, observationError: undefined, protectedChanged: [] })
    expect(report.ok).toBe(false)
  }, BUDGET)

  test('a tampered protected file fails the arm and the final files', async () => {
    const { report } = await run([fault('protected')], [DENY])
    const arm = report.fixtures[0]!.arms[0]!
    expect({ check: arm.protectedCheck, changed: arm.protectedChanged }).toEqual({ check: 'changed', changed: ['check.txt'] })
    expect(versusExpected(report, 'fault-protected')).toEqual(['prompt-bytes: pass', 'event-order: pass', 'permissions: pass', 'final-files: fail'])
    expect(summarize(report)).toEqual(['FAIL deny-write (fault-protected: protected file changed, final-files vs expected)'])
  }, BUDGET)

  test('altered protected bytes fail the arm even when the expected files agree with them', async () => {
    const fixture = JSON.parse(await readFile(DENY, 'utf8')) as { expected: { files: { path: string; hex: string }[] } }
    const tampered = Buffer.from('tampered\n').toString('hex')
    fixture.expected.files = fixture.expected.files.map(file => file.path === 'check.txt' ? { ...file, hex: tampered } : file)
    const path = join(root, 'deny-write.json')
    await writeFile(path, JSON.stringify(fixture))
    const { report } = await run([fault('protected')], [path])
    const arm = report.fixtures[0]!.arms[0]!
    expect(versusExpected(report, 'fault-protected')).toEqual(['prompt-bytes: pass', 'event-order: pass', 'permissions: pass', 'final-files: pass'])
    expect({ check: arm.protectedCheck, changed: arm.protectedChanged, ok: arm.ok }).toEqual({ check: 'changed', changed: ['check.txt'], ok: false })
  }, BUDGET)

  test.skipIf(!posix)('a protected file replaced by a link to identical bytes is unobserved, not unchanged', async () => {
    // Windows symlinks need a privilege CI runners may not grant.
    const { report } = await run([fault('protected-symlink')], [DENY])
    const arm = report.fixtures[0]!.arms[0]!
    expect({ check: arm.protectedCheck, changed: arm.protectedChanged, workspaceError: arm.workspaceError, ok: arm.ok }).toEqual({
      check: 'unavailable', changed: [], workspaceError: '"check.txt" is not a regular file or directory', ok: false,
    })
    expect(summarize(report)).toEqual(['FAIL deny-write (fault-protected-symlink: unreadable workspace, protected files unobserved)'])
    expect(await readdir(tempRoot)).toEqual([])
  }, BUDGET)

  test('an oversize workspace file leaves the files and protected bytes unobserved', async () => {
    const { report } = await run([fault('oversize')])
    const arm = report.fixtures[0]!.arms[0]!
    expect({ check: arm.protectedCheck, workspaceError: arm.workspaceError, ok: arm.ok })
      .toEqual({ check: 'unavailable', workspaceError: `"large.bin" exceeds ${MAX_TEXT_BYTES} bytes`, ok: false })
    expect(versusExpected(report, 'fault-oversize').at(-1)).toBe('final-files: unavailable')
  }, BUDGET)

  test.skipIf(!permissionsApply)('host errors reach the report only as codes, never as absolute paths', async () => {
    const script = join(root, 'not-executable')
    await writeFile(script, '#!/bin/sh\n', { mode: 0o644 })
    const reportPath = join(root, 'report.json')
    const diagnosticsPath = join(root, 'diagnostics.json')
    const { report, diagnostics } = await run([{ name: 'noexec', argv: [script] }, fault('unreadable')], [ALLOW], { reportPath, diagnosticsPath })
    const [noexec, unreadable] = report.fixtures[0]!.arms
    expect({ spawnError: noexec!.spawnError, exitCode: noexec!.exitCode, signal: noexec!.signal })
      .toEqual({ spawnError: 'EACCES', exitCode: null, signal: null })
    expect({ exitCode: unreadable!.exitCode, workspaceError: unreadable!.workspaceError, check: unreadable!.protectedCheck })
      .toEqual({ exitCode: 0, workspaceError: 'I/O failure EACCES', check: 'unavailable' })
    // The raw messages name the private root; only the diagnostics keep them.
    expect(diagnostics.flatMap(entry => entry.errors).some(message => message.includes(tempRoot))).toBe(true)
    expect(JSON.parse(await readFile(diagnosticsPath, 'utf8'))).toEqual(diagnostics)
    expect((await stat(diagnosticsPath)).mode & 0o777).toBe(0o600)
    const written = await readFile(reportPath, 'utf8')
    for (const path of [root, tempRoot, REPOSITORY]) expect(written).not.toContain(path)
    expect(written).not.toContain('keep trailing space')
    expect(summarize(report)).toEqual(['FAIL allow-write (noexec: spawn failed EACCES, malformed observation, final-files vs expected; fault-unreadable: unreadable workspace, protected files unobserved)'])
    expect(await readdir(tempRoot)).toEqual([])
  }, BUDGET)

  test.each(['garbage', 'no-newline', 'two-lines', 'unknown-field'])('a %s observation is malformed and fails the arm', async (name) => {
    const { report } = await run([fault(name)])
    const arm = report.fixtures[0]!.arms[0]!
    expect(arm.exitCode).toBe(0)
    expect(arm.observationError).toBeString()
    expect(arm.ok).toBe(false)
    // The file comparison still stands on its own.
    expect(versusExpected(report, `fault-${name}`)).toEqual(['prompt-bytes: unavailable', 'event-order: unavailable', 'permissions: unavailable', 'final-files: pass'])
  }, BUDGET)

  test('unbounded stdout stops the owned child as an overflow', async () => {
    // A driver budget inside the test budget turns a missing stop into a reported timeout.
    const { report } = await run([fault('flood')], [ALLOW], { timeoutMs: 20_000 })
    const arm = report.fixtures[0]!.arms[0]!
    expect({ overflow: arm.stdoutOverflow, timedOut: arm.timedOut, ok: arm.ok }).toEqual({ overflow: true, timedOut: false, ok: false })
    expect(arm.stdoutBytes).toBeGreaterThan(512 * 1024)
    expect(await readdir(tempRoot)).toEqual([])
  }, BUDGET)

  test('a child that reached its hang is stopped at the timeout and leaves nothing behind', async () => {
    const marker = join(root, 'hang.pid')
    const { report } = await run([fault('hang', marker)], [ALLOW], { timeoutMs: 15_000 })
    const arm = report.fixtures[0]!.arms[0]!
    // The marker proves the child hung rather than launching slowly.
    const pid = Number(await readFile(marker, 'utf8'))
    expect(pid).toBeGreaterThan(0)
    expect({ timedOut: arm.timedOut, cancelled: arm.cancelled, ok: arm.ok }).toEqual({ timedOut: true, cancelled: false, ok: false })
    expect(isAlive(pid)).toBe(false)
    expect(await readdir(tempRoot)).toEqual([])
  }, BUDGET)

  test('cancellation stops the hanging child, starts no later arm or fixture, and removes its root', async () => {
    const marker = join(root, 'hang.pid')
    const laterMarker = join(root, 'later.json')
    const controller = new AbortController()
    // The driver's own timeout stops the child within the test budget if the marker never appears.
    const pending = run([fault('hang', marker), fault('env', laterMarker)], [ALLOW, DENY], { signal: controller.signal, timeoutMs: 20_000 })
    try {
      const pid = Number(await waitForFile(marker, pending))
      expect(isAlive(pid)).toBe(true)
      controller.abort()
      const { report, diagnostics } = await pending
      expect(report.cancelled).toBe(true)
      expect(report.ok).toBe(false)
      expect(report.fixtures.map(fixture =>
        ({ id: fixture.id, arms: fixture.arms.map(arm => arm.arm), skipped: fixture.skippedArms, pairs: fixture.pairs })))
        .toEqual([{ id: 'allow-write', arms: ['fault-hang'], skipped: ['fault-env'], pairs: [] }])
      expect(report.skippedFixtures).toEqual(['deny-write'])
      expect(report.fixtures[0]!.arms[0]!.cancelled).toBe(true)
      expect(summarize(report)).toEqual(['FAIL allow-write (fault-hang: cancelled, signal SIGKILL, malformed observation; not started: fault-env)', 'skipped deny-write'])
      // Only the hanging child was ever launched.
      expect(diagnostics.map(entry => entry.arm)).toEqual(['fault-hang'])
      expect(await stat(laterMarker).catch(() => null)).toBeNull()
      expect(isAlive(pid)).toBe(false)
      expect(await readdir(tempRoot)).toEqual([])
    } finally {
      controller.abort()
      await pending
    }
  }, BUDGET)

  test('setup errors throw before any arm runs', async () => {
    await expect(run([typescriptArm()], [])).rejects.toThrow('no fixtures to run')
    for (const timeoutMs of [Number.NaN, Number.POSITIVE_INFINITY, 0, -1, 0.5, MAX_TIMEOUT_MS + 1]) {
      await expect(run([typescriptArm()], [ALLOW], { timeoutMs })).rejects.toThrow('timeout must be from 1')
    }
    const missing = { name: 'rust', argv: [join(root, 'missing-runner')] }
    await expect(run([missing])).rejects.toThrow(ConformanceSetupError)
    await expect(run([typescriptArm(), typescriptArm()])).rejects.toThrow('arm names repeat')
    await expect(run([typescriptArm()], [ALLOW, ALLOW])).rejects.toThrow('fixture ids repeat')
    await expect(run([typescriptArm()], [join(FIXTURES, '..', 'invalid', 'unknown-field.json')])).rejects.toThrow('invalid fixture')
    await expect(listFixtures(tempRoot)).rejects.toThrow('holds no fixtures')
    expect(await readdir(tempRoot)).toEqual([])
  })
})

describe('launch', () => {
  const missing = (): string => join(root, 'missing-runner')

  test('a pre-aborted signal reports cancellation without spawning', async () => {
    const controller = new AbortController()
    controller.abort()
    // Spawning a missing executable always reports ENOENT, so its absence shows nothing was spawned.
    const aborted = await launch([missing()], root, {}, Buffer.alloc(0), 5_000, controller.signal)
    expect({ cancelled: aborted.cancelled, spawnError: aborted.spawnError, exitCode: aborted.exitCode })
      .toEqual({ cancelled: true, spawnError: undefined, exitCode: null })
    const control = await launch([missing()], root, {}, Buffer.alloc(0), 5_000, undefined)
    expect({ cancelled: control.cancelled, spawnError: control.spawnError, exitCode: control.exitCode })
      .toEqual({ cancelled: false, spawnError: 'ENOENT', exitCode: null })
    expect(control.errors.join('\n')).toContain('ENOENT')
  }, BUDGET)

  test('input the child never reads is recorded as a stdin failure beside a clean exit', async () => {
    // Far more than a pipe buffer holds, so the write fails once the child exits.
    const input = Buffer.alloc(16 * 1024 * 1024)
    const result = await launch([process.execPath, '-e', ''], root, { PATH: process.env.PATH ?? '' }, input, 20_000, undefined)
    expect({ exitCode: result.exitCode, signal: result.signal, timedOut: result.timedOut })
      .toEqual({ exitCode: 0, signal: null, timedOut: false })
    expect(result.stdinError).toMatch(/^[A-Z][A-Z0-9_]*$/)
  }, BUDGET)
})

describe('snapshotWorkspace', () => {
  // Windows symlinks need a privilege CI runners may not grant.
  test.skipIf(!posix)('rejects a symbolic link', async () => {
    const workspace = join(root, 'linked')
    await mkdir(workspace)
    await symlink(root, join(workspace, 'link'))
    await expect(snapshotWorkspace(workspace)).rejects.toThrow('"link" is not a regular file or directory')
  })

  test.skipIf(!posix)('rejects a root that is a link or not a directory', async () => {
    const workspace = join(root, 'real')
    await mkdir(workspace)
    await symlink(workspace, join(root, 'alias'))
    await writeFile(join(root, 'file'), '')
    await expect(snapshotWorkspace(join(root, 'alias'))).rejects.toThrow('the workspace root is not a directory')
    await expect(snapshotWorkspace(join(root, 'file'))).rejects.toThrow('the workspace root is not a directory')
  })

  test('rejects an oversize file and too many entries', async () => {
    const large = join(root, 'large')
    await mkdir(large)
    await writeFile(join(large, 'exact.bin'), Buffer.alloc(MAX_TEXT_BYTES))
    expect((await snapshotWorkspace(large)).map(file => file.hex.length)).toEqual([2 * MAX_TEXT_BYTES])
    await writeFile(join(large, 'over.bin'), Buffer.alloc(MAX_TEXT_BYTES + 1))
    await expect(snapshotWorkspace(large)).rejects.toThrow(`"over.bin" exceeds ${MAX_TEXT_BYTES} bytes`)
    const crowded = join(root, 'crowded')
    await mkdir(crowded)
    for (let index = 0; index <= MAX_WORKSPACE_ENTRIES; index++) await mkdir(join(crowded, String(index)))
    await expect(snapshotWorkspace(crowded)).rejects.toThrow(`holds more than ${MAX_WORKSPACE_ENTRIES} entries`)
  }, BUDGET)

  test.skipIf(process.platform !== 'linux')('closes every handle it opens, including on a rejection', async () => {
    const workspace = join(root, 'handles')
    const descriptors = async (): Promise<number> => {
      const targets = await Promise.all((await readdir('/proc/self/fd')).map(async (fd) => {
        try {
          return await readlink(`/proc/self/fd/${fd}`)
        } catch (error) {
          // An unrelated descriptor can close between listing and reading it.
          if ((error as NodeJS.ErrnoException).code === 'ENOENT') return ''
          throw error
        }
      }))
      return targets.filter(target => target === workspace || target.startsWith(`${workspace}/`)).length
    }
    await mkdir(join(workspace, 'nested'), { recursive: true })
    await writeFile(join(workspace, 'nested', 'a.txt'), 'a')
    await writeFile(join(workspace, 'b.txt'), 'b')
    const before = await descriptors()
    await snapshotWorkspace(workspace)
    await writeFile(join(workspace, 'nested', 'over.bin'), Buffer.alloc(MAX_TEXT_BYTES + 1))
    await expect(snapshotWorkspace(workspace)).rejects.toThrow('exceeds')
    for (let index = 0; index <= MAX_WORKSPACE_ENTRIES; index++) await writeFile(join(workspace, `f${index}`), '')
    await expect(snapshotWorkspace(workspace)).rejects.toThrow('entries')
    expect(await descriptors()).toBe(before)
  }, BUDGET)
})

describe('driver CLI', () => {
  const cli = async (...args: string[]) => {
    const env = Object.fromEntries(Object.entries(process.env).filter((entry): entry is [string, string] => entry[1] !== undefined))
    const result = await launch([process.execPath, DRIVER, '--output-dir', join(root, 'output'), ...args],
      root, env, Buffer.alloc(0), 20_000, undefined)
    expect({ timedOut: result.timedOut, signal: result.signal, spawnError: result.spawnError })
      .toEqual({ timedOut: false, signal: null, spawnError: undefined })
    return { code: result.exitCode, stdout: result.stdout.toString('utf8'), stderr: result.stderr.toString('utf8') }
  }

  test('--help exits 0', async () => {
    const result = await cli('--help')
    expect(result.code).toBe(0)
    expect(result.stdout).toContain('usage: bun scripts/rust-conformance/driver.ts')
  }, BUDGET)

  test('an unknown argument or a missing value exits 2', async () => {
    expect((await cli('--report', 'x')).code).toBe(2)
    expect((await cli('--fixtures')).code).toBe(2)
  }, BUDGET)

  test('a missing Rust runner exits 2 without running', async () => {
    const result = await cli('--rust-runner', join(root, 'missing-runner'))
    expect(result.code).toBe(2)
    expect(result.stderr).toMatch(/^setup failed: rust runner <external>\/missing-runner is not a file\n$/)
  }, BUDGET)

  test('failed fixture discovery removes a previous report', async () => {
    const output = join(root, 'output')
    await mkdir(output)
    await writeFile(join(output, 'report.json'), '{"ok":true}\n')
    await writeFile(join(output, 'diagnostics.json'), '[]\n')
    const result = await cli('--fixtures', join(root, 'missing-fixtures'))
    expect(result.code).toBe(2)
    expect(result.stderr).toContain('fixture directory <external>/missing-fixtures is unreadable')
    expect(await readdir(output)).toEqual([])
  }, BUDGET)
})
