/**
 * The native fixture adapter over real spawned arms: each case runs the fake
 * arm in tests/fixtures through the evaluator's real fixture, prompt, and
 * `validate()`. The fake is one process with no children, so process-group
 * teardown stays covered by the conformance driver's own launch tests.
 */
import { afterEach, beforeEach, describe, expect, test } from 'bun:test'
import { createHash } from 'node:crypto'
import { mkdir, mkdtemp, readdir, readFile, realpath, rm, stat } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { basename, dirname, join, relative } from 'node:path'
import { MAX_TIMEOUT_MS, STDOUT_LIMIT } from '../../scripts/rust-conformance/driver.ts'
import { NATIVE_FIXTURE_CASES, runNativeFixture, type NativeFixtureOptions } from './native-fixture.ts'
import { prompts } from './scenarios.ts'

const FAKE = join(import.meta.dir, 'tests/fixtures/fake-native-arm.mjs')
// Process-bound cases take the evals-unit lane budget.
const BUDGET = 30_000
const posix = process.platform !== 'win32'

let root: string
let parent: string
let markers: string
const active = new Set<{ controller: AbortController; pending: ReturnType<typeof runNativeFixture> }>()
beforeEach(async () => {
  // Children report resolved paths, including macOS's /var -> /private/var.
  root = await realpath(await mkdtemp(join(tmpdir(), 'bake-native-fixture-test-')))
  parent = join(root, 'runs')
  markers = join(root, 'markers')
  await mkdir(parent)
  await mkdir(markers)
}, BUDGET)
afterEach(async () => {
  // Abort and join runs before removing their paths, even when a concurrent case fails.
  const unfinished = [...active]
  for (const run of unfinished) run.controller.abort()
  await Promise.allSettled(unfinished.map(run => run.pending))
  await rm(root, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 })
}, BUDGET)

const run = (mode: string, extra: Partial<NativeFixtureOptions> = {}, ...args: string[]) => {
  const controller = new AbortController()
  const onAbort = () => controller.abort()
  if (extra.signal?.aborted) controller.abort()
  else extra.signal?.addEventListener('abort', onAbort, { once: true })
  const pending = runNativeFixture({ argv: [process.execPath, FAKE, mode, ...args], scenario: 'ordinary_edit', timeoutMs: 20_000,
    tempRoot: parent, ...extra, signal: controller.signal })
  const owned = { controller, pending }
  active.add(owned)
  return pending.finally(() => {
    active.delete(owned)
    extra.signal?.removeEventListener('abort', onAbort)
  })
}

const isAlive = (pid: number): boolean => {
  try {
    process.kill(pid, 0)
    return true
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === 'ESRCH') return false
    throw error
  }
}

/** Resolves with the file's text once it is non-empty; rejects if `pending` settles first. */
async function waitForFile(path: string, pending: Promise<unknown>): Promise<string> {
  let settled = false
  const done = pending.then(() => { settled = true }, () => { settled = true })
  while (!settled) {
    const text = await readFile(path, 'utf8').catch(() => '')
    if (text !== '') return text
    await Promise.race([done, new Promise(resolve => setTimeout(resolve, 20))])
  }
  throw new Error(`the run settled before ${basename(path)} appeared`)
}

describe('runNativeFixture', () => {
  test('a correct edit succeeds, and the arm received the exact prompt bytes', async () => {
    // A relative executable resolves against this process's cwd, not the arm's workspace.
    const result = await run('edit', { argv: [relative(process.cwd(), process.execPath), FAKE, 'edit'] })
    const expected = createHash('sha256').update(Buffer.from(prompts.ordinary_edit!, 'utf8')).digest('hex')
    expect(NATIVE_FIXTURE_CASES).toEqual(['ordinary_edit'])
    expect(result.promptSha256).toBe(expected)
    expect(result.stdout).toBe(`${expected}\n`)
    expect(result.process).toMatchObject({ exitCode: 0, signal: null, timedOut: false, cancelled: false, stdoutOverflow: false, stderrOverflow: false })
    expect(result.process).not.toHaveProperty('stdout')
    expect(result.process).not.toHaveProperty('errors')
    expect(result.verdict).toMatchObject({ validated: true, testsUnchanged: true, testExit: 0 })
    expect(result.verdict.source).toContain('Math.round(')
    expect(result.success).toBe(true)
    expect(await readdir(parent)).toEqual([])
  }, BUDGET)

  // The replaced check exits 0 over a correct fix, so only the evaluator's
  // byte comparison of test.cjs can reject it.
  test('a correct edit with a replaced test.cjs that passes is rejected by the unchanged-tests check', async () => {
    const result = await run('tamper-tests')
    expect(result.process).toMatchObject({ exitCode: 0, signal: null, timedOut: false })
    expect(result.verdict.source).toContain('Math.round(')
    expect(result.verdict).toMatchObject({ testsUnchanged: false, testExit: 0 })
    expect(result.success).toBe(false)
  }, BUDGET)

  test('a wrong edit with a weakened test.cjs that passes is rejected', async () => {
    const result = await run('tamper-weaken')
    expect(result.process.exitCode).toBe(0)
    expect(result.verdict.source).toContain('Math.ceil(')
    expect(result.verdict).toMatchObject({ validated: false, testsUnchanged: false, testExit: 0 })
    expect(result.success).toBe(false)
  }, BUDGET)

  test('a deleted test.cjs is rejected', async () => {
    const result = await run('delete-tests')
    expect(result.process.exitCode).toBe(0)
    expect(result.verdict).toMatchObject({ validated: false, testsUnchanged: false })
    expect(result.verdict.testExit).not.toBe(0)
    expect(result.success).toBe(false)
  }, BUDGET)

  test('a success claim on stdout without an edit is rejected', async () => {
    const result = await run('claim-only')
    expect(result.process.exitCode).toBe(0)
    expect(result.stdout).toContain('FIXTURE_PASS')
    expect(result.verdict).toMatchObject({ validated: false, testsUnchanged: true })
    expect(result.verdict.testExit).not.toBe(0)
    expect(result.success).toBe(false)
  }, BUDGET)

  test('a correct edit that exits 1 is validated but not a success', async () => {
    const result = await run('edit-exit1')
    expect(result.process).toMatchObject({ exitCode: 1, signal: null, timedOut: false })
    expect(result.verdict).toMatchObject({ validated: true, testsUnchanged: true, testExit: 0 })
    expect(result.success).toBe(false)
  }, BUDGET)

  test('an aborted signal starts no arm', async () => {
    const marker = join(markers, 'pre-abort.pid')
    const controller = new AbortController()
    controller.abort()
    const result = await run('hang', { signal: controller.signal }, marker)
    expect(result.process).toMatchObject({ cancelled: true, exitCode: null, timedOut: false })
    expect(result.process.spawnError).toBeUndefined()
    expect(result.success).toBe(false)
    expect(await stat(marker).catch(() => null)).toBeNull()
    expect(await readdir(parent)).toEqual([])
  }, BUDGET)

  test('cancellation stops a hanging arm and removes its root', async () => {
    const marker = join(markers, 'cancel.pid')
    const controller = new AbortController()
    // The run's own timeout bounds the case if the marker never appears.
    const pending = run('hang', { signal: controller.signal }, marker)
    try {
      const pid = Number(await waitForFile(marker, pending))
      expect(isAlive(pid)).toBe(true)
      controller.abort()
      const result = await pending
      expect(result.process).toMatchObject({ cancelled: true, timedOut: false })
      expect(result.success).toBe(false)
      expect(isAlive(pid)).toBe(false)
      expect(await readdir(parent)).toEqual([])
    } finally {
      controller.abort()
      await pending
    }
  }, BUDGET)

  test('a timeout stops a hanging arm and removes its root', async () => {
    const marker = join(markers, 'timeout.pid')
    const result = await run('hang', { timeoutMs: 1_000 }, marker)
    expect(result.process).toMatchObject({ timedOut: true, cancelled: false, exitCode: null })
    expect(result.success).toBe(false)
    // A slow start can time out before the fake hangs; the cancellation case covers a ready one.
    const pid = Number(await readFile(marker, 'utf8').catch(() => '0'))
    if (pid > 0) expect(isAlive(pid)).toBe(false)
    expect(await readdir(parent)).toEqual([])
  }, BUDGET)

  test('output beyond the stdout cap stops the arm', async () => {
    const result = await run('flood')
    expect(result.process.stdoutOverflow).toBe(true)
    expect(result.process.stdoutBytes).toBeGreaterThan(STDOUT_LIMIT)
    expect(Buffer.byteLength(result.stdout)).toBeLessThanOrEqual(STDOUT_LIMIT)
    expect(result.success).toBe(false)
    expect(await readdir(parent)).toEqual([])
  }, BUDGET)

  test('invalid setup throws before creating a root or starting an arm', async () => {
    const marker = join(markers, 'setup.pid')
    const hang = [process.execPath, FAKE, 'hang', marker]
    const refusals: [Partial<NativeFixtureOptions>, string][] = [
      [{ scenario: 'stale_edit' as never }, 'not a native fixture case'],
      [{ scenario: 'no_tools' as never }, 'not a native fixture case'],
      [{ argv: [] }, 'non-empty list'],
      [{ argv: [process.execPath, 'a\0b'] }, 'without NUL'],
      [{ argv: [join(root, 'missing-arm')] }, 'not an existing file'],
      [{ argv: [root] }, 'not an existing file'],
      ...[Number.NaN, Number.POSITIVE_INFINITY, 0, -1, 0.5, MAX_TIMEOUT_MS + 1]
        .map((timeoutMs): [Partial<NativeFixtureOptions>, string] => [{ argv: hang, timeoutMs }, 'timeout must be from 1']),
    ]
    for (const [extra, message] of refusals) {
      await expect(runNativeFixture({ argv: hang, scenario: 'ordinary_edit', timeoutMs: 20_000, tempRoot: parent, ...extra })).rejects.toThrow(message)
    }
    expect(await readdir(parent)).toEqual([])
    expect(await stat(marker).catch(() => null)).toBeNull()
  }, BUDGET)

  test('concurrent runs keep private roots and give the arm and the check only the minimal environment', async () => {
    const outputs = [join(markers, 'first'), join(markers, 'second')]
    for (const path of outputs) await mkdir(path)
    const completed = await Promise.allSettled([run('env', {}, outputs[0]!),
      run('env', { tempRoot: relative(process.cwd(), parent) }, outputs[1]!), run('edit')])
    const [first, second, edit] = completed.map(result => {
      if (result.status === 'rejected') throw result.reason
      return result.value
    })
    // The env mode replaces test.cjs with its own passing reporter, which the evaluator rejects.
    for (const result of [first!, second!]) expect(result.verdict).toMatchObject({ validated: false, testsUnchanged: false, testExit: 0 })
    expect(edit!.success).toBe(true)

    const allowed = ['BAKE_HOME', 'DSH_HOME', 'HOME', 'PATH', 'TEMP', 'TMP', 'TMPDIR']
    const roots = new Set<string>()
    for (const output of outputs) {
      type Observed = { names: string[]; cwd: string; paths: Record<string, string | null> }
      const arm = JSON.parse(await readFile(join(output, 'arm.json'), 'utf8')) as Observed
      const check = JSON.parse(await readFile(join(output, 'check.json'), 'utf8')) as Observed
      const runRoot = dirname(arm.cwd)
      expect(dirname(runRoot)).toBe(parent)
      expect(basename(runRoot).startsWith('bake-native-eval-')).toBe(true)
      roots.add(runRoot)
      const home = join(runRoot, 'home')
      const temporary = join(runRoot, 'tmp')
      const paths = { HOME: home, BAKE_HOME: join(home, '.bake'), DSH_HOME: join(home, '.bake'), TMPDIR: temporary, TMP: temporary, TEMP: temporary }
      for (const observed of [arm, check]) {
        expect(observed.cwd).toBe(join(runRoot, 'workspace'))
        expect(observed.paths).toEqual(paths)
        // CoreFoundation can add its text-encoding setting after exec on macOS.
        const names = observed.names.filter(name => process.platform !== 'darwin' || name !== '__CF_USER_TEXT_ENCODING')
        // On Windows libuv adds required system variables such as SYSTEMROOT to any child environment.
        if (posix) expect(names).toEqual(allowed)
        else expect(observed.names.filter(name => /KEY|TOKEN|SECRET|PASSWORD/i.test(name))).toEqual([])
      }
    }
    expect(roots.size).toBe(2)
    expect(await readdir(parent)).toEqual([])
  }, BUDGET)
})
