/** Display readings derived from the current session's Harness projections. */
import type { ContextPressureProjection, TokenUsageProjection } from '@deepseek-ai/dsh-token-meter/client'
import type { ContextUsage, TokenTotals } from '@dsh-tui/ui/format.ts'
import type { GoalEntry } from '@dsh-tui/ui/app.tsx'
import type { GoalView } from '@deepseek-ai/dsh-goal'

/**
 * Map the goal service view onto the header's display entry.
 *
 * Returns undefined when no goal is current. `armed` is this process's
 * activation flag, not a field of the durable projection.
 */
export function goalFor(goal: GoalView | undefined): GoalEntry | undefined {
  if (goal === undefined) return undefined
  return {
    objective: goal.objective, phase: goal.phase, armed: goal.activation === 'armed',
    rounds: goal.roundsStarted, maxRounds: goal.maxGoalRounds,
    ...goal.blockedReason === undefined ? {} : { blocked: goal.blockedReason.message },
  }
}

/** Never pair an earlier model's usage with the selected model or a newer request's capacity. */
export function contextFor(pressure: ContextPressureProjection | undefined, model: string): ContextUsage | undefined {
  if (pressure?.projectedTokens === undefined || pressure.contextWindow === undefined
    || pressure.contextWindow !== pressure.sampledContextWindow) return undefined
  const matches = (route: ContextPressureProjection['sampledRoute']) => route !== undefined
    && `${route.provider}/${route.model}` === model
  if (!matches(pressure.sampledRoute) || !matches(pressure.requestRoute)) return undefined
  return { used: pressure.projectedTokens, window: pressure.contextWindow }
}

/** Session totals are cumulative, not a reading of the last request or model. */
export function usageFor(usage: TokenUsageProjection | undefined): TokenTotals | undefined {
  if (usage === undefined) return undefined
  const input = usage.uncachedInputTokens + usage.cacheReadTokens + usage.cacheWriteTokens
  if (input + usage.outputTokens === 0) return undefined
  return { input, output: usage.outputTokens,
    ...usage.cacheReadTokens + usage.cacheWriteTokens === 0 ? {} : { cached: usage.cacheReadTokens } }
}
