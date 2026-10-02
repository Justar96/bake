/**
 * Coalesces one tool call's process-local progress snapshots so a chatty
 * body cannot flood `agent/tool-progress` listeners. Nothing here is logged
 * or reaches a model request.
 * @module dsh-agent-loop/tool-progress
 */

import { TOOL_PROGRESS_INTERVAL_MS, TOOL_PROGRESS_MAX_CHARS, type ToolProgress } from '@deepseek-ai/dsh-agent'

/**
 * Keep the tail of a snapshot's output within {@link TOOL_PROGRESS_MAX_CHARS},
 * never starting on the second half of a surrogate pair.
 * @param progress - a snapshot the registry already checked carries a string output.
 * @returns the bounded snapshot.
 */
export function boundProgress(progress: ToolProgress): ToolProgress {
  const { output } = progress
  if (output.length <= TOOL_PROGRESS_MAX_CHARS) return { output }
  let tail = output.slice(-TOOL_PROGRESS_MAX_CHARS)
  const first = tail.charCodeAt(0)
  if (first >= 0xdc00 && first <= 0xdfff) tail = tail.slice(1)
  return { output: tail }
}

/**
 * Publishes a call's newest snapshot at most once per interval. The first
 * snapshot publishes at once; later ones within the interval replace one
 * pending snapshot, which a single timer publishes when the interval ends.
 * {@link close} cancels that timer and drops the pending snapshot, after
 * which nothing publishes: the call's result replaces whatever was shown.
 */
export class ProgressThrottle {
  private last = Number.NEGATIVE_INFINITY
  private pending: ToolProgress | undefined
  private timer: ReturnType<typeof setTimeout> | undefined
  private closed = false

  /**
   * @param publish - emits one snapshot; listener failures are the emitter's to contain.
   * @param intervalMs - shortest gap between two publications.
   * @param now - monotonic clock in milliseconds.
   */
  constructor(
    private readonly publish: (progress: ToolProgress) => void,
    private readonly intervalMs: number = TOOL_PROGRESS_INTERVAL_MS,
    private readonly now: () => number = () => performance.now(),
  ) {}

  /**
   * Offer the newest snapshot. Dropped after {@link close}.
   * @param progress - the snapshot, replacing any still pending.
   */
  push(progress: ToolProgress): void {
    if (this.closed) return
    const snapshot = boundProgress(progress)
    const wait = this.last + this.intervalMs - this.now()
    if (wait <= 0 && this.timer === undefined) {
      this.flush(snapshot)
      return
    }
    this.pending = snapshot
    this.timer ??= setTimeout(() => {
      this.timer = undefined
      const next = this.pending
      this.pending = undefined
      if (next !== undefined && !this.closed) this.flush(next)
    }, Math.max(0, wait))
  }

  /** Stop publishing: cancel the timer and drop any pending snapshot. Idempotent. */
  close(): void {
    this.closed = true
    this.pending = undefined
    if (this.timer !== undefined) clearTimeout(this.timer)
    this.timer = undefined
  }

  private flush(progress: ToolProgress): void {
    this.last = this.now()
    this.publish(progress)
  }
}
