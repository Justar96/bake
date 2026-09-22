/**
 * Calls and their results folded into one block per action.
 *
 * The session log records a call and its result as two events, and projected
 * one row each they printed as two blocks: the call, and a few rows below it,
 * after any other call made alongside it, the outcome. This fold holds each
 * call until its result arrives and releases the two together, in call order,
 * so each action prints once, finished, where the model made it.
 *
 * Pure over the rows it is handed; the application feeds it every committed
 * event's rows, live or replayed, so a resumed session prints the same blocks
 * a live one did.
 *
 * @module @dsh-tui/ui/actions
 */

import type { Row } from './rows.ts'

type Call = Extract<Row, { readonly kind: 'tool-call' }>

/** Held calls, and the fold that releases them. */
export class Actions {
  private readonly held: Call[] = []

  /** Calls not yet released, in call order: running, or finished behind one that is. */
  get pending(): readonly Row[] { return this.held }

  /**
   * Fold one event's rows.
   *
   * A result matching a held call finishes it; a result with no held call
   * passes through as its own row. Every other row passes through unchanged:
   * a command's notice mid-turn prints above the actions still running.
   *
   * @param rows - one event's projected rows.
   * @param settle - release every held call first, finished or not, as a
   *   message or a turn's end requires: whatever follows them follows them all.
   * @returns the rows to commit, in order.
   */
  fold(rows: readonly Row[], settle = false): readonly Row[] {
    const out: Row[] = settle ? this.held.splice(0) : []
    for (const row of rows) {
      if (row.kind === 'tool-call') { this.held.push(row); continue }
      if (row.kind === 'tool-result') {
        const index = this.held.findIndex(call => call.callId === row.callId && call.result === undefined)
        if (index >= 0) {
          const { kind: _kind, callId: _callId, ...result } = row
          this.held[index] = { ...this.held[index]!, result }
          continue
        }
      }
      out.push(row)
    }
    const running = this.held.findIndex(call => call.result === undefined)
    out.push(...this.held.splice(0, running === -1 ? this.held.length : running))
    return out
  }
}
