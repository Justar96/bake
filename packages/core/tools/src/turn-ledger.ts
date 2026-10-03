/**
 * Per-turn duplicate-call suppression: the registry remembers model-direct
 * refusals that an unchanged retry would repeat, and answers such a retry
 * without policy or dispatch until another call settles.
 * @module
 */

import type { Session } from '@deepseek-ai/dsh-session'
import type { ToolExecution, ToolExecutionResult, ToolFailure } from './index.ts'

/**
 * Canonical error code for a model-direct call suppressed because an identical
 * call already met a deterministic refusal earlier in the same turn.
 */
export const TOOL_DUPLICATE_CALL = 'DUPLICATE_TOOL_CALL'

/**
 * Failure codes whose unchanged retry must fail the same way until another
 * tool call settles: the filesystem observation policy refuses an unread
 * (`FS_NOT_OBSERVED`) or stale (`FS_STALE_VERSION`) guarded mutation, and only
 * a later observation or mutation can change that verdict. The codes are
 * matched as strings so this package takes no filesystem dependency.
 */
const REPEAT_REFUSAL_CODES: ReadonlySet<string> = new Set(['FS_NOT_OBSERVED', 'FS_STALE_VERSION'])

/**
 * Refusals that an identical retry would repeat, keyed by call identity
 * ({@link repeatKey}), for each Session with an open turn. `turn/start`
 * opens an empty ledger and `turn/end` drops it, so suppression never spans
 * turns and never applies outside one.
 */
export class TurnRefusalLedger {
  private readonly turnRefusals = new WeakMap<Session, Map<string, ToolFailure>>()
  /** Executions answered from {@link turnRefusals} without dispatch. */
  private readonly suppressedExecutions = new WeakSet<ToolExecution>()

  /** Open an empty ledger for the Session's new turn. */
  open(session: Session): void {
    this.turnRefusals.set(session, new Map())
  }

  /** Drop the Session's ledger when its turn ends. */
  close(session: Session): void {
    this.turnRefusals.delete(session)
  }

  /**
   * Answer a model-direct call that repeats an earlier refusal this turn,
   * marking it suppressed so its own settlement leaves the ledger unchanged.
   * Nested transport sub-dispatches are never suppressed: a program's retry
   * loop is its own logic, and its outer result reports what happened.
   * @param exec - the materialized execution about to enter policy.
   * @returns the duplicate-call failure, or undefined when the call should run.
   */
  suppress(exec: ToolExecution): ToolExecutionResult | undefined {
    if (exec.parent !== undefined) return undefined
    const key = repeatKey(exec)
    const prior = key === undefined ? undefined : this.ledgerFor(exec)?.get(key)
    if (prior === undefined) return undefined
    this.suppressedExecutions.add(exec)
    return duplicateCallResult(exec.name, prior)
  }

  /**
   * Update the turn ledger at the commit point of one final result. A
   * repeat-refusal is remembered for a model-direct call; every other settled
   * call, including successes, other failures, and nested sub-dispatches,
   * forgets all remembered refusals because it may have observed or changed
   * the state that decided them. A suppressed duplicate changes nothing.
   * @param exec - the execution whose final result just materialized.
   * @param result - that final result.
   */
  record(exec: ToolExecution, result: ToolExecutionResult): void {
    if (this.suppressedExecutions.has(exec)) return
    const ledger = this.ledgerFor(exec)
    if (ledger === undefined) return
    const code = result.isError ? result.error.info?.code : undefined
    const key = exec.parent === undefined && code !== undefined && REPEAT_REFUSAL_CODES.has(code)
      ? repeatKey(exec)
      : undefined
    if (key === undefined || !result.isError) {
      ledger.clear()
      return
    }
    ledger.set(key, result.error)
  }

  /**
   * The open-turn refusal ledger that governs one execution, if any. Calls
   * without an agent have no turn and are never suppressed.
   */
  private ledgerFor(exec: ToolExecution): Map<string, ToolFailure> | undefined {
    return exec.agent === undefined ? undefined : this.turnRefusals.get(exec.agent.session)
  }
}

/**
 * Identity of one call for repeat detection: the tool name plus its
 * materialized arguments with object keys sorted, so key order never makes
 * two identical calls differ. Undefined when arguments failed to materialize.
 */
function repeatKey(exec: ToolExecution): string | undefined {
  if (exec.arguments === undefined) return undefined
  return JSON.stringify([exec.name, exec.arguments], (_key, value: unknown) => {
    if (value === null || typeof value !== 'object' || Array.isArray(value)) return value
    const record = value as Record<string, unknown>
    return Object.fromEntries(Object.keys(record).sort().map(name => [name, record[name]]))
  })
}

/** Result for a model-direct call suppressed as a repeat of an earlier refusal. */
function duplicateCallResult(name: string, prior: ToolFailure): ToolExecutionResult {
  const message = `not run: this "${name}" call repeats one already refused this turn. Earlier refusal: ${prior.message}`
  return {
    content: [{ type: 'text', text: `Error: ${message}` }],
    isError: true,
    error: { message, info: { name: 'DuplicateToolCallError', code: TOOL_DUPLICATE_CALL } },
  }
}
