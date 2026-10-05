/**
 * Transient-failure classification and cancellable backoff for summary calls,
 * reusing the summarizing provider's own request-retry policy.
 *
 * @module bake-compaction-basic/summary-retry
 */

import {
  CONTEXT_WINDOW_EXCEEDED_CODE,
  LlmError,
  isRetryableFailureCode,
  retryDelayMs,
} from 'bake-llm'
import type { ResolvedRetryPolicy } from 'bake-llm'

/**
 * Most transient retries of one compaction's summary calls. The provider
 * policy may allow fewer; an `always` policy is held to this bound too,
 * because a failed summary leaves the turn's own recovery to continue.
 */
export const MAX_SUMMARY_RETRIES = 3

/** Transient-retry state for one compaction transaction. */
export interface SummaryRetryPlan {
  /** Summarizing provider's resolved request-retry policy. */
  readonly policy: ResolvedRetryPolicy
  /** Aborts every wait when the owning plugin is disposed. */
  readonly lifetime: AbortSignal
  /** Jitter sample; injectable for deterministic tests. */
  readonly random?: () => number
}

/**
 * Whether a summarizer failure reports that its request exceeded the model's
 * context window.
 * @param error - the summarizer's thrown value.
 * @returns whether the failure carries the canonical context-overflow code.
 */
export function isContextOverflow(error: unknown): boolean {
  return errorCode(error) === CONTEXT_WINDOW_EXCEEDED_CODE
}

/**
 * Delay before the next transient retry of a failed summary call, or
 * `undefined` when the failure is not a retryable model-request failure, the
 * retry budget is spent, or the provider asks for a longer wait than its
 * policy accepts.
 * @param plan - the provider policy and lifetime for this transaction.
 * @param error - the summarizer's thrown value.
 * @param retry - one-based number of the retry being considered.
 * @returns the backoff in milliseconds, or `undefined` to stop retrying.
 */
export function summaryRetryDelay(plan: SummaryRetryPlan, error: unknown, retry: number): number | undefined {
  if (!(error instanceof LlmError) || error.code === CONTEXT_WINDOW_EXCEEDED_CODE) return undefined
  const limit = plan.policy.mode === 'always'
    ? MAX_SUMMARY_RETRIES
    : Math.min(plan.policy.maxRetries, MAX_SUMMARY_RETRIES)
  if (retry > limit || !isRetryableFailureCode(plan.policy, error.code)) return undefined
  return retryDelayMs(plan.policy, retry, error.failure, plan.random)
}

/**
 * Wait out one backoff unless the transaction is cancelled or the plugin is
 * disposed first; the timer and listeners never outlive the wait.
 * @param delayMs - backoff in milliseconds.
 * @param signals - transaction cancellation and plugin lifetime.
 * @returns `true` when the full delay elapsed, `false` when a signal aborted it.
 */
export function waitForRetry(delayMs: number, signals: readonly AbortSignal[]): Promise<boolean> {
  const signal = AbortSignal.any([...signals])
  if (signal.aborted) return Promise.resolve(false)
  return new Promise((resolve) => {
    const timer = setTimeout(() => {
      signal.removeEventListener('abort', onAbort)
      resolve(true)
    }, delayMs)
    function onAbort(): void {
      clearTimeout(timer)
      resolve(false)
    }
    signal.addEventListener('abort', onAbort, { once: true })
  })
}

/** Stable code of an `LlmError` or code-bearing Error, if any. */
function errorCode(error: unknown): string | undefined {
  if (error instanceof LlmError) return error.code
  if (typeof error === 'object' && error !== null && 'code' in error) {
    const { code } = error as { code?: unknown }
    return typeof code === 'string' ? code : undefined
  }
  return undefined
}
