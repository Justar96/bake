/**
 * Calls and their results folded into one block per action, and a step's
 * calls into one block per step.
 *
 * The session log records a call and its result as two events, and projected
 * one row each they printed as two blocks: the call, and a few rows below it,
 * after any other call made alongside it, the outcome. This fold holds each
 * call until its result arrives and releases the two together, in call order,
 * so each action prints once, finished, where the model made it.
 *
 * Calls are held until their step ends rather than released one by one, so
 * the calls one step made print together: two or more as a `tool-group`, one
 * head over all of them, which reads as the one decision the model made.
 *
 * The block is the same shape from the moment its calls stream to the moment
 * it prints: calls the model is still streaming, calls its message announced
 * but the loop has not dispatched, and calls running or finished are drawn as
 * one block, in the order the model made them. A block that changed shape on
 * the way — separate calls becoming one group, or a group losing the calls
 * not yet dispatched — would give up rows the frame holds blank until history
 * next prints, which leaves a gap over whatever the turn draws next.
 *
 * Pure over the rows it is handed; the application feeds it every committed
 * event's rows, live or replayed, so a resumed session prints the same blocks
 * a live one did.
 *
 * @module @dsh-tui/ui/actions
 */

import type { Row, ToolCallRow } from './rows.ts'

/**
 * Events that release every held call: the end of the step that made them,
 * and, for a log that records no step boundary, the message or turn end that
 * follows them.
 */
export const SETTLES: ReadonlySet<string> = new Set(['step/end', 'assistant/message', 'turn/end'])

/** Held calls, and the fold that releases them. */
export class Actions {
  private readonly held: ToolCallRow[] = []
  /** Calls the step's message made that the loop has not dispatched yet. */
  private announced: readonly ToolCallRow[] = []

  /** Calls not yet released, in call order, as the block they will print as. */
  get pending(): readonly Row[] { return this.live() }

  /**
   * The live region's rows: what is still streaming, then every call of the
   * step not yet released, as the one block they will print as.
   *
   * @param stream - the rows of the attempt still streaming, whose calls join
   *   the block and whose text stays ahead of it, as the message will order them.
   * @returns the rows to draw live.
   */
  live(stream: readonly Row[] = []): readonly Row[] {
    const calls = [...this.held]
    const rest: Row[] = []
    for (const row of [...this.announced, ...stream]) {
      if (row.kind !== 'tool-call') rest.push(row)
      else if (!calls.some(call => call.callId === row.callId)) calls.push(row)
    }
    return [...rest, ...block(calls)]
  }

  /**
   * Hold a place for the calls a committed message made, until each call's
   * own event replaces it or the step ends.
   * @param calls - the message's calls, as `announcedCalls` reads them.
   */
  announce(calls: readonly ToolCallRow[]): void {
    this.announced = calls
  }

  /**
   * Fold one event's rows.
   *
   * A result matching a held call finishes it; a result with no held call
   * passes through as its own row. Every other row passes through unchanged:
   * a command's notice mid-turn prints above the actions still running.
   *
   * @param rows - one event's projected rows.
   * @param settle - release every held call, finished or not, as the end of
   *   a step requires: whatever follows them follows them all. See {@link SETTLES}.
   * @returns the rows to commit, in order.
   */
  fold(rows: readonly Row[], settle = false): readonly Row[] {
    const out: Row[] = settle ? [...block(this.held.splice(0))] : []
    if (settle) this.announced = []
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
    return out
  }
}

/**
 * Calls as the block they print as.
 * @param calls - one step's calls, in order.
 * @returns nothing, the call alone, or a group of them.
 */
function block(calls: readonly ToolCallRow[]): readonly Row[] {
  return calls.length < 2 ? [...calls] : [{ kind: 'tool-group', calls: [...calls] }]
}
