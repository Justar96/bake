/**
 * Fold calls and their results into one block per action, and a step's calls
 * into one block per step.
 *
 * The session log records a call and its result as two events. Projected as
 * one row each, they print as two blocks. The call, then the outcome a few
 * rows later, after any other call made alongside it. This fold holds each
 * call until its result arrives and releases the two together, in call order,
 * so each action prints once, finished, where the model made it.
 *
 * Calls stay held until their step ends. They are not released one by one.
 * The calls one step made print together. Two or more are a `tool-group`, with
 * one head over all of them, because they were one model decision.
 *
 * The block keeps the same shape from the first streaming call until it
 * prints. That includes calls the model is still streaming, calls the message
 * announced but the loop has not dispatched, and calls that are running or
 * finished. They stay one block, in the order the model made them. A block
 * that changed shape — separate calls becoming one group, or a group dropping
 * calls not yet dispatched — would give up rows the frame holds blank until
 * history next prints. That leaves a gap above whatever the turn draws next.
 *
 * Pure over the rows it is handed. The application feeds it every committed
 * event's rows, live or replayed, so a resumed session prints the same blocks
 * a live one did.
 *
 * @module @dsh-tui/ui/actions
 */

import type { Row, ToolCallRow } from './rows.ts'

/**
 * Events that release every held call.
 *
 * That is the end of the step that made them. A log with no step boundary
 * uses the following message or turn end instead.
 */
export const SETTLES: ReadonlySet<string> = new Set(['step/end', 'assistant/message', 'turn/end'])

/** Held calls, and the fold that releases them. */
export class Actions {
  private readonly held: ToolCallRow[] = []
  /** Calls the step's message made that the loop has not dispatched yet. */
  private announced: readonly ToolCallRow[] = []

  /** Calls not yet released, in call order, in the block they will print as. */
  get pending(): readonly Row[] { return this.live() }

  /**
   * Rows for the live region. What is still streaming, then every call of the
   * step not yet released, as the one block they will print as.
   *
   * @param stream - rows of the attempt still streaming. Their calls join the
   *   block. Their text stays ahead of it, in the order the message will use.
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
   * Reserve a place for the calls a committed message made, until each call's
   * own event replaces it or the step ends.
   * @param calls - the message's calls, as `announcedCalls` reads them.
   */
  announce(calls: readonly ToolCallRow[]): void {
    this.announced = calls
  }

  /**
   * Fold one event's rows.
   *
   * A result that matches a held call finishes that call. A result with no
   * held call passes through as its own row. Every other row passes through
   * unchanged. A command notice mid-turn prints above the actions still running.
   *
   * @param rows - one event's projected rows.
   * @param settle - release every held call, finished or not. A step end
   *   requires this. Whatever follows the calls follows all of them. See {@link SETTLES}.
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
 * The block one step's calls print as.
 * @param calls - one step's calls, in order.
 * @returns nothing, the call alone, or a group of them.
 */
function block(calls: readonly ToolCallRow[]): readonly Row[] {
  return calls.length < 2 ? [...calls] : [{ kind: 'tool-group', calls: [...calls] }]
}
