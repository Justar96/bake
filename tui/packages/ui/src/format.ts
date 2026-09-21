/** Number formatting for the status line. Pure, locale-independent digits. */

/** Context occupancy as the harness measures it, or absent when unknown. */
export interface ContextUsage {
  /** Tokens the next request would carry. */
  readonly used: number
  /** The exact model's context capacity. */
  readonly window: number
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
 * Render context occupancy as `used/window (percent)`.
 *
 * The percentage rounds down, so a context that is merely close to full never
 * reads as 100%: the one number a user acts on must not overstate itself.
 *
 * @param usage - the occupancy reported by the harness.
 * @returns the status-line fragment.
 */
export function formatContext(usage: ContextUsage): string {
  const percent = usage.window === 0 ? 0 : Math.floor((usage.used / usage.window) * 100)
  return `${formatTokens(usage.used)}/${formatTokens(usage.window)} (${percent}%)`
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
