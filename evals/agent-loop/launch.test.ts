/**
 * `launch()` over real child processes: cancellation before spawn, a stdin
 * write the child never reads, the stdout bound, and a hanging child stopped
 * at its timeout and by cancellation. These cases moved here with the
 * launcher from the retired synthetic conformance driver.
 */
import { afterEach, beforeEach, describe, expect, test } from 'bun:test'
import { mkdtemp, readFile, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { childEnvironment, launch, STDOUT_LIMIT } from './launch.ts'

// Process-bound cases take the evals-unit lane budget.
const BUDGET = 30_000

let root: string
beforeEach(async () => {
  root = await mkdtemp(join(tmpdir(), 'bake-eval-launch-test-'))
}, BUDGET)
afterEach(async () => {
  await rm(root, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 })
}, BUDGET)

const env = (): Record<string, string> => ({ PATH: process.env.PATH ?? '' })

const isAlive = (pid: number): boolean => {
  try {
    process.kill(pid, 0)
    return true
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === 'ESRCH') return false
    throw error
  }
}

/** A child that records its pid once it has reached its hang, then stays alive. */
const hang = (marker: string): string[] => [process.execPath, '-e',
  `require('node:fs').writeFileSync(${JSON.stringify(marker)}, String(process.pid)); setInterval(() => {}, 1 << 30)`]

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
    // Far more than a pipe buffer holds, so the write is still queued when the child exits;
    // depending on the platform it then fails or is cancelled, before or after `close`.
    const input = Buffer.alloc(16 * 1024 * 1024)
    const result = await launch([process.execPath, '-e', ''], root, env(), input, 20_000, undefined)
    expect({ exitCode: result.exitCode, signal: result.signal, timedOut: result.timedOut })
      .toEqual({ exitCode: 0, signal: null, timedOut: false })
    expect(result.stdinError).toMatch(/^[A-Z][A-Z0-9_]*$/)
  }, BUDGET)

  test('unbounded stdout stops the child as an overflow', async () => {
    const flood = [process.execPath, '-e', `const block = 'x'.repeat(64 * 1024); const write = () => process.stdout.write(block, write); write()`]
    // A launch budget inside the test budget turns a missing stop into a reported timeout.
    const result = await launch(flood, root, env(), Buffer.alloc(0), 20_000, undefined)
    expect({ overflow: result.stdoutOverflow, timedOut: result.timedOut }).toEqual({ overflow: true, timedOut: false })
    expect(result.stdoutBytes).toBeGreaterThan(STDOUT_LIMIT)
    expect(result.stdout.byteLength).toBeLessThanOrEqual(STDOUT_LIMIT)
  }, BUDGET)

  test('a child that reached its hang is stopped at the timeout', async () => {
    const marker = join(root, 'hang.pid')
    const result = await launch(hang(marker), root, env(), Buffer.alloc(0), 15_000, undefined)
    // The marker proves the child hung rather than launching slowly.
    const pid = Number(await readFile(marker, 'utf8'))
    expect(pid).toBeGreaterThan(0)
    expect({ timedOut: result.timedOut, cancelled: result.cancelled, exitCode: result.exitCode })
      .toEqual({ timedOut: true, cancelled: false, exitCode: null })
    expect(isAlive(pid)).toBe(false)
  }, BUDGET)

  test('cancellation stops a hanging child', async () => {
    const marker = join(root, 'hang.pid')
    const controller = new AbortController()
    // The launch timeout stops the child within the test budget if the marker never appears.
    const pending = launch(hang(marker), root, env(), Buffer.alloc(0), 20_000, controller.signal)
    try {
      const pid = Number(await waitForFile(marker, pending))
      expect(isAlive(pid)).toBe(true)
      controller.abort()
      const result = await pending
      expect({ cancelled: result.cancelled, timedOut: result.timedOut }).toEqual({ cancelled: true, timedOut: false })
      expect(isAlive(pid)).toBe(false)
    } finally {
      controller.abort()
      await pending
    }
  }, BUDGET)
})

describe('childEnvironment', () => {
  test('passes only the search path and private homes', () => {
    const env = childEnvironment('/h', '/t')
    expect(env.PATH).toBe(process.env.PATH ?? '')
    expect({ HOME: env.HOME, BAKE_HOME: env.BAKE_HOME, DSH_HOME: env.DSH_HOME, TMPDIR: env.TMPDIR, TMP: env.TMP, TEMP: env.TEMP })
      .toEqual({ HOME: '/h', BAKE_HOME: join('/h', '.bake'), DSH_HOME: join('/h', '.bake'), TMPDIR: '/t', TMP: '/t', TEMP: '/t' })
    if (process.platform !== 'win32') expect(Object.keys(env).sort()).toEqual(['BAKE_HOME', 'DSH_HOME', 'HOME', 'PATH', 'TEMP', 'TMP', 'TMPDIR'])
  })
})
