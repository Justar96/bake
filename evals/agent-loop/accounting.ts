/** Normalize captured provider counters; cache and reasoning are subsets, never extra output. */
export function normalizeWire(api: string, events: readonly Record<string, any>[]) {
  if (events.length === 0) return undefined
  const merged = Object.assign({}, ...events)
  // Chat Completions names the same counters prompt and completion tokens.
  const usage = api === 'openai-completions' ? {
    ...merged, input_tokens: merged.prompt_tokens, output_tokens: merged.completion_tokens,
    input_tokens_details: { cached_tokens: merged.prompt_tokens_details?.cached_tokens ?? merged.prompt_cache_hit_tokens ?? 0 },
    output_tokens_details: merged.completion_tokens_details,
  } : merged
  const numeric = (key: string) => typeof usage[key] === 'number' && Number.isFinite(usage[key]) && usage[key] >= 0
  if (!numeric('input_tokens') || !numeric('output_tokens')) return undefined
  const cacheReadTokens = api === 'anthropic-messages' ? (usage.cache_read_input_tokens ?? 0) : (usage.input_tokens_details?.cached_tokens ?? 0)
  const cacheWriteTokens = api === 'anthropic-messages' ? (usage.cache_creation_input_tokens ?? 0) : (usage.input_tokens_details?.cache_write_tokens ?? 0)
  const inputTokens = api === 'anthropic-messages' ? usage.input_tokens : usage.input_tokens - cacheReadTokens - cacheWriteTokens
  if (inputTokens < 0) return undefined
  const outputTokens = usage.output_tokens
  const componentTotal = inputTokens + cacheReadTokens + cacheWriteTokens + outputTokens
  const totalTokens = usage.total_tokens ?? componentTotal
  const reasoningTokens = usage.output_tokens_details?.reasoning_tokens ?? usage.output_tokens_details?.thinking_tokens ?? null
  const extra = totalTokens - componentTotal
  return {
    inputTokens, outputTokens, cacheReadTokens, cacheWriteTokens, totalTokens,
    reasoningTokens, additionalReasoningTokens: extra,
    reportedTotalConsistent: extra === 0 || (reasoningTokens !== null && extra === reasoningTokens),
  }
}
interface WireRequest {
  status?: number
  usage: Record<string, any>[]
  stream?: { lastEvent?: string | null } | undefined
}

function terminalUsage(api: string, request: WireRequest): boolean {
  if (api !== 'anthropic-messages') return true
  // Anthropic sends initial output in message_start, then one or more
  // cumulative message_delta counters before the terminal message_stop.
  const final = request.usage.at(-1)
  return request.stream?.lastEvent === 'message_stop' && final?.event === 'message_delta'
    && typeof final.output_tokens === 'number' && Number.isFinite(final.output_tokens) && final.output_tokens >= 0
}

/** Reconcile every request, preserving partial counters without treating them as complete cost. */
export function reconcile(api: string, requests: readonly WireRequest[], observed: Record<string, number>) {
  const normalized = requests.map(request => normalizeWire(api, request.usage))
  const complete = requests.length > 0 && requests.every(request => request.status === 200 && terminalUsage(api, request)) && normalized.every(value => value !== undefined && value.reportedTotalConsistent)
  const totals: Record<string, number> = { inputTokens: 0, outputTokens: 0, cacheReadTokens: 0, cacheWriteTokens: 0, totalTokens: 0 }
  for (const usage of normalized) if (usage !== undefined) for (const key of Object.keys(totals)) totals[key] = totals[key]! + (usage[key as keyof typeof usage] as number)
  return {
    complete,
    matches: complete && Object.keys(totals).every(key => totals[key] === observed[key]),
    totals,
    additionalReasoningTokens: normalized.reduce((sum, value) => sum + (value?.additionalReasoningTokens ?? 0), 0),
    reasoningTokens: complete && normalized.every(value => value?.reasoningTokens !== null)
      ? normalized.reduce((sum, value) => sum + value!.reasoningTokens, 0) : null,
  }
}
