/**
 * Synthetic conformance driver. For each shared fixture it prepares a private
 * workspace per arm, sends the fixture input (never its expected values) on
 * stdin, and compares each arm's observation and final files with the
 * fixture's expected values and with every other arm.
 *
 * This qualifies the comparison harness on synthetic runners only. It is not
 * evidence of runtime, policy, session, sandbox, or model parity, and the
 * arms are trusted: the private roots and minimal environment keep runs apart,
 * not contain a hostile executable.
 *
 * Usage: `bun scripts/rust-conformance/driver.ts [--fixtures <dir>] [--rust-runner <path>] [--output-dir <dir>]`
 */

import { spawn, type ChildProcessByStdio } from 'node:child_process'
import { createHash } from 'node:crypto'
import { constants, createReadStream } from 'node:fs'
import { lstat, mkdir, mkdtemp, open, opendir, readdir, readFile, rename, rm, stat, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import type { Readable, Writable } from 'node:stream'
import { basename, dirname, isAbsolute, join, relative, resolve, sep } from 'node:path'
import { compareAll, type ComparatorResult, type Outcome } from './compare.ts'
import {
  ConformanceInputError, MAX_DOCUMENT_BYTES, MAX_TEXT_BYTES, parseStrictJson, validateFixture, validateObservation,
  type FileEntry, type Fixture,
} from './fixture.ts'

const ROOT = resolve(import.meta.dirname, '../..')
const RUNNER = join(import.meta.dirname, 'runner.ts')

/** Default per-arm budget; generous enough for a cold CI launch. */
export const DEFAULT_TIMEOUT_MS = 30_000
/** Bytes kept from each stream; more stops the arm as an overflow. */
export const STDOUT_LIMIT = 2 * MAX_DOCUMENT_BYTES
export const STDERR_LIMIT = 64 * 1024
/** Most regular files read back from one workspace. */
export const MAX_WORKSPACE_FILES = 256
/** Most directory entries of any kind visited in one workspace. */
export const MAX_WORKSPACE_ENTRIES = 1024
/** The largest budget a Node timer holds without firing at once. */
export const MAX_TIMEOUT_MS = 2 ** 31 - 1

export const DEFAULT_FIXTURES = join(ROOT, 'conformance', 'fixtures')
export const DEFAULT_REPORT = join(ROOT, '.preflight', 'rust-conformance', 'report.json')
export const DEFAULT_RUST_RUNNER = join(ROOT, 'rust', 'target', 'debug', `bake-conformance-runner${process.platform === 'win32' ? '.exe' : ''}`)

/** One runner under comparison. */
export interface ArmSpec {
  /** Short label used in the report. */
  name: string
  /** Executable and arguments; the executable must be an existing file. */
  argv: readonly string[]
  /** Files whose digests identify what ran; defaults to the executable. */
  artifacts?: readonly string[]
}

/** The Bun runner as an arm. */
export const typescriptArm = (): ArmSpec => ({
  name: 'typescript',
  argv: [process.execPath, RUNNER],
  artifacts: [RUNNER, join(import.meta.dirname, 'fixture.ts')],
})

/** The built Rust runner as an arm. */
export const rustArm = (binary = DEFAULT_RUST_RUNNER): ArmSpec => ({ name: 'rust', argv: [binary] })

export interface RunOptions {
  /** Fixture file paths, run in this order. */
  fixtures: readonly string[]
  arms: readonly ArmSpec[]
  /** Parent of the private per-run roots; the default is the OS temporary directory. */
  tempRoot?: string
  /** Where the sanitized report is written; omitted means not written. */
  reportPath?: string
  /** Optional separate file for raw stderr and host error messages. */
  diagnosticsPath?: string
  /** Per-arm budget: a finite number of milliseconds from 1 to {@link MAX_TIMEOUT_MS}. */
  timeoutMs?: number
  /** Stops the current arm and starts no further arm or fixture. */
  signal?: AbortSignal
}

/**
 * One arm's run of one fixture. Process facts are reported independently.
 * Error fields hold a stable code or a workspace-relative message, never a
 * raw host path; raw detail goes to {@link Diagnostic}.
 */
export interface ArmRun {
  arm: string
  exitCode: number | null
  signal: string | null
  timedOut: boolean
  cancelled: boolean
  stdoutOverflow: boolean
  stderrOverflow: boolean
  stdoutBytes: number
  stderrBytes: number
  /** Error code when the child could not start, such as `ENOENT`. */
  spawnError?: string
  /** Error code when the input could not be delivered on stdin, such as `EPIPE`. */
  stdinError?: string
  /** Error code when stopping the child failed, such as `EPERM`. */
  stopError?: string
  observationError?: string
  workspaceError?: string
  /**
   * `unavailable` when the workspace could not be read back, so protected
   * bytes were not observed; that fails the arm without a change claim.
   */
  protectedCheck: 'pass' | 'changed' | 'unavailable'
  /** Protected files whose bytes changed or that disappeared. */
  protectedChanged: string[]
  /** All four comparators against the fixture's expected values. */
  expected: ComparatorResult[]
  ok: boolean
}

export interface PairRun { left: string; right: string; comparisons: ComparatorResult[]; ok: boolean }

/** One fixture's runs. `skippedArms` names arms never started because the run was cancelled. */
export interface FixtureRun {
  id: string
  path: string
  sha256: string
  arms: ArmRun[]
  skippedArms: string[]
  pairs: PairRun[]
  ok: boolean
}

export interface ConformanceReport {
  schema: 'bake/synthetic-conformance/report'
  version: 1
  scope: string
  host: { platform: string; arch: string; runtime: string }
  timeoutMs: number
  arms: { name: string; command: string[]; artifacts: { path: string; sha256: string }[] }[]
  fixtures: FixtureRun[]
  /** Ids of fixtures never started because the run was cancelled. */
  skippedFixtures: string[]
  cancelled: boolean
  ok: boolean
}

/** Raw stderr and error messages per run, kept out of the sanitized report. */
export interface Diagnostic { fixture: string; arm: string; stderr: string; errors: string[] }

export interface ConformanceResult { report: ConformanceReport; diagnostics: Diagnostic[] }

/** A setup problem: an unreadable or invalid fixture, or a missing runner. */
export class ConformanceSetupError extends Error {
  override name = 'ConformanceSetupError'
}

const sha256 = (bytes: Uint8Array): string => createHash('sha256').update(bytes).digest('hex')

/** Publish a whole JSON document; readers never see a partially written run. */
async function writeJson(path: string, value: unknown): Promise<void> {
  await mkdir(dirname(path), { recursive: true })
  const temporary = await mkdtemp(join(dirname(path), '.conformance-'))
  try {
    const file = join(temporary, 'output.json')
    await writeFile(file, `${JSON.stringify(value, null, 2)}\n`, { flag: 'wx', mode: 0o600 })
    await rename(file, path)
  } finally {
    await rm(temporary, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 })
  }
}

async function clearOutputs(...paths: (string | undefined)[]): Promise<void> {
  for (const path of paths) if (path !== undefined) await rm(path, { force: true })
}

async function fileDigest(path: string): Promise<string> {
  const hash = createHash('sha256')
  for await (const chunk of createReadStream(path)) hash.update(chunk as Buffer)
  return hash.digest('hex')
}

/** Repository paths become relative; anything else keeps only its name. */
function sanitize(value: string): string {
  if (!isAbsolute(value)) return value
  const path = relative(ROOT, value)
  return path === '' || path.startsWith('..') || isAbsolute(path) ? `<external>/${basename(value)}` : path.split(sep).join('/')
}

/**
 * List the `*.json` fixtures in one directory, sorted by name.
 * @param directory - the fixture directory.
 */
export async function listFixtures(directory: string): Promise<string[]> {
  let names: string[]
  try {
    names = await readdir(directory)
  } catch {
    throw new ConformanceSetupError(`fixture directory ${sanitize(directory)} is unreadable`)
  }
  const paths = names.filter(name => name.endsWith('.json')).sort().map(name => join(directory, name))
  if (paths.length === 0) throw new ConformanceSetupError(`fixture directory ${sanitize(directory)} holds no fixtures`)
  return paths
}

async function loadFixture(path: string): Promise<{ fixture: Fixture; sha256: string }> {
  let bytes: Buffer
  try {
    if ((await stat(path)).size > MAX_DOCUMENT_BYTES) throw new ConformanceSetupError(`fixture ${sanitize(path)} exceeds ${MAX_DOCUMENT_BYTES} bytes`)
    bytes = await readFile(path)
  } catch (error) {
    if (error instanceof ConformanceSetupError) throw error
    throw new ConformanceSetupError(`fixture ${sanitize(path)} is unreadable`)
  }
  try {
    return { fixture: validateFixture(parseStrictJson(bytes, sanitize(path))), sha256: sha256(bytes) }
  } catch (error) {
    if (error instanceof ConformanceInputError) throw new ConformanceSetupError(`invalid fixture ${error.message}`)
    throw error
  }
}

const isFile = async (path: string): Promise<boolean> => (await stat(path).catch(() => undefined))?.isFile() === true

/** The minimal child environment: a search path and private homes, nothing inherited beyond them. */
function childEnvironment(home: string, temporary: string): Record<string, string> {
  const env: Record<string, string> = {
    PATH: process.env.PATH ?? '',
    HOME: home, BAKE_HOME: join(home, '.bake'), DSH_HOME: join(home, '.bake'),
    TMPDIR: temporary, TMP: temporary, TEMP: temporary,
  }
  if (process.platform === 'win32') {
    env.USERPROFILE = home
    for (const name of ['SystemRoot', 'windir', 'ComSpec', 'PATHEXT']) {
      const value = process.env[name]
      if (value !== undefined) env[name] = value
    }
  }
  return env
}

export interface Launch {
  exitCode: number | null
  signal: string | null
  timedOut: boolean
  cancelled: boolean
  stdout: Buffer
  stderr: Buffer
  stdoutOverflow: boolean
  stderrOverflow: boolean
  stdoutBytes: number
  stderrBytes: number
  spawnError?: string
  stdinError?: string
  stopError?: string
  /** Raw error messages, which may name host paths. */
  errors: string[]
}

const errorCode = (error: unknown): string => {
  const code = (error as NodeJS.ErrnoException | undefined)?.code
  return typeof code === 'string' && /^[A-Z][A-Z0-9_]*$/u.test(code) ? code : 'UNKNOWN'
}

/**
 * Run one owned child to completion. On POSIX it leads its own process group
 * so a timeout, overflow, or cancellation stops that group and nothing else;
 * when stopping the group fails, only the direct child is retried. Resolves
 * only after the child closes, including after an asynchronous spawn error,
 * and never starts a child once `signal` has aborted.
 * @param argv - executable and arguments.
 * @param cwd - the working directory.
 * @param env - the complete child environment.
 * @param input - bytes written to stdin, which is then closed.
 * @param timeoutMs - budget before the child is stopped.
 * @param signal - stops the child when aborted.
 */
export function launch(argv: readonly string[], cwd: string, env: Record<string, string>, input: Uint8Array,
  timeoutMs: number, signal: AbortSignal | undefined): Promise<Launch> {
  return new Promise((resolvePromise) => {
    const posix = process.platform !== 'win32'
    const state: Launch = {
      exitCode: null, signal: null, timedOut: false, cancelled: false,
      stdout: Buffer.alloc(0), stderr: Buffer.alloc(0),
      stdoutOverflow: false, stderrOverflow: false, stdoutBytes: 0, stderrBytes: 0, errors: [],
    }
    if (signal?.aborted) {
      resolvePromise({ ...state, cancelled: true })
      return
    }
    const [command, ...args] = argv
    let child: ChildProcessByStdio<Writable, Readable, Readable>
    try {
      if (command === undefined) throw Object.assign(new Error('no executable'), { code: 'ENOENT' })
      child = spawn(command, args, { cwd, env, stdio: ['pipe', 'pipe', 'pipe'], detached: posix, windowsHide: true })
    } catch (error) {
      resolvePromise({ ...state, spawnError: errorCode(error), errors: [(error as Error).message] })
      return
    }
    let closed = false
    // Runs inside timer, abort, and stream callbacks, so it records failures instead of throwing.
    const stop = (): void => {
      if (closed || child.pid === undefined) return
      if (posix) {
        try {
          process.kill(-child.pid, 'SIGKILL')
          return
        } catch (error) {
          if (errorCode(error) === 'ESRCH') return
          state.stopError ??= errorCode(error)
          state.errors.push((error as Error).message)
        }
      }
      try {
        child.kill('SIGKILL')
      } catch (error) {
        state.stopError ??= errorCode(error)
        state.errors.push((error as Error).message)
      }
    }
    const timer = setTimeout(() => { state.timedOut = true; stop() }, timeoutMs)
    const onAbort = (): void => { state.cancelled = true; stop() }
    signal?.addEventListener('abort', onAbort, { once: true })
    const collect = (stream: 'stdout' | 'stderr', limit: number) => (chunk: Buffer): void => {
      state[`${stream}Bytes`] += chunk.byteLength
      if (state[`${stream}Overflow`]) return
      if (state[stream].byteLength + chunk.byteLength > limit) {
        state[`${stream}Overflow`] = true
        stop()
        return
      }
      state[stream] = Buffer.concat([state[stream], chunk])
    }
    child.stdout.on('data', collect('stdout', STDOUT_LIMIT))
    child.stderr.on('data', collect('stderr', STDERR_LIMIT))
    child.stdin.on('error', (error) => {
      // A spawn failure also closes stdin; that is reported as the spawn error.
      if (child.pid === undefined) return
      state.stdinError ??= errorCode(error)
      state.errors.push(error.message)
    })
    child.stdin.end(input)
    child.on('error', (error) => {
      if (child.pid === undefined) state.spawnError ??= errorCode(error)
      state.errors.push(error.message)
    })
    child.on('close', (code, killSignal) => {
      if (closed) return
      closed = true
      clearTimeout(timer)
      signal?.removeEventListener('abort', onAbort)
      // After a spawn error, `close` reports a negative errno rather than an exit status.
      if (state.spawnError === undefined) {
        state.exitCode = code
        state.signal = killSignal
      }
      resolvePromise(state)
    })
  })
}

/** A workspace that could not be read back; the message names only workspace-relative paths. */
export class WorkspaceError extends Error {
  override name = 'WorkspaceError'
}

/**
 * Read at most `MAX_TEXT_BYTES + 1` bytes of one regular file. On POSIX the
 * open neither follows a final link nor blocks on a FIFO swapped in after
 * `lstat`; the handle's own type is checked before reading.
 */
async function readBoundedFile(path: string, relativePath: string): Promise<Buffer> {
  const handle = await open(path, constants.O_RDONLY | (constants.O_NOFOLLOW ?? 0) | (constants.O_NONBLOCK ?? 0))
  try {
    if (!(await handle.stat()).isFile()) throw new WorkspaceError(`${JSON.stringify(relativePath)} is not a regular file or directory`)
    const buffer = Buffer.alloc(MAX_TEXT_BYTES + 1)
    let length = 0
    while (length < buffer.byteLength) {
      const { bytesRead } = await handle.read(buffer, length, buffer.byteLength - length, null)
      if (bytesRead === 0) break
      length += bytesRead
    }
    if (length > MAX_TEXT_BYTES) throw new WorkspaceError(`${JSON.stringify(relativePath)} exceeds ${MAX_TEXT_BYTES} bytes`)
    return buffer.subarray(0, length)
  } finally {
    await handle.close()
  }
}

/**
 * Read back every regular file under `root`, which must itself be a real
 * directory. Links and special files are rejected, and the entries visited,
 * files kept, and bytes read per file are bounded; empty directories are not
 * observed. Throws {@link WorkspaceError} for a rejected workspace; host I/O
 * errors propagate unchanged.
 * @param root - the workspace.
 */
export async function snapshotWorkspace(root: string): Promise<FileEntry[]> {
  if (!(await lstat(root)).isDirectory()) throw new WorkspaceError('the workspace root is not a directory')
  const files: FileEntry[] = []
  let entries = 0
  const visit = async (directory: string, prefix: string): Promise<void> => {
    const names: string[] = []
    // The iterator closes the directory handle on completion and on a throw.
    for await (const entry of await opendir(directory)) {
      if (++entries > MAX_WORKSPACE_ENTRIES) throw new WorkspaceError(`holds more than ${MAX_WORKSPACE_ENTRIES} entries`)
      names.push(entry.name)
    }
    for (const name of names.sort()) {
      const path = join(directory, name)
      const relativePath = prefix === '' ? name : `${prefix}/${name}`
      const entry = await lstat(path)
      if (entry.isDirectory()) await visit(path, relativePath)
      else if (!entry.isFile()) throw new WorkspaceError(`${JSON.stringify(relativePath)} is not a regular file or directory`)
      else {
        if (files.length >= MAX_WORKSPACE_FILES) throw new WorkspaceError(`holds more than ${MAX_WORKSPACE_FILES} files`)
        files.push({ path: relativePath, hex: (await readBoundedFile(path, relativePath)).toString('hex') })
      }
    }
  }
  await visit(root, '')
  return files
}

/** Parse stdout as exactly one JSON observation followed by one LF. */
function readObservation(stdout: Buffer): ReturnType<typeof validateObservation> {
  if (stdout.at(-1) !== 0x0a) throw new ConformanceInputError('stdout: must end with one LF')
  const body = stdout.subarray(0, -1)
  if (/^\s|\s$/u.test(body.toString('latin1'))) throw new ConformanceInputError('stdout: has whitespace around the observation')
  return validateObservation(parseStrictJson(body, 'stdout'))
}

const failed = (run: ArmRun): boolean =>
  run.exitCode !== 0 || run.signal !== null || run.timedOut || run.cancelled || run.stdoutOverflow || run.stderrOverflow
  || run.spawnError !== undefined || run.stdinError !== undefined || run.stopError !== undefined
  || run.observationError !== undefined || run.workspaceError !== undefined || run.protectedCheck !== 'pass'
  || run.expected.some(entry => entry.outcome !== 'pass')

async function runArm(fixture: Fixture, arm: ArmSpec, options: RunOptions, timeoutMs: number, diagnostics: Diagnostic[]):
Promise<{ run: ArmRun; outcome: Outcome }> {
  const root = await mkdtemp(join(options.tempRoot ?? tmpdir(), 'bake-conformance-'))
  try {
    const home = join(root, 'home')
    const temporary = join(root, 'tmp')
    const work = join(root, 'work')
    await Promise.all([mkdir(home), mkdir(temporary), mkdir(work)])
    for (const file of fixture.initialFiles) {
      const target = join(work, ...file.path.split('/'))
      await mkdir(dirname(target), { recursive: true })
      await writeFile(target, Buffer.from(file.hex, 'hex'), { flag: 'wx' })
    }

    const input = Buffer.from(JSON.stringify(fixture.input), 'utf8')
    const result = await launch(arm.argv, work, childEnvironment(home, temporary), input, timeoutMs, options.signal)
    const errors = [...result.errors]
    diagnostics.push({ fixture: fixture.id, arm: arm.name, stderr: result.stderr.toString('utf8'), errors })

    const run: ArmRun = {
      arm: arm.name, exitCode: result.exitCode, signal: result.signal, timedOut: result.timedOut, cancelled: result.cancelled,
      stdoutOverflow: result.stdoutOverflow, stderrOverflow: result.stderrOverflow,
      stdoutBytes: result.stdoutBytes, stderrBytes: result.stderrBytes,
      protectedCheck: 'unavailable', protectedChanged: [], expected: [], ok: false,
    }
    if (result.spawnError !== undefined) run.spawnError = result.spawnError
    if (result.stdinError !== undefined) run.stdinError = result.stdinError
    if (result.stopError !== undefined) run.stopError = result.stopError
    const outcome: Outcome = {}
    if (!result.stdoutOverflow) {
      try {
        const observation = readObservation(result.stdout)
        outcome.prompts = observation.prompts
        outcome.events = observation.events
        outcome.permissions = observation.permissions
      } catch (error) {
        if (!(error instanceof ConformanceInputError)) throw error
        run.observationError = error.message
      }
    }
    try {
      outcome.files = await snapshotWorkspace(work)
    } catch (error) {
      errors.push((error as Error).message)
      run.workspaceError = error instanceof WorkspaceError ? error.message : `I/O failure ${errorCode(error)}`
    }
    if (outcome.files !== undefined) {
      // Protected bytes come only from the bounded snapshot, compared with what the driver wrote.
      const observed = new Map(outcome.files.map(file => [file.path, file.hex]))
      const initial = new Map(fixture.initialFiles.map(file => [file.path, file.hex]))
      run.protectedChanged = fixture.protectedFiles.filter(path => observed.get(path) !== initial.get(path))
      run.protectedCheck = run.protectedChanged.length === 0 ? 'pass' : 'changed'
    }
    run.expected = compareAll(outcome, fixture.expected)
    run.ok = !failed(run)
    return { run, outcome }
  } finally {
    await rm(root, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 })
  }
}

/**
 * Run every fixture through every arm and compare the results. Each arm runs
 * in its own private root, which is removed after the arm's process closes.
 * Throws {@link ConformanceSetupError} before any arm runs when there is no
 * fixture or arm, the budget is invalid, or a fixture or runner is unusable.
 *
 * Once `signal` aborts, the running arm is stopped and no further arm or
 * fixture starts. A fixture cut short keeps the arms that ran and names the
 * rest in `skippedArms`; fixtures never started are listed in `skippedFixtures`.
 * @param options - fixtures, arms, owned paths, and the per-arm budget.
 */
export async function runConformance(options: RunOptions): Promise<ConformanceResult> {
  // A failed setup or interrupted run must not leave an earlier success as its evidence.
  await clearOutputs(options.reportPath, options.diagnosticsPath)
  if (options.fixtures.length === 0) throw new ConformanceSetupError('no fixtures to run')
  if (options.arms.length === 0) throw new ConformanceSetupError('no arms to run')
  const timeoutMs = options.timeoutMs ?? DEFAULT_TIMEOUT_MS
  if (!Number.isFinite(timeoutMs) || timeoutMs < 1 || timeoutMs > MAX_TIMEOUT_MS) {
    throw new ConformanceSetupError(`timeout must be from 1 to ${MAX_TIMEOUT_MS} milliseconds`)
  }
  const names = options.arms.map(arm => arm.name)
  if (new Set(names).size !== names.length) throw new ConformanceSetupError('arm names repeat')
  for (const arm of options.arms) {
    const [executable = ''] = arm.argv
    if (!await isFile(executable)) throw new ConformanceSetupError(`${arm.name} runner ${sanitize(executable)} is not a file`)
  }
  const loaded = []
  for (const path of options.fixtures) loaded.push({ path, ...await loadFixture(path) })
  const ids = loaded.map(entry => entry.fixture.id)
  if (new Set(ids).size !== ids.length) throw new ConformanceSetupError('fixture ids repeat')

  const arms = await Promise.all(options.arms.map(async arm => ({
    name: arm.name,
    command: arm.argv.map(sanitize),
    artifacts: await Promise.all((arm.artifacts ?? arm.argv.slice(0, 1))
      .map(async path => ({ path: sanitize(path), sha256: await fileDigest(path) }))),
  })))
  const diagnostics: Diagnostic[] = []
  const fixtures: FixtureRun[] = []
  const skippedFixtures: string[] = []
  for (const { path, fixture, sha256: digest } of loaded) {
    if (options.signal?.aborted) {
      skippedFixtures.push(fixture.id)
      continue
    }
    const runs = []
    const skippedArms: string[] = []
    for (const arm of options.arms) {
      if (options.signal?.aborted) skippedArms.push(arm.name)
      else runs.push(await runArm(fixture, arm, options, timeoutMs, diagnostics))
    }
    const pairs: PairRun[] = []
    for (const [index, left] of runs.entries()) {
      for (const right of runs.slice(index + 1)) {
        const comparisons = compareAll(left.outcome, right.outcome)
        pairs.push({ left: left.run.arm, right: right.run.arm, comparisons, ok: comparisons.every(entry => entry.outcome === 'pass') })
      }
    }
    fixtures.push({
      id: fixture.id, path: sanitize(path), sha256: digest, arms: runs.map(entry => entry.run), skippedArms, pairs,
      ok: skippedArms.length === 0 && runs.every(entry => entry.run.ok) && pairs.every(pair => pair.ok),
    })
  }
  const cancelled = options.signal?.aborted === true
  const report: ConformanceReport = {
    schema: 'bake/synthetic-conformance/report', version: 1,
    scope: 'synthetic comparison-harness qualification only; not runtime, policy, session, sandbox, or model parity',
    host: { platform: process.platform, arch: process.arch, runtime: `bun ${process.versions.bun ?? 'unknown'}` },
    timeoutMs, arms, fixtures, skippedFixtures, cancelled,
    ok: !cancelled && skippedFixtures.length === 0 && fixtures.every(entry => entry.ok),
  }
  if (options.diagnosticsPath !== undefined) await writeJson(options.diagnosticsPath, diagnostics)
  if (options.reportPath !== undefined) await writeJson(options.reportPath, report)
  return { report, diagnostics }
}

/** One line per failing arm or pair, naming only what failed. */
export function summarize(report: ConformanceReport): string[] {
  const lines: string[] = []
  for (const fixture of report.fixtures) {
    const problems: string[] = []
    for (const arm of fixture.arms) {
      const facts = [
        arm.spawnError !== undefined && `spawn failed ${arm.spawnError}`, arm.stdinError !== undefined && `stdin failed ${arm.stdinError}`,
        arm.stopError !== undefined && `stop failed ${arm.stopError}`, arm.timedOut && 'timed out', arm.cancelled && 'cancelled',
        arm.stdoutOverflow && 'stdout overflow', arm.stderrOverflow && 'stderr overflow',
        arm.signal !== null && `signal ${arm.signal}`, arm.exitCode !== 0 && arm.exitCode !== null && `exit ${arm.exitCode}`,
        arm.observationError !== undefined && 'malformed observation', arm.workspaceError !== undefined && 'unreadable workspace',
        arm.protectedCheck === 'changed' && 'protected file changed', arm.protectedCheck === 'unavailable' && 'protected files unobserved',
        ...arm.expected.filter(entry => entry.outcome === 'fail').map(entry => `${entry.comparator} vs expected`),
      ].filter((fact): fact is string => typeof fact === 'string')
      if (facts.length > 0) problems.push(`${arm.arm}: ${facts.join(', ')}`)
    }
    for (const pair of fixture.pairs) {
      const failed = pair.comparisons.filter(entry => entry.outcome === 'fail').map(entry => entry.comparator)
      if (failed.length > 0) problems.push(`${pair.left} vs ${pair.right}: ${failed.join(', ')}`)
    }
    if (fixture.skippedArms.length > 0) problems.push(`not started: ${fixture.skippedArms.join(', ')}`)
    lines.push(`${fixture.ok ? 'pass' : 'FAIL'} ${fixture.id}${problems.length > 0 ? ` (${problems.join('; ')})` : ''}`)
  }
  for (const id of report.skippedFixtures) lines.push(`skipped ${id}`)
  return lines
}

const HELP = `usage: bun scripts/rust-conformance/driver.ts [options]

Compare the Bun synthetic runner with the built Rust runner on the shared
fixtures. A synthetic harness check, not a runtime parity claim.

options:
  --fixtures <dir>      fixture directory (default conformance/fixtures)
  --rust-runner <path>  Rust runner (default rust/target/debug/bake-conformance-runner)
  --output-dir <dir>    report and diagnostics directory (default .preflight/rust-conformance)
  --help                show this help

The output directory holds sanitized report.json and raw diagnostics.json.
Exit 0 when every comparison passes, 1 when a comparison or run fails,
2 for invalid arguments or setup.`

async function main(argv: readonly string[]): Promise<number> {
  let fixtures = DEFAULT_FIXTURES
  let rustRunner = DEFAULT_RUST_RUNNER
  let outputDirectory = dirname(DEFAULT_REPORT)
  for (let index = 0; index < argv.length; index++) {
    const option = argv[index]
    const value = argv[index + 1]
    if (option === '--help') {
      console.log(HELP)
      return 0
    }
    if ((option === '--fixtures' || option === '--rust-runner' || option === '--output-dir') && value !== undefined && !value.startsWith('--')) {
      if (option === '--fixtures') fixtures = resolve(value)
      else if (option === '--rust-runner') rustRunner = resolve(value)
      else outputDirectory = resolve(value)
      index++
      continue
    }
    console.error(`invalid argument ${JSON.stringify(option)}; see --help`)
    return 2
  }
  const controller = new AbortController()
  const interrupt = (): void => controller.abort()
  process.once('SIGINT', interrupt)
  process.once('SIGTERM', interrupt)
  try {
    const reportPath = join(outputDirectory, 'report.json')
    const diagnosticsPath = join(outputDirectory, 'diagnostics.json')
    // Fixture discovery can fail before runConformance is entered.
    await clearOutputs(reportPath, diagnosticsPath)
    const { report } = await runConformance({
      fixtures: await listFixtures(fixtures),
      arms: [typescriptArm(), rustArm(rustRunner)],
      reportPath, diagnosticsPath,
      signal: controller.signal,
    })
    for (const line of summarize(report)) console.log(line)
    console.log(`synthetic conformance ${report.ok ? 'passed' : 'failed'}: ${report.fixtures.length} fixtures, arms ${report.arms.map(arm => arm.name).join(' and ')}; report ${sanitize(reportPath)}`)
    if (!report.ok) console.error(`runner diagnostics: ${sanitize(diagnosticsPath)}`)
    if (report.cancelled) return 130
    return report.ok ? 0 : 1
  } catch (error) {
    if (!(error instanceof ConformanceSetupError)) throw error
    console.error(`setup failed: ${error.message}`)
    return 2
  } finally {
    process.off('SIGINT', interrupt)
    process.off('SIGTERM', interrupt)
  }
}

if (import.meta.main) process.exitCode = await main(process.argv.slice(2))
