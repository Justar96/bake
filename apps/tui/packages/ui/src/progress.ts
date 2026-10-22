/**
 * Running calls' live state, projected from the runtime's process-local
 * `agent/tool-progress` and `agent/tool-executed` events onto the rows the
 * live region draws.
 *
 * The session log stays the only source of committed rows. This state is
 * held only until each call's `tool/result` commits, and is laid over the
 * held calls when the live region is drawn; `Actions` never sees it, so a
 * committed or replayed block carries none of it.
 *
 * Pure over the events it is handed. The application owns the subscriptions.
 *
 * @module bake-tui-ui/progress
 */

import type { SessionEvent } from 'bake-session'
import { SETTLES } from './actions.ts'
import type { Row, ToolCallLive, ToolCallRow } from './rows.ts'

/**
 * Most lines of a call's output tail kept. The presenter draws at most the
 * result preview's bound of them; this caps the copy held per call.
 */
export const LIVE_TAIL_LINES = 32

/**
 * Split live output into display lines. A carriage return without a line
 * feed rewrites its line in a terminal, as a progress bar does, so each line
 * keeps only what followed its last one.
 * @param output - the newest output, possibly cut mid-line at either end.
 * @returns at most {@link LIVE_TAIL_LINES} lines, oldest first.
 */
export function outputTail(output: string): readonly string[] {
  const lines = output.replace(/\r\n/g, '\n').split('\n').map((line) => {
    const parts = line.split('\r').filter(part => part !== '')
    return parts.at(-1) ?? ''
  })
  if (lines.at(-1) === '') lines.pop()
  return lines.slice(-LIVE_TAIL_LINES)
}

/** Live state of each call still waiting for its logged result, by call id. */
export class CallProgress {
  private readonly calls = new Map<string, ToolCallLive>()

  /** Whether any call has live state to draw. */
  get any(): boolean { return this.calls.size > 0 }

  /**
   * Take a call's newest output snapshot, replacing the last one.
   * @param callId - the call.
   * @param output - its newest output tail.
   */
  progress(callId: string, output: string): void {
    const tail = outputTail(output)
    const current = this.calls.get(callId)
    this.calls.set(callId, { ...current, tail })
  }

  /**
   * Mark a call finished while its result waits for earlier calls to commit.
   * @param callId - the call.
   * @param ok - its outcome before post-execute policy.
   */
  finished(callId: string, ok: boolean): void {
    this.calls.set(callId, { ...this.calls.get(callId), finished: { ok } })
  }

  /**
   * Drop what a committed event supersedes. A call's result replaces its
   * live state; a step's end releases every call.
   * @param event - a committed session event.
   */
  fold(event: SessionEvent): void {
    if (event.type === 'tool/result') this.calls.delete(event.data.message.source.callId)
    else if (SETTLES.has(event.type)) this.calls.clear()
  }

  /** Forget every call, as when the displayed session changes. */
  clear(): void { this.calls.clear() }

  /**
   * Lay live state over the calls in the rows the live region draws. A call
   * that already has its result is left as it is.
   * @param rows - live rows, as `Actions.live` returns them.
   * @returns the same rows, each waiting call carrying its live state.
   */
  decorate(rows: readonly Row[]): readonly Row[] {
    if (this.calls.size === 0) return rows
    const call = (row: ToolCallRow): ToolCallRow => {
      const live = row.result === undefined ? this.calls.get(row.callId) : undefined
      return live === undefined ? row : { ...row, live }
    }
    return rows.map(row => row.kind === 'tool-call' ? call(row)
      : row.kind === 'tool-group' ? { ...row, calls: row.calls.map(call) } : row)
  }
}
