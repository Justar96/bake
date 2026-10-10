/**
 * Owned child launches for the native eval fixture adapter. `launch()` runs
 * one trusted executable with a complete environment, bounded output, a
 * timeout, and cancellation, and resolves only after the child has closed.
 * Moved from the retired synthetic conformance driver (D32), which used the
 * same launcher for its arms.
 */

import { spawn, type ChildProcessByStdio } from 'node:child_process'
import { join } from 'node:path'
import type { Readable, Writable } from 'node:stream'

/** Bytes kept from stdout; more stops the child as an overflow. */
export const STDOUT_LIMIT = 512 * 1024
/** Bytes kept from stderr; more stops the child as an overflow. */
export const STDERR_LIMIT = 64 * 1024
/** The largest budget a Node timer holds without firing at once. */
export const MAX_TIMEOUT_MS = 2 ** 31 - 1

/** The minimal child environment: a search path and private homes, nothing inherited beyond them. */
export function childEnvironment(home: string, temporary: string): Record<string, string> {
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
 * only after the child closes and its stdin settles, including after an
 * asynchronous spawn error, and never starts a child once `signal` has aborted.
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
    let pending = 2
    const settle = (): void => {
      if (--pending === 0) resolvePromise(state)
    }
    // The end callback receives the same error.
    child.stdin.on('error', () => {})
    // The runtime destroys stdin when the child exits, which can cancel a
    // queued write without an `error` event and after `close`; only the end
    // callback reports every outcome.
    child.stdin.end(input, (error?: Error | null) => {
      // A spawn failure also closes stdin; that is reported as the spawn error.
      if (error && child.pid !== undefined) {
        state.stdinError = errorCode(error)
        state.errors.push(error.message)
      }
      settle()
    })
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
      settle()
    })
  })
}
