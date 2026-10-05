/**
 * Check that a release on disk launches before `current` names it.
 *
 * An install checks the release it unpacked, and a rollback the release it
 * returns to, by running that release's own `dsh --self-check`: the command
 * composes its shipped `tui` and `headless` profiles in a private temporary
 * home and imports every module a launch loads, from its plugin rows to the
 * terminal's runner, without starting anything. The release pack runs the
 * same command, so a release that ships passed the check its updates run.
 *
 * The check runs with no TTY and no model key, its Bake home and temporary
 * directory inside a scratch directory removed afterwards, whether the check
 * finished, failed, or was stopped at its time limit.
 *
 * @module bake-updater/verify
 */

import { spawn } from 'node:child_process'
import { existsSync } from 'node:fs'
import { mkdir, mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { compareVersions } from './version.ts'

/** Bound on one launch check. */
export const LAUNCH_CHECK_TIMEOUT_MS = 60_000

/**
 * The newest release whose command has no `--self-check`.
 *
 * A newer updater only installs newer releases, which all have it. Only a
 * rollback can reach one of these, and it was checked when it was installed,
 * so it is held to that same check: its command must report its version.
 */
export const LAST_RELEASE_WITHOUT_SELF_CHECK = '0.3.6'

/** A release's command, relative to its directory. */
export const RELEASE_COMMAND = 'apps/cli/lib/bin.js'

/** How much of each output stream a check keeps for its report. */
const OUTPUT_LIMIT = 64 * 1024

/** What {@link launchProblem} runs. */
export interface LaunchCheck {
  /** Node executable that runs the release's command. */
  readonly node: string
  /** The release directory, the one holding `apps/`. */
  readonly release: string
  /** The version the release must report. */
  readonly version: string
  /** Bound on the check; {@link LAUNCH_CHECK_TIMEOUT_MS} by default. */
  readonly timeoutMs?: number
  /** Cancels the check; the command is stopped and the abort error thrown. */
  readonly signal?: AbortSignal | undefined
}

/**
 * Run a release's launch check.
 *
 * A release newer than {@link LAST_RELEASE_WITHOUT_SELF_CHECK} runs
 * `--self-check`; an older one runs `--version`. Either way it must exit 0
 * and print its version. The command starts as the release launchers start
 * it, with Node's report flags and a diagnostics directory under its Bake
 * home, so it runs directly instead of restarting itself.
 *
 * @param check - the release, its version, and the effects to use.
 * @returns why the release cannot be trusted to launch, or undefined when it can.
 * @throws the abort error when `signal` cancels the check.
 */
export async function launchProblem(check: LaunchCheck): Promise<string | undefined> {
  const command = join(check.release, RELEASE_COMMAND)
  if (!existsSync(command)) return `${RELEASE_COMMAND} is missing`
  const argument = compareVersions(check.version, LAST_RELEASE_WITHOUT_SELF_CHECK) > 0 ? '--self-check' : '--version'
  const timeout = check.timeoutMs ?? LAUNCH_CHECK_TIMEOUT_MS
  const scratch = await mkdtemp(join(tmpdir(), 'bake-launch-check-'))
  try {
    const home = join(scratch, 'home')
    const diagnostics = join(home, 'diagnostics')
    // The check's own temporary files, its private home among them, go here,
    // so removing the scratch directory removes them even after a kill.
    const temporary = join(scratch, 'tmp')
    await mkdir(diagnostics, { recursive: true })
    await mkdir(temporary)
    const result = await runBounded(check.node, [
      '--report-exclude-env', '--report-exclude-network', `--diagnostic-dir=${diagnostics}`, command, argument,
    ], {
      ...process.env, DSH_HOME: home, TMPDIR: temporary, TMP: temporary, TEMP: temporary,
      // A cache written under the scratch directory would be thrown away with it.
      NODE_DISABLE_COMPILE_CACHE: '1',
    }, timeout, check.signal)
    if (result.timedOut) return `no result within ${duration(timeout)}`
    if (result.code !== 0) {
      return failureLine(result.stderr, result.stdout) ?? `it exited ${result.code === null ? 'on a signal' : String(result.code)}`
    }
    if (!result.stdout.trim().split(/\s+/).includes(check.version)) {
      return `it reported ${failureLine(result.stdout) ?? 'nothing'}, not ${check.version}`
    }
    return undefined
  } finally {
    await rm(scratch, { recursive: true, force: true, maxRetries: 3 })
  }
}

/**
 * The line of a failed check's output that says why.
 *
 * Node prints an uncaught error's source line and a caret above its message,
 * so an error line is preferred to the first; Node's own warnings are never
 * the reason.
 * @param outputs - the check's stderr, then its stdout.
 * @returns the line, shortened to 400 characters, or undefined when there is none.
 */
export function failureLine(...outputs: readonly string[]): string | undefined {
  const lines = outputs.join('\n').split(/\r?\n/u).map(line => line.trim())
    .filter(line => line !== '' && !/^\(node:\d+\)/u.test(line) && !line.startsWith('(Use `node --trace-'))
  const line = lines.find(candidate => /^(?:[A-Z]\w*)?Error\b/u.test(candidate)) ?? lines[0]
  if (line === undefined) return undefined
  return line.length > 400 ? `${line.slice(0, 399)}…` : line
}

interface Bounded {
  readonly code: number | null
  readonly stdout: string
  readonly stderr: string
  readonly timedOut: boolean
}

/** Run `command` to completion, killing it once `timeout` passes; settles only after it has exited. */
function runBounded(command: string, args: readonly string[], env: NodeJS.ProcessEnv, timeout: number,
  signal: AbortSignal | undefined): Promise<Bounded> {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, {
      env, stdio: ['ignore', 'pipe', 'pipe'], windowsHide: true, ...signal === undefined ? {} : { signal },
    })
    let stdout = ''
    let stderr = ''
    let timedOut = false
    let failure: Error | undefined
    const settle = (code: number | null): void => {
      clearTimeout(timer)
      if (signal?.aborted === true) reject(failure ?? signal.reason)
      else if (failure !== undefined && child.pid === undefined) resolve({ code: null, stdout, stderr: `Could not run ${command}: ${failure.message}`, timedOut })
      else resolve({ code, stdout, stderr, timedOut })
    }
    child.stdout.setEncoding('utf8').on('data', (chunk: string) => { stdout = (stdout + chunk).slice(0, OUTPUT_LIMIT) })
    child.stderr.setEncoding('utf8').on('data', (chunk: string) => { stderr = (stderr + chunk).slice(0, OUTPUT_LIMIT) })
    const timer = setTimeout(() => {
      timedOut = true
      child.kill('SIGKILL')
      // A process the command started could hold the pipes open past its exit.
      child.stdout.destroy()
      child.stderr.destroy()
    }, timeout)
    child.on('error', (error) => {
      failure = error
      // Nothing started, so no close follows.
      if (child.pid === undefined) settle(null)
    })
    child.on('close', code => settle(code))
  })
}

function duration(milliseconds: number): string {
  return milliseconds < 1000 ? `${milliseconds} ms` : `${Math.round(milliseconds / 1000)} s`
}
