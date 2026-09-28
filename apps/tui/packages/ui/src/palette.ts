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

/**
 * How far a release has baked while Bake installs it, from pale dough to dark
 * crust. The loaf takes the tone for its progress, so colour reads as how
 * much is done, and the percentage beside it carries the same under `NO_COLOR`.
 */
export const CRUST: readonly string[] = ['#f5e6c4', '#f0d49a', '#e8ba68', '#dc9c3f', '#c97f2a', '#ab621f', '#8b4a17']

/**
 * Identity tones for subagents. They are not states: each tells one child
 * apart from its siblings, so the row under the input and the sheet read the
 * same child in the same colour. Hues none of {@link PALETTE}'s states use,
 * so a child's colour is never mistaken for running, done, or failed.
 */
export const AGENT_TONES = ['#a78bfa', '#f472b6', '#2dd4bf', '#a3e635', '#818cf8', '#e879f9'] as const

/** A colour from the palette, for props that carry one. */
export type PaletteColor = typeof PALETTE[keyof typeof PALETTE] | typeof AGENT_TONES[number]

/**
 * A child's identity tone, by its place in the catalog. The catalog appends
 * children, so a child keeps its colour for the session.
 * @param index - the child's index in catalog order.
 * @returns its tone; the tones repeat past the sixth child.
 */
export function agentTone(index: number): PaletteColor {
  return AGENT_TONES[((index % AGENT_TONES.length) + AGENT_TONES.length) % AGENT_TONES.length]!
}

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

/** Context occupancy from here up is worth compacting soon. */
export const CONTEXT_WARN = 70
/** Context occupancy from here up is close to the model's limit. */
export const CONTEXT_FULL = 90

/**
 * Tone for a context-occupancy percentage.
 *
 * No tone while there is room, so the meter stays in the terminal's own
 * foreground for most of a session. `waiting` once compacting is worth
 * considering and `failed` near the limit. The percentage says the same
 * thing without colour.
 * @param percent - whole-percent occupancy.
 * @returns the palette tone, or undefined for the normal foreground.
 */
export function contextTone(percent: number): PaletteColor | undefined {
  return percent >= CONTEXT_FULL ? PALETTE.failed : percent >= CONTEXT_WARN ? PALETTE.waiting : undefined
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
