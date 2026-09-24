/** Number formatting for the status line. Pure, locale-independent digits. */

/** Context occupancy as the harness measures it, or absent when unknown. */
export interface ContextUsage {
  /** Provider-anchored estimate of tokens in the next request. */
  readonly used: number
  /** The exact model's context capacity. */
  readonly window: number
}

/**
 * Tokens the provider reported for the session so far, summed across its requests.
 *
 * Only what the provider reports: a session that has made no request has no
 * totals, and a provider that reports no cache traffic has no `cached`, which
 * is not the same as a cache that missed.
 */
export interface TokenTotals {
  /** Prompt tokens sent, whether or not the provider served them from its cache. */
  readonly input: number
  /** Tokens the model generated. */
  readonly output: number
  /** Of `input`, the tokens the provider read from its prompt cache. */
  readonly cached?: number
}

/**
 * The session's token totals as status-line fields: input and output.
 *
 * @param totals - the provider-reported totals.
 * @param words - locale-owned labels for each field.
 * @returns `in 12.3k` and `out 1.2k`.
 */
export function formatTotals(totals: TokenTotals, words: { readonly input: string, readonly output: string }): readonly string[] {
  return [`${words.input} ${formatTokens(totals.input)}`, `${words.output} ${formatTokens(totals.output)}`]
}

/**
 * The share of input the provider's cache served, in whole percent.
 *
 * Rounds down, as occupancy does, so a cache that missed once never reads as
 * a perfect one.
 *
 * @param totals - the provider-reported totals.
 * @returns 0 to 100, or undefined when the provider reports no cache traffic.
 */
export function cacheHit(totals: TokenTotals): number | undefined {
  if (totals.cached === undefined) return undefined
  return totals.input === 0 ? 0 : Math.floor((totals.cached / totals.input) * 100)
}

/**
 * Abbreviate a token count for a status line.
 * @param tokens - a non-negative count.
 * @returns the count with a magnitude suffix, or the exact digits below 1000.
 */
export function formatTokens(tokens: number): string {
  if (tokens < 1_000) return String(tokens)
  // Promote on the ROUNDED magnitude, not the raw one: 999,999 scales to
  // 999.999k, which one decimal place renders as the nonsensical `1000k`.
  const thousands = tokens / 1_000
  if (thousands < 999.95) return `${trim(thousands)}k`
  return `${trim(tokens / 1_000_000)}M`
}

/**
 * Render estimated context occupancy as `~used/window (percent)`.
 *
 * The percentage rounds down, so a context that is merely close to full never
 * reads as 100%: the one number a user acts on must not overstate itself.
 *
 * @param usage - the occupancy reported by the harness.
 * @returns the status-line fragment.
 */
/** Whole-percent occupancy, rounded down for both full and compact readings. */
export function contextPercent(usage: ContextUsage): number {
  return usage.window === 0 ? 0 : Math.floor((usage.used / usage.window) * 100)
}

export function formatContext(usage: ContextUsage): string {
  return `~${formatTokens(usage.used)}/${formatTokens(usage.window)} (${contextPercent(usage)}%)`
}

/**
 * Drop a trailing `.0` so whole magnitudes read as `2k` rather than `2.0k`.
 * @param value - the scaled magnitude.
 * @returns one decimal place, without a redundant zero.
 */
function trim(value: number): string {
  const fixed = value.toFixed(1)
  return fixed.endsWith('.0') ? fixed.slice(0, -2) : fixed
}

/**
 * How long ago something happened, to the coarsest unit that is not zero.
 *
 * For telling sessions apart at a glance, where `3d ago` reads faster than a
 * timestamp and the exact time is one keypress away in the session itself.
 *
 * @param then - epoch milliseconds of the event.
 * @param now - epoch milliseconds to measure from.
 * @param words - locale-owned unit suffixes.
 * @returns `just now`, `5m ago`, `2h ago`, or `3d ago`; a future time reads as now.
 */
export function formatAge(then: number, now: number, words: {
  readonly now: string, readonly minutes: string, readonly hours: string, readonly days: string
}): string {
  const minutes = Math.floor(Math.max(0, now - then) / 60_000)
  if (minutes < 1) return words.now
  if (minutes < 60) return `${minutes}${words.minutes}`
  const hours = Math.floor(minutes / 60)
  return hours < 24 ? `${hours}${words.hours}` : `${Math.floor(hours / 24)}${words.days}`
}
