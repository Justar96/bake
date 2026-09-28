/** Bounded, escalating process shutdown for the long-lived CLI surfaces. */

/** Maximum grace allowed for the application tree to dispose before process exit. */
export const PROCESS_SHUTDOWN_TIMEOUT_MS = 5_000

/** Process-exit controller shared by normal completion and Unix signal handlers. */
export interface ProcessShutdown {
  /** Start or join graceful disposal before allowing natural completion with `code`. */
  shutdown(code: number): Promise<void>
  /** Start graceful disposal followed by exit, or force exit when shutdown is already running. */
  interrupt(code: number): void
  /**
   * Start graceful disposal followed by exit, or join a disposal already running without
   * escalating: one terminal close can deliver several SIGHUPs, and a repeat must not cut
   * short the flush the first one started. After disposal has settled, force the exit that
   * natural completion may still be waiting on, since no user remains to interrupt it.
   */
  hangup(code: number): void
}

/**
 * Create one process-exit controller around an application disposer.
 * @param dispose - Whole-application teardown that resolves at quiescence.
 * @param forceExit - Function that exits the process immediately, replaceable by tests.
 * @param complete - Function that records the natural completion code, replaceable by tests.
 * @param timeoutMs - Grace before forced exit, replaceable by tests.
 * @returns A controller whose normal calls coalesce and whose repeated signal call escalates.
 */
export function createProcessShutdown(
  dispose: () => Promise<void>,
  forceExit: (code: number) => void = (code) => { process.exit(code) },
  complete: (code: number) => void = (code) => { process.exitCode = code },
  timeoutMs = PROCESS_SHUTDOWN_TIMEOUT_MS,
): ProcessShutdown {
  let pending: Promise<void> | undefined
  let timeout: ReturnType<typeof setTimeout> | undefined
  let disposed = false
  let completed = false
  let forceExited = false

  const clearExitTimeout = (): void => {
    /* v8 ignore else -- shutdown() arms the timer before any asynchronous exit path can run. */
    if (timeout !== undefined) clearTimeout(timeout)
  }

  const forceExitOnce = (code: number): void => {
    if (forceExited) return
    forceExited = true
    clearExitTimeout()
    forceExit(code)
  }

  const completeOnce = (code: number): void => {
    if (completed || forceExited) return
    completed = true
    clearExitTimeout()
    complete(code)
  }

  const start = (code: number, forceAfterDispose: boolean): Promise<void> => {
    if (pending !== undefined) return pending
    timeout = setTimeout(() => { forceExitOnce(code) }, timeoutMs)
    pending = Promise.resolve().then(dispose).then(
      () => {
        disposed = true
        if (forceAfterDispose) {
          forceExitOnce(code)
        } else {
          completeOnce(code)
          // Natural completion only sets `exitCode`; it still depends on the
          // event loop draining on its own. A handle disposal did not close
          // would otherwise hang the process forever with no bound left
          // armed, so rearm one more forced exit, unref'd so a normal drain
          // still exits immediately rather than waiting out the grace period.
          // Skip it when a concurrent interrupt/hangup already forced the
          // exit: `completeOnce` was then already a no-op, and this would
          // otherwise leave an unnecessary timer running past a process that
          // is already tearing down.
          if (!forceExited) {
            timeout = setTimeout(() => { forceExitOnce(code) }, timeoutMs)
            timeout.unref()
          }
        }
      },
      () => {
        disposed = true
        forceExitOnce(code)
      },
    )
    return pending
  }

  return {
    shutdown(code) {
      return start(code, false)
    },
    interrupt(code) {
      if (pending !== undefined) {
        forceExitOnce(code)
        return
      }
      void start(code, true)
    },
    hangup(code) {
      if (pending === undefined) void start(code, true)
      else if (disposed) forceExitOnce(code)
    },
  }
}
