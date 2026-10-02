/**
 * Live-output polling for foreground runs, shared by the bash and pwsh
 * executors so both honor {@link ShellExecSpec.onOutput} the same way.
 * @module @deepseek-ai/dsh-shell/watch
 */

import type { SubprocessOutputReader } from '@deepseek-ai/dsh-subprocess'
import type { ShellExecSpec } from './types.ts'

/** How often a running command's captured streams are read for live output. */
export const LIVE_OUTPUT_POLL_MS = 200

/** Longest tail passed to {@link ShellExecSpec.onOutput}. */
export const LIVE_OUTPUT_MAX_CHARS = 4096

/**
 * Poll a foreground run's captured streams and pass the growing tail of
 * their text to `onOutput`. Each poll appends new stdout, then new stderr,
 * so the two interleave at poll granularity. A throwing receiver is
 * contained. The returned function stops polling; after it returns no call
 * is made.
 * @param streams - the run's collect-mode readers, read from their start.
 * @param onOutput - the spec's receiver; absent, nothing polls.
 * @param intervalMs - poll period.
 * @returns a stop function, idempotent.
 */
export function watchOutput(
  streams: { readonly stdout: SubprocessOutputReader; readonly stderr: SubprocessOutputReader },
  onOutput: ShellExecSpec['onOutput'],
  intervalMs: number = LIVE_OUTPUT_POLL_MS,
): () => void {
  if (onOutput === undefined) return () => {}
  let stdoutOffset = 0
  let stderrOffset = 0
  let tail = ''
  let stopped = false
  const poll = (): void => {
    if (stopped) return
    const out = streams.stdout.readFrom(stdoutOffset)
    const err = streams.stderr.readFrom(stderrOffset)
    stdoutOffset = out.nextOffset
    stderrOffset = err.nextOffset
    const delta = out.text + err.text
    if (delta === '') return
    tail = (tail + delta).slice(-LIVE_OUTPUT_MAX_CHARS)
    try {
      onOutput(tail)
    } catch {
      // Display-only: a receiver failure must not affect the command.
    }
  }
  const timer = setInterval(poll, intervalMs)
  return () => {
    stopped = true
    clearInterval(timer)
  }
}
