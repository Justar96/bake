/**
 * Keyless fixture adapter for a native eval arm. It builds a scenario's real
 * fixture with `scenarios.ts`, sends the scenario's exact prompt bytes on
 * stdin, and judges the workspace with the evaluator's own `validate()`. No
 * model, proxy, or session log is involved, and nothing here measures
 * requests, tokens, or usage.
 *
 * Process ownership belongs to `launch()` in `launch.ts`: timeout,
 * output caps, cancellation, and stopping the child. Arms are trusted single
 * processes. The private root and minimal environment keep runs apart; they
 * do not contain a hostile executable, and a descendant that outlives a
 * normal exit is not stopped.
 */
import { createHash } from 'node:crypto'
import { mkdir, mkdtemp, rm, stat } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { childEnvironment, launch, MAX_TIMEOUT_MS, type Launch } from './launch.ts'
import { fixture, prompts, validate, type Verdict } from './scenarios.ts'

/** Scenarios whose verdict rests only on the exit code and evaluator-observed files. */
export const NATIVE_FIXTURE_CASES = ['ordinary_edit'] as const
export type NativeFixtureCase = (typeof NATIVE_FIXTURE_CASES)[number]

export interface NativeFixtureOptions {
  /** The arm's executable and arguments. The executable must be an existing file; a relative one resolves against this process's cwd. Arguments pass verbatim. */
  argv: readonly string[]
  scenario: NativeFixtureCase
  /** Budget before the arm is stopped: an integer from 1 to {@link MAX_TIMEOUT_MS}. */
  timeoutMs: number
  /** Parent of the private root; the default is the OS temporary directory. */
  tempRoot?: string
  /** Stops the arm when aborted; an already aborted signal starts no arm. */
  signal?: AbortSignal
}

export interface NativeFixtureRun {
  scenario: NativeFixtureCase
  /** Process facts from `launch()`: exit, signal, timeout, cancellation, overflow, byte counts, and spawn, stdin, or stop error codes. */
  process: Omit<Launch, 'stdout' | 'stderr' | 'errors'>
  /** SHA-256 of the exact bytes sent on stdin. */
  promptSha256: string
  /** Bounded by the driver's stream caps; diagnostics only, never judged. */
  stdout: string
  stderr: string
  /** The evaluator's verdict on the workspace after the arm closed. */
  verdict: Verdict
  /** A clean exit 0 with no process fault, and a validated verdict. */
  success: boolean
}

/**
 * Run one arm over one fixture scenario in a private root under `tempRoot`,
 * holding `workspace/`, `home/`, and `tmp/`. The arm runs in `workspace/`
 * with the driver's minimal environment, and the evaluator's `node test.cjs`
 * check gets the same environment, never this process's.
 *
 * Throws before creating anything or starting a process when the scenario,
 * argv, or budget is invalid, or the executable is not an existing file.
 * Throws when the fixture cannot be built or judged. The private root is
 * removed before the call settles, after the arm has closed.
 * @param options - the arm, scenario, budget, and owned paths.
 */
export async function runNativeFixture(options: NativeFixtureOptions): Promise<NativeFixtureRun> {
  const { scenario, timeoutMs, signal } = options
  if (!(NATIVE_FIXTURE_CASES as readonly string[]).includes(scenario)) throw new Error(`scenario ${String(scenario)} is not a native fixture case`)
  if (!Number.isInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > MAX_TIMEOUT_MS) throw new Error(`timeout must be from 1 to ${MAX_TIMEOUT_MS} whole milliseconds`)
  const argv = options.argv as unknown
  if (!Array.isArray(argv) || argv.length === 0 || !argv.every(arg => typeof arg === 'string' && !arg.includes('\0'))) {
    throw new Error('argv must be a non-empty list of strings without NUL bytes')
  }
  // Resolved here, because the arm starts in its workspace.
  const executable = resolve(argv[0] as string)
  if ((await stat(executable).catch(() => undefined))?.isFile() !== true) throw new Error(`executable ${executable} is not an existing file`)

  const input = Buffer.from(prompts[scenario]!, 'utf8')
  const root = await mkdtemp(join(resolve(options.tempRoot ?? tmpdir()), 'bake-native-eval-'))
  try {
    const home = join(root, 'home')
    const temporary = join(root, 'tmp')
    await mkdir(home)
    await mkdir(temporary)
    const built = fixture(root, scenario)
    const env = childEnvironment(home, temporary)
    const result = await launch([executable, ...argv.slice(1) as string[]], built.workspace, env, input, timeoutMs, signal)
    const { stdout, stderr, errors: _errors, ...facts } = result
    const final = stdout.toString('utf8')
    // Tool counts are placeholders: no allowlisted predicate reads them.
    const verdict = validate(scenario, built, { code: result.exitCode, final, toolCalls: 0, subagentCalls: 0, injectionPath: join(root, 'injection.json') }, env)
    const success = result.exitCode === 0 && result.signal === null && !result.timedOut && !result.cancelled
      && !result.stdoutOverflow && !result.stderrOverflow
      && result.spawnError === undefined && result.stdinError === undefined && result.stopError === undefined
      && verdict.validated
    return {
      scenario, process: facts, promptSha256: createHash('sha256').update(input).digest('hex'),
      stdout: final, stderr: stderr.toString('utf8'), verdict, success,
    }
  } finally {
    await rm(root, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 })
  }
}
