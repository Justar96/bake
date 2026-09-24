/**
 * Pure client-safe token-projection vocabulary.
 *
 * @module @deepseek-ai/dsh-token-meter/projection
 */

/**
 * Durable cumulative provider usage for a complete session log.
 *
 * The four buckets are disjoint. In particular, reasoning tokens are already
 * included in `outputTokens` and are not accumulated again.
 */
export interface TokenUsageProjection {
  uncachedInputTokens: number
  outputTokens: number
  cacheReadTokens: number
  cacheWriteTokens: number
}

/**
 * Approximate context occupancy for a status display.
 *
 * The fields, when present, are not one atomic request observation: each is a
 * last-wins record of a different moment. On a model switch a new capacity can
 * precede the new route's usage. Compare both routes and capacities with the
 * displayed model before showing a ratio; usage is still estimated within
 * one route. The value is a reference, not billing or gating input.
 */
export interface ContextPressureProjection {
  /**
   * Provider-reported prompt size of the most recent request: uncached input
   * plus cache reads and writes. Response output is excluded, so this does not
   * grow as the current turn streams. Absent until a provider reports usage.
   */
  pressureTokens?: number
  /**
   * What the NEXT request's prompt would cost: {@link pressureTokens} plus the
   * heuristic repricing of everything the surface gained or lost since that
   * sample. Only the delta is estimated, so the figure stays anchored to the
   * provider while still reacting the moment a compaction shadows a span —
   * which `pressureTokens` alone cannot do, since compaction reports no usage
   * of its own. Absent until a provider reports usage.
   */
  projectedTokens?: number
  /** Newest recorded route capacity; absent when no adapter advertised one. */
  contextWindow?: number
  /** Capacity when the newest usage sample was taken; absent before a known-capacity sample. */
  sampledContextWindow?: number
  /** Route whose request is currently being prepared or was last sent. */
  requestRoute?: { readonly provider: string; readonly model: string }
  /** Route that supplied the newest provider usage sample; absent before one can be attributed. */
  sampledRoute?: { readonly provider: string; readonly model: string }
}

/**
 * Heuristic composition of the next request's context: what the prompt is
 * made of, not what it costs. All three figures use the meter's fixed
 * density estimate, so they will not sum to the provider-anchored
 * `projectedTokens`: the estimator systematically underprices CJK text and
 * JSON schemas, which is exactly the error the anchoring in
 * {@link ContextPressureProjection.projectedTokens} keeps out of the occupancy
 * figure. Present these as approximations of composition, never as a total.
 */
export interface ContextBreakdownProjection {
  /** Heuristic tokens of the last nonempty surviving system prompt in surface order; 0 when none exists. */
  systemTokens: number
  /** Heuristic tokens of the newest request envelope's tool schemas; 0 before any request. */
  toolsTokens: number
  /** Heuristic tokens of every other visible surface node, including superseded system prompts. */
  messageTokens: number
}

declare module '@deepseek-ai/dsh-session-projection/types' {
  interface SessionProjectionMap {
    /** Provider-reported usage accumulated across the complete durable log. */
    tokenUsage: TokenUsageProjection
    /** Newest request pressure paired with the newest known route capacity. */
    contextPressure: ContextPressureProjection
    /** Heuristic system/tools/message composition of the next request. */
    contextBreakdown: ContextBreakdownProjection
  }
}
