/**
 * The terminal palette: one vivid tone per thing an indicator can mean.
 *
 * Colour here is semantic and never decorative. Each tone answers a question the
 * reader asks of a row — is this still running, did it finish, did it fail, is
 * it asking me something, is it waiting on me — and the same tone means the same
 * thing wherever it appears, so a marker, its verb, and the turn header agree
 * without the reader learning a second vocabulary.
 *
 * Saturated orange, green, red, ocean blue, and yellow make the states easy to scan.
 * Text and glyphs still carry each state without colour. Hex values let Ink
 * choose the closest available tone on terminals without truecolour support.
 *
 * The palette is the only place a colour is written. Components name a role,
 * never a hue, which is what keeps a change here from becoming a hunt through
 * the package — and what keeps two roles from silently drifting onto one tone.
 * Syntax colour for code and structured output comes from the highlighter; it
 * says what kind of token a run is rather than what state anything is in; it
 * preserves the semantic tone of diff signs and changed words.
 *
 * @module @dsh-tui/ui/palette
 */

/**
 * The semantic tones of the default terminal theme.
 *
 * Every role is load-bearing: `running`, `done`, `failed`, `asking`, and
 * `waiting` are the five states an indicator can report, and `glint` is the only
 * tone that is movement rather than meaning. A reading such as the cache hit
 * borrows `done`, `waiting`, and `failed` as good, fair, and poor.
 */
export const PALETTE = {
  /** Blue: references, including Markdown headings and links, paths, and informational tool fields. */
  reference: '#60a5fa',
  /**
   * Orange: a turn in progress. The header and its spinner, the marker of an
   * action still running, and manual compaction.
   *
   * Its own hue, because every other tone already answers a different question
   * and "still happening" is none of them.
   */
  running: '#f97316',
  /**
   * Bright yellow: the highlight that crosses the running word.
   *
   * Read only against {@link PALETTE.running}; on its own it means nothing.
   */
  glint: '#fde047',
  /** Green: what finished well. A finished action, a completed turn's summary, and an added line. */
  done: '#22c55e',
  /** Red: what went wrong. A failure, an error, a removed line, and the header of a turn being stopped. */
  failed: '#ef4444',
  /**
   * Ocean blue: what the user is choosing or has staged. The composer's prompt,
   * a selection, staged attachments, and an action in progress on the task list.
   */
  asking: '#0ea5e9',
  /**
   * Yellow: what is waiting on the user. A question, an approval, a picker, queued
   * input, a notice, and a turn that stopped rather than failed.
   */
  waiting: '#eab308',
} as const

/** A colour from the palette, for props that carry one. */
export type PaletteColor = typeof PALETTE[keyof typeof PALETTE]

/** Cache hits from here up are good, and from {@link CACHE_FAIR} up fair. */
export const CACHE_GOOD = 70
/** Cache hits below this are poor: most of the input was billed uncached. */
export const CACHE_FAIR = 30

/**
 * The tone of a cache-hit reading: `done` when the provider served most of the
 * input from its cache, `waiting` when it served some, `failed` when it served
 * little. The percentage beside it says the same without colour.
 * @param hit - the whole-percent hit rate.
 * @returns its palette tone.
 */
export function cacheTone(hit: number): PaletteColor {
  return hit >= CACHE_GOOD ? PALETTE.done : hit >= CACHE_FAIR ? PALETTE.waiting : PALETTE.failed
}

/**
 * Distinguish the access boundary; custom and automatic policies need attention.
 * @param preset - the session's effective permission selection.
 * @returns the access indicator's semantic tone.
 */
export function permissionTone(preset: string): PaletteColor {
  switch (preset) {
    case 'read-only': return PALETTE.reference
    case 'workspace-write': return PALETTE.done
    case 'danger-full-access': return PALETTE.failed
    default: return PALETTE.waiting
  }
}

/**
 * The turn header's colours: the running orange, the glint that sweeps it,
 * and the ramp between them.
 *
 * Named separately because the header pairs the two tones for one effect; the
 * glint is never used on its own. The ramp is the two mixed at even steps, so
 * the shimmer's band brightens and fades through the tones between rather than
 * switching a letter from one to the other: `ramp[0]` is the base and
 * `ramp.at(-1)` the glint.
 */
export const ACCENT = {
  base: PALETTE.running,
  glint: PALETTE.glint,
  ramp: [0, 1, 2, 3].map(step => mix(PALETTE.running, PALETTE.glint, step / 3)),
} as const

/**
 * A colour between two, channel by channel.
 * @param from - `#rrggbb` at 0.
 * @param to - `#rrggbb` at 1.
 * @param amount - how far toward `to`, 0 to 1.
 * @returns the mixed `#rrggbb`.
 */
function mix(from: string, to: string, amount: number): string {
  const channel = (hex: string, index: number): number => Number.parseInt(hex.slice(1 + index * 2, 3 + index * 2), 16)
  return `#${[0, 1, 2].map(index => Math.round(channel(from, index) + (channel(to, index) - channel(from, index)) * amount)
    .toString(16).padStart(2, '0')).join('')}`
}
