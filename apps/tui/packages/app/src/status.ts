/** Display readings derived from the current session's Harness projections. */
import type { ContextPressureProjection, TokenUsageProjection } from '@deepseek-ai/dsh-token-meter/client'
import type { ContextUsage, TokenTotals } from '@dsh-tui/ui/format.ts'

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
