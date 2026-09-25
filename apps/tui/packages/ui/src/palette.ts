/**
 * Terminal palette. One tone per state an indicator can report.
 *
 * Colour is semantic, never decorative. The same tone means the same state
 * everywhere, so a marker, its verb, and the turn header do not need a second
 * vocabulary. Saturated orange, green, red, ocean blue, and yellow are the
 * five states. `output` only marks a tool result's preview text. It says
 * where output is, not what state it is in. Text and glyphs still
 * carry each state without colour. Hex values let Ink pick the closest tone
 * on terminals without truecolour.
 *
 * This module is the only place a colour is written. Components name a role,
 * never a hue, so a change here does not require a search through the package
 * and two roles cannot drift onto one tone. Syntax colour comes from the
 * highlighter. It names the token kind, not a state, and it keeps the semantic
 * tone of diff signs and changed words.
 *
 * @module @dsh-tui/ui/palette
 */

/**
 * Semantic tones of the default terminal theme.
 *
 * `running`, `done`, `failed`, `asking`, and `waiting` are the five states an
 * indicator can report. `output` marks a result's preview text.
 * A cache-hit reading borrows `done`, `waiting`, and `failed` as good, fair,
 * and poor.
 */
export const PALETTE = {
  /** Blue. References, including Markdown headings and links, paths, and informational tool fields. */
  reference: '#60a5fa',
  /**
   * Orange. A turn in progress. The header and its spinner, a running
   * action's marker, and manual compaction.
   *
   * A separate hue, because every other tone already means something else
   * and none of them means "still happening".
   */
  running: '#f97316',
  /** Green. What finished well. A finished action, a completed turn's summary, and an added line. */
  done: '#22c55e',
  /** Red. What went wrong. A failure, an error, a removed line, and the header of a turn being stopped. */
  failed: '#ef4444',
  /**
   * Ocean blue. What the user is choosing or has staged. The composer's prompt,
   * a selection, staged attachments, and an action in progress on the task list.
   */
  asking: '#0ea5e9',
  /**
   * Yellow. What is waiting on the user. A question, an approval, a picker, queued
   * input, a notice, and a turn that stopped without failing.
   */
  waiting: '#eab308',
  /**
   * Soft grey. Plain text in a tool result's preview. Quieter than an answer,
   * so output stays supporting material, but brighter than the terminal's dim,
   * which reasoning and metadata use.
   */
  output: '#b4b8bf',
} as const

/** A colour from the palette, for props that carry one. */
export type PaletteColor = typeof PALETTE[keyof typeof PALETTE]

/** Cache hits from here up are good, and from {@link CACHE_FAIR} up fair. */
export const CACHE_GOOD = 70
/** Cache hits below this are poor. Most of the input was billed uncached. */
export const CACHE_FAIR = 30

/**
 * Tone for a cache-hit percentage.
 *
 * `done` when the provider served most of the input from cache, `waiting`
 * when it served some, `failed` when it served little. The percentage beside
 * the tone says the same thing without colour.
 * @param hit - whole-percent hit rate.
 * @returns the palette tone.
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
