/**
 * What the agent is doing, as one line that holds for the whole turn.
 *
 * The live region shows the output a turn produces; this module names the
 * turn itself. Its word is chosen once when the turn starts and kept until it
 * ends, so the line reads as a steady header rather than one more thing
 * changing under the reader. What does change — the phase, the elapsed time,
 * the newest line of reasoning — changes in place, on rows that do not move.
 *
 * Everything here is pure: the caller owns the clock.
 *
 * @module @dsh-tui/ui/activity
 */

import type { Row } from './rows.ts'
import type { TuiCopy } from './copy.ts'
import { PAST, VERB, type Verb } from './layout.ts'
import { verbFor } from './present.ts'

/** The source of time an animated surface reads, supplied by the terminal owner. */
export interface Clock {
  /** Milliseconds since an arbitrary fixed origin. */
  readonly now: () => number
  /**
   * Call `tick` every `ms` milliseconds until the returned function is called.
   * The caller disposes it on unmount, so no tick arrives after teardown.
   */
  readonly every: (ms: number, tick: () => void) => () => void
}

/** What the turn is doing right now, as far as the rows on screen say. */
export type Phase =
  | { readonly kind: 'thinking' }
  | { readonly kind: 'writing' }
  | { readonly kind: 'running', readonly tool: string }

/**
 * Spinner frames, played forward and back.
 *
 * Each sits alone in a two-cell rail, the one place the layout accepts a
 * character above 0x7f (`MARKER` in `layout.ts`): a terminal that draws one
 * wider than measured shifts nothing but the rail. U+2733 is left out because
 * it has an emoji presentation that some terminals draw two cells wide even
 * without a variation selector.
 */
export const SPINNER = ['·', '✢', '✶', '✻', '✽'] as const

/** The frame shown when nothing animates: a screen reader, or no clock. */
export const SPINNER_REST = '✻'

/** Milliseconds between spinner frames. */
export const SPINNER_MS = 120

/**
 * The turn header's colours: a warm crust, and the glint that sweeps across it.
 *
 * Its own hue, because every named colour already means something here —
 * yellow is input waiting on the user, cyan a question, green and red an
 * outcome — and the header means none of them. A terminal with fewer colours
 * gets the nearest it has; `NO_COLOR` gets none.
 */
export const ACCENT = { base: '#d7875f', glint: '#ffd7af' } as const

/**
 * Characters on either side of the glint's centre it also lights.
 *
 * One lit character reads as a flicker; a band of three reads as light
 * moving across the word.
 */
const GLINT_RADIUS = 1

/**
 * Characters of the word the glint lights at an elapsed time.
 *
 * The glint crosses the word left to right, one character per spinner frame,
 * starting and ending off the word so each pass begins with the word at rest.
 *
 * @param length - characters in the word, ellipsis included.
 * @param elapsed - milliseconds since the turn started.
 * @returns whether each character is lit.
 */
export function glint(length: number, elapsed: number): readonly boolean[] {
  const span = length + GLINT_RADIUS * 2 + 4
  const centre = Math.floor(Math.max(0, elapsed) / SPINNER_MS) % span - GLINT_RADIUS - 2
  return Array.from({ length }, (_, index) => Math.abs(index - centre) <= GLINT_RADIUS)
}

/**
 * Pick the spinner glyph for an elapsed time, bouncing through the frames.
 * @param elapsed - milliseconds since the turn started.
 * @returns the glyph for that moment.
 */
export function spinnerFrame(elapsed: number): string {
  const cycle = SPINNER.length * 2 - 2
  const step = Math.floor(Math.max(0, elapsed) / SPINNER_MS) % cycle
  return SPINNER[step < SPINNER.length ? step : cycle - step]!
}

/**
 * Choose the turn's word from the locale's list.
 *
 * Deterministic in its seed, so one turn keeps one word and a snapshot keeps
 * the same one on every run, while consecutive turns usually differ.
 *
 * @param copy - locale-owned labels; `activityWords` is `|`-separated.
 * @param seed - any value fixed for the turn, such as its session and position.
 * @returns one word, without trailing punctuation.
 */
export function activityWord(copy: TuiCopy, seed: string): string {
  const words = copy.activityWords.split('|').filter(word => word !== '')
  let hash = 0
  for (const character of seed) hash = (hash * 31 + character.codePointAt(0)!) >>> 0
  return words[hash % words.length] ?? copy.working
}

/**
 * Read the turn's phase off the rows it has produced.
 *
 * The last live row is the newest thing the model said. A call without its
 * result is a tool still running, whether the live region holds it or, with
 * nothing streaming, it committed unfinished.
 *
 * @param live - live rows: running actions, then streaming blocks.
 * @param lastCommitted - the newest committed row, if any.
 * @returns the phase, or undefined while the turn is waiting on the model.
 */
export function phaseOf(live: readonly Row[], lastCommitted: Row | undefined): Phase | undefined {
  const last = live.at(-1)
  if (last?.kind === 'reasoning') return { kind: 'thinking' }
  if (last?.kind === 'assistant') return { kind: 'writing' }
  // The oldest call still without a result: a finished call waits behind it
  // in the live region, so the newest row need not be the one running.
  const running = live.find(row => row.kind === 'tool-call' && row.result === undefined)
  if (running?.kind === 'tool-call') return { kind: 'running', tool: running.tool }
  if (last === undefined && lastCommitted?.kind === 'tool-call' && lastCommitted.result === undefined) {
    return { kind: 'running', tool: lastCommitted.tool }
  }
  return undefined
}

/**
 * The newest line of reasoning, while reasoning is what is streaming.
 *
 * Reasoning arrives faster than it can be read, and drawn row by row it
 * scrolls the whole region with it. One line, replaced in place, shows that
 * the model is working and roughly on what, at a height that never changes.
 * The full text commits to the transcript with the rest of the step.
 *
 * @param live - live rows in arrival order.
 * @returns the last non-empty reasoning line, or undefined when not reasoning.
 */
export function reasoningTicker(live: readonly Row[]): string | undefined {
  const last = live.at(-1)
  if (last?.kind !== 'reasoning') return undefined
  const lines = last.text.split('\n').map(line => line.trim()).filter(line => line !== '')
  return lines.at(-1)
}

/**
 * Localize a phase for the activity line.
 * @param phase - the current phase.
 * @param copy - locale-owned labels.
 * @returns the phase text, or undefined for none.
 */
export function phaseLabel(phase: Phase | undefined, copy: TuiCopy): string | undefined {
  if (phase === undefined) return undefined
  if (phase.kind === 'thinking') return copy.phaseThinking
  if (phase.kind === 'writing') return copy.phaseWriting
  return `${copy.phaseRunning} ${phase.tool}`
}

/**
 * Format elapsed seconds compactly: `8s`, `1m 05s`.
 * @param ms - elapsed milliseconds.
 * @returns the duration text.
 */
export function formatElapsed(ms: number): string {
  const seconds = Math.floor(Math.max(0, ms) / 1000)
  if (seconds < 60) return `${seconds}s`
  return `${Math.floor(seconds / 60)}m ${String(seconds % 60).padStart(2, '0')}s`
}

/** How a finished turn ended, as the summary's glyph and colour read it. */
export type Outcome = 'done' | 'stopped' | 'failed'

/** The line a finished turn leaves above the input until the next one starts. */
export interface TurnSummary {
  readonly outcome: Outcome
  /** Locale-owned word for the outcome. */
  readonly label: string
  /** Elapsed time and action counts, already joined; empty for a turn with neither. */
  readonly details: string
}

/**
 * The newest turn a transcript records as ended: the rows after the turn end
 * before it, through its own.
 *
 * A replayed session has no clock that watched its last turn run, but its log
 * says how that turn ended and what it did, which is what the summary shows.
 *
 * @param rows - committed rows, in order.
 * @returns the turn's rows, or undefined when no turn has ended or a newer
 *   one has started since.
 */
export function lastTurn(rows: readonly Row[]): readonly Row[] | undefined {
  const end = rows.findLastIndex(row => row.kind === 'notice' && row.placement === 'turn-end')
  if (end === -1 || rows.slice(end + 1).some(row => row.kind === 'user')) return undefined
  const previous = rows.findLastIndex((row, index) => index < end && row.kind === 'notice' && row.placement === 'turn-end')
  return rows.slice(previous + 1, end + 1)
}

/** Count order: the verbs a reader scans for first lead. */
const COUNTED: readonly Verb[] = [VERB.edit, VERB.run, VERB.read, VERB.find, VERB.fetch]

/**
 * Summarize a finished turn from the rows it committed.
 *
 * Read from the transcript rather than counted as the turn ran, so the line
 * says what the session log says: a call the log holds is counted once
 * however its result arrived, and the outcome is the recorded turn end.
 *
 * @param rows - rows the turn committed, in order.
 * @param copy - locale-owned labels.
 * @param elapsed - the turn's wall time, absent when no clock measured it.
 * @returns the outcome, its label, and the joined details.
 */
export function turnSummary(rows: readonly Row[], copy: TuiCopy, elapsed: number | undefined): TurnSummary {
  const counts = new Map<Verb, number>()
  let failed = 0
  let outcome: Outcome = 'done'
  let reason: string | undefined
  for (const row of rows) {
    if (row.kind === 'tool-call') {
      const verb = verbFor(row.tool)
      counts.set(verb, (counts.get(verb) ?? 0) + 1)
      if (row.result?.ok === false) failed++
    } else if (row.kind === 'tool-result' && !row.ok) failed++
    else if (row.kind === 'notice' && row.placement === 'turn-end') {
      outcome = row.tone === 'error' ? 'failed' : row.tone === 'warn' ? 'stopped' : 'done'
      reason = row.text.split('\n')[0]
    }
  }
  // A stopped turn says why in a few words — interrupted, blocked, out of
  // tokens — while an error's message can run to paragraphs, so it stays in
  // the transcript and the row says only that the turn failed.
  const label = outcome === 'done' ? copy.turnCompleted : outcome === 'stopped' ? reason ?? copy.cancelled : copy.summaryFailed
  const parts = [
    ...elapsed === undefined ? [] : [formatElapsed(elapsed)],
    ...COUNTED.flatMap(verb => counts.has(verb) ? [`${PAST[verb]} ${counts.get(verb)!}`] : []),
    ...failed === 0 ? [] : [`${failed} ${copy.summaryFailures}`],
  ]
  return { outcome, label, details: parts.join(' \u00b7 ') }
}
