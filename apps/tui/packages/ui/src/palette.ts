/**
 * Terminal palette. One tone per state an indicator can report.
 *
 * Colour is semantic, never decorative. The same tone means the same state
 * everywhere, so a marker, its verb, and the turn header do not need a second
 * vocabulary. Saturated orange, green, red, ocean blue, yellow, and royal
 * blue are the six states. `output` only marks a tool result's preview text,
 * and `body` an answer's prose.
 * It says where output is, not what state it is in. Text and glyphs still
 * carry each state without colour. Hex values let Ink pick the closest tone
 * on terminals without truecolour.
 *
 * This module is the only place a colour is written. Components name a role,
 * never a hue, so a change here does not require a search through the package
 * and two roles cannot drift onto one tone. Syntax colour comes from the
 * highlighter. It names the token kind, not a state, and it keeps the semantic
 * tone of diff signs and changed words.
 *
 * @module bake-tui-ui/palette
 */

/**
 * Semantic tones of the default terminal theme.
 *
 * `running`, `done`, `failed`, `asking`, `waiting`, and `compacting` are the
 * six states an indicator can report. `output` marks a result's preview text.
 * A cache-hit reading borrows `waiting` and `failed` as fair and poor; a good
 * one keeps the terminal's own foreground.
 */
export const PALETTE = {
  /** Blue. References, including Markdown headings and links, paths, and informational tool fields. */
  reference: '#60a5fa',
  /** Lavender. Inline code in an answer; surrounding prose is `body`. */
  code: '#c4b5fd',
  /**
   * Orange. A turn in progress. The header and its spinner, and a running
   * action's marker.
   *
   * A separate hue, because every other tone already means something else
   * and none of them means "still happening".
   */
  running: '#f97316',
  /**
   * Royal blue. Context maintenance. The header, its folding spinner, and its
   * word while history is compacted, by `/compact` or automatically inside a
   * turn, and the transcript's notice that the context was compacted.
   *
   * Its own hue, because compaction is neither the turn's work nor a choice
   * the user makes. Deeper than `reference` and `asking`, so it is not read
   * as a link or a selection.
   */
  compacting: '#3b82f6',
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
  /**
   * Light grey. An answer's body text. A step below the terminal's own
   * foreground, so headings, bold words, and the user's own words stand out
   * from it, and a step above `output`, so an answer still reads before a
   * tool result does.
   */
  body: '#cfd3d9',
} as const

/**
 * The install meter's tones. It fills from `running`'s orange to amber, a
 * pale glint sweeps across what is filled, and a comet lights the dark track
 * while a step's length is unknown. The meter's glyphs and the percentage
 * beside it carry the same under `NO_COLOR`.
 */
export const PROGRESS_TONES = { from: PALETTE.running, to: '#fbbf24', glint: '#fff7ed', track: '#44403c' } as const

/**
 * Identity tones for subagents. They are not states: each tells one child
 * apart from its siblings, so the row under the input and the sheet read the
 * same child in the same colour. Hues none of {@link PALETTE}'s states use,
 * so a child's colour is never mistaken for running, done, or failed.
 */
export const AGENT_TONES = ['#a78bfa', '#f472b6', '#2dd4bf', '#a3e635', '#818cf8', '#e879f9'] as const

/**
 * How full the context window is, warming as it fills: soft yellow, yellow,
 * orange, then red. Not states: the status line's context reading takes the
 * step {@link contextTone} picks for its occupancy, and keeps the terminal's
 * own foreground while there is plenty of room. Its own values rather than the
 * state tones, so a filling context is never read as a running turn or a
 * failure; the percentage beside the tone says the same under `NO_COLOR`.
 */
export const CONTEXT_RAMP = ['#fde68a', '#facc15', '#fb923c', '#f87171'] as const

/**
 * The marks of an answer's Markdown, each its own soft tone so a reply reads
 * by its structure: what a list hangs from, what a quote hangs from, a deep
 * heading, a code block's language, and inline code by what it names. None
 * is a state; each mark keeps its glyph or its word under `NO_COLOR`.
 */
export const MARKDOWN = {
  /** Sky. List bullets and numbers. */
  bullet: '#7dd3fc',
  /** Violet. The bar a quote hangs from. */
  quote: '#a78bfa',
  /** Teal. Headings from the third level down. */
  heading: '#5eead4',
  /** Amber. A code block's language, and inline code that is a literal: a number, a string, `true`. */
  literal: '#fbbf24',
} as const

/** A colour from the palette, for props that carry one. */
export type PaletteColor = typeof PALETTE[keyof typeof PALETTE] | typeof AGENT_TONES[number] | typeof CONTEXT_RAMP[number]
  | typeof MARKDOWN[keyof typeof MARKDOWN]

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
 * None when the provider served most of the input from cache, so a healthy
 * reading stays in the terminal's own foreground; `waiting` when it served
 * some, `failed` when it served little. The percentage beside the tone says
 * the same thing without colour.
 * @param hit - whole-percent hit rate.
 * @returns the palette tone, or undefined for the normal foreground.
 */
export function cacheTone(hit: number): PaletteColor | undefined {
  return hit >= CACHE_GOOD ? undefined : hit >= CACHE_FAIR ? PALETTE.waiting : PALETTE.failed
}

/**
 * Context occupancy from here up is worth compacting soon: the status line
 * keeps the reading's absolute count, `(90k/128k)`, among the last things it
 * gives up rather than the first.
 */
export const CONTEXT_WARN = 70
/** Context occupancy from here up is close to the model's limit, and red whatever the compaction mark. */
export const CONTEXT_FULL = 90
/** Where the ramp turns orange when no compaction mark is known. */
export const CONTEXT_HOT = 80
/** Points below the compaction mark where the ramp turns orange. */
export const CONTEXT_LEAD = 10
/** The latest the ramp turns orange, so it stays a step apart from red. */
export const CONTEXT_HOT_MAX = 85
/** Points between the ramp's steps below orange. */
export const CONTEXT_STEP = 10

/**
 * Tone for a context-occupancy percentage, from {@link CONTEXT_RAMP}.
 *
 * No tone while there is plenty of room, so the meter stays in the terminal's
 * own foreground for most of a session. Orange starts {@link CONTEXT_LEAD}
 * points below the compaction mark when one is known (no later than
 * {@link CONTEXT_HOT_MAX}), and at {@link CONTEXT_HOT} otherwise; yellow and
 * soft yellow lead it by {@link CONTEXT_STEP} points each, and red starts at
 * {@link CONTEXT_FULL}. Without a mark: soft yellow from 60%, yellow from 70%,
 * orange from 80%, red from 90%. With a mark at 80%: 50%, 60%, 70%, and 90%.
 * The percentage says the same thing without colour.
 * @param percent - whole-percent occupancy.
 * @param compactAt - whole-percent occupancy at which automatic compaction starts, when known.
 * @returns the ramp's tone, or undefined for the normal foreground.
 */
export function contextTone(percent: number, compactAt?: number): PaletteColor | undefined {
  if (percent >= CONTEXT_FULL) return CONTEXT_RAMP[3]
  const hot = compactAt === undefined ? CONTEXT_HOT : Math.min(CONTEXT_HOT_MAX, compactAt - CONTEXT_LEAD)
  if (percent >= hot) return CONTEXT_RAMP[2]
  if (percent >= hot - CONTEXT_STEP) return CONTEXT_RAMP[1]
  return percent >= hot - 2 * CONTEXT_STEP ? CONTEXT_RAMP[0] : undefined
}

/**
 * How a reasoning effort reads, warming as it asks for more: the light
 * efforts dim, `medium` in the normal foreground, `high` blue, `xhigh`
 * orange, and `max` pink. An effort a provider names otherwise keeps the
 * normal foreground; the word beside the tone says the same under `NO_COLOR`.
 * @param level - the effort id, as the route offers it.
 * @returns the effort's tone, or `dim` for the light ones.
 */
export function thinkingTone(level: string): { readonly color?: PaletteColor, readonly dim?: true } {
  switch (level.toLowerCase()) {
    case 'none': case 'off': case 'minimal': case 'low': return { dim: true }
    case 'high': return { color: PALETTE.asking }
    case 'xhigh': return { color: CONTEXT_RAMP[2] }
    case 'max': return { color: AGENT_TONES[1] }
    default: return {}
  }
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
