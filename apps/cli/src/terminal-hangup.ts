/** Process stdio after its terminal hangs up. */

/** The process streams a lost terminal breaks. */
export type HangupStdio = Pick<NodeJS.Process, 'stdin' | 'stdout' | 'stderr'>

const tolerant = new WeakSet<object>()
const ignore = (): void => {}

/**
 * Keep the process's own stdio failures from ending shutdown once the terminal is gone.
 *
 * A hung-up terminal fails every later write with EIO, and a closed pipe reader
 * fails it with EPIPE; Node ignores SIGPIPE, so neither kills the process. Node
 * reports the failure as a stream `'error'` event after the write returns. With
 * no listener it becomes an uncaught exception, which the fail-loud guard turns
 * into exit 1 under a 2-second release bound, cutting short the disposal that
 * flushes the session. Nothing remains to read the output, so the errors are
 * dropped. The listeners stay for the rest of the process. Calling this again
 * adds none.
 *
 * Stdin is included because restoring a hung-up terminal's mode can fail and
 * report through stdin, synchronously. Reading `process.stdin` may create the
 * stream, and on a terminal that is already gone that can throw. Each stream is
 * therefore handled on its own.
 * @param stdio - the streams to cover; the current process by default.
 */
export function tolerateLostTerminal(stdio: HangupStdio = process): void {
  for (const name of ['stdout', 'stderr', 'stdin'] as const) {
    try {
      const stream = stdio[name]
      if (tolerant.has(stream)) continue
      tolerant.add(stream)
      stream.on('error', ignore)
    } catch {
      // The stream cannot be opened on the lost terminal, so nothing can fail on it either.
    }
  }
}
