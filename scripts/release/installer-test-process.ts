import { spawnSync } from 'node:child_process'

export const INSTALLER_WATCHDOG_MS = 10_000
export const INSTALLER_DRAIN_MS = 10_000

export interface InstallerProcessResult {
  readonly code: number
  readonly signal: string | null
  readonly timedOut: boolean
  readonly aborted: boolean
  readonly elapsedMs: number
  readonly stdout: string
  readonly stderr: string
  readonly cleanupErrors: readonly string[]
}

/** Own the preflight process, its descendants, and both output streams through test teardown. */
export function installerTestProcess(
  argv: string[],
  options: { env?: NodeJS.ProcessEnv; timeoutMs?: number; onStdout?: (text: string) => void } = {},
): { done: Promise<InstallerProcessResult>; stop: (reason: 'timeout' | 'test-aborted') => Promise<InstallerProcessResult> } {
  const started = performance.now()
  const timeoutMs = options.timeoutMs ?? INSTALLER_WATCHDOG_MS
  const child = Bun.spawn(argv, {
    env: options.env ?? process.env, stdin: 'ignore', stdout: 'pipe', stderr: 'pipe', detached: process.platform !== 'win32',
  })
  let timedOut = false
  let aborted = false
  let complete = false
  let stopping = false
  const cleanupErrors: string[] = []
  const readers = [child.stdout.getReader(), child.stderr.getReader()] as const
  const read = async (index: 0 | 1): Promise<string> => {
    const chunks: Uint8Array[] = []
    try {
      for (;;) {
        const next = await readers[index].read()
        if (next.done) break
        chunks.push(next.value)
        if (index === 0) options.onStdout?.(Buffer.concat(chunks).toString('utf8'))
      }
    } catch (error) {
      cleanupErrors.push(`reading ${index === 0 ? 'stdout' : 'stderr'}: ${String(error)}`)
    }
    return Buffer.concat(chunks).toString('utf8')
  }
  let drainTimer: ReturnType<typeof setTimeout> | undefined
  let cancelledReaders: Promise<unknown> = Promise.resolve()
  const exited = child.exited.catch((error) => { cleanupErrors.push(`waiting for exit: ${String(error)}`); return -1 })
  const done = Promise.all([exited, read(0), read(1)]).then(async ([code, stdout, stderr]) => {
    await cancelledReaders
    return {
      code, stdout, stderr, signal: child.signalCode, timedOut, aborted, elapsedMs: Math.round(performance.now() - started), cleanupErrors,
    }
  }).finally(() => {
    complete = true
    clearTimeout(timer)
    clearTimeout(drainTimer)
  })
  const stop = (reason: 'timeout' | 'test-aborted'): Promise<InstallerProcessResult> => {
    if (complete || stopping) return done
    stopping = true
    timedOut = reason === 'timeout'
    aborted = reason === 'test-aborted'
    clearTimeout(timer)
    // Never signal a PID after observing its exit. Windows /T includes the native Node child.
    if (child.exitCode === null) {
      if (process.platform === 'win32') {
        const killed = spawnSync('taskkill', ['/PID', String(child.pid), '/T', '/F'], {
          stdio: 'pipe', windowsHide: true, timeout: INSTALLER_DRAIN_MS,
        })
        if (killed.error !== undefined || killed.status !== 0) {
          cleanupErrors.push(`taskkill: ${killed.error?.message ?? `exit ${killed.status}, signal ${killed.signal}`}; ${killed.stderr.toString()}`)
          try { child.kill('SIGKILL') } catch (error) { cleanupErrors.push(`parent kill: ${String(error)}`) }
        }
      } else {
        try { process.kill(-child.pid, 'SIGKILL') } catch (error) {
          if ((error as NodeJS.ErrnoException).code !== 'ESRCH') cleanupErrors.push(`process group kill: ${String(error)}`)
        }
      }
    } else {
      cleanupErrors.push('the parent exited while a descendant still held its output; its PID was not signalled')
    }
    drainTimer = setTimeout(() => {
      cleanupErrors.push(`output did not close within ${INSTALLER_DRAIN_MS} ms after stopping; cleanup is incomplete`)
      cancelledReaders = Promise.all(readers.map(reader => reader.cancel().catch(error => cleanupErrors.push(`output cancellation: ${String(error)}`))))
    }, INSTALLER_DRAIN_MS)
    return done
  }
  const timer = setTimeout(() => { void stop('timeout') }, timeoutMs)
  return { done, stop }
}
