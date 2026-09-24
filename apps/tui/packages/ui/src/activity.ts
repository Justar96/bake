/**
 * What the agent is doing, with a steady label and a looping dot indicator.
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

import wrapAnsi from 'wrap-ansi'
import { callsOf, type Row } from './rows.ts'
import type { TuiCopy } from './copy.ts'
import { PAST, VERB, type Verb } from './layout.ts'
import { verbFor } from './present.ts'
import { markdownLines } from './markdown.ts'

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

/** Milliseconds per shared animation beat; moving parts publish together. */
export const FRAME_MS = 150

/**
 * A six-column, three-row dot wave moving right. Six-dot Braille packs two
 * columns per cell; the fourth dot row stays unused. Each step wraps one
 * dot column across the edge, preserving the diagonal wave through the seam.
 */
export const SPINNER: readonly string[] = Array.from({ length: 6 }, (_, step) =>
  Array.from({ length: 3 }, (_, cell) => {
    let dots = 0
    for (let row = 0; row < 3; row++) {
      const edge = 3 - row
      for (let column = 0; column < 2; column++) {
        const x = (cell * 2 + column - step + 6) % 6
        if (x === edge || x === edge + 1) dots |= 1 << (row + column * 3)
      }
    }
    return String.fromCodePoint(0x2800 + dots)
  }).join(''))

/** Centered wave when motion is disabled or no clock is supplied. */
export const SPINNER_REST = SPINNER[0]!

/**
 * The shimmer's band, as brightness levels from its trailing to its leading
 * edge: a soft rise to a peak and a matching fall, so the light reads as
 * gliding along the composer's rule rather than a block jumping from cell to
 * cell. Level 0 is the rule's own line, and {@link SHIMMER_LEVELS} the glint.
 */
const BAND = [1, 2, 3, 2, 1] as const

/** Brightness levels above the word's own colour; the band's peak. */
export const SHIMMER_LEVELS = 3

/**
 * Beats the rule rests unlit between sweeps. Constant light is noise; a sweep
 * that returns after a pause reads as a pulse of life, and while the rule
 * rests only the spinner draws, on the beats where its glyph changes.
 */
const SHIMMER_REST = 12

/** Cells of rule per cell of stride: up to 32 cells the light moves one a beat, up to 64 two, beyond that three. */
const LIGHT_CELLS_PER_STRIDE = 32

/**
 * Cells the light moves a beat along a rule of a given length.
 *
 * One cell a beat crosses a wide terminal's rule in fifteen seconds, which
 * reads as stalled; a stride longer than the band would leave gaps it jumps.
 * So a wider rule is crossed in longer strides, up to three cells.
 *
 * @param length - cells of rule the light crosses.
 * @returns 1 to 3.
 */
export const lightStride = (length: number): number => Math.min(3, Math.max(1, Math.ceil(length / LIGHT_CELLS_PER_STRIDE)))

/**
 * The shimmer's brightness for each cell at an elapsed time.
 *
 * The band enters from the left, `stride` cells a beat, crosses and leaves on
 * the right, then the cells rest unlit for {@link SHIMMER_REST} beats. A
 * longer run takes longer to cross, at the same speed.
 *
 * @param length - cells the band crosses.
 * @param elapsed - milliseconds since the work started.
 * @param stride - cells the band moves a beat.
 * @returns one level per cell, 0 (unlit) to {@link SHIMMER_LEVELS}; all
 *   zero while the band rests.
 */
export function shimmer(length: number, elapsed: number, stride = 1): readonly number[] {
  const levels = Array.from<number>({ length }).fill(0)
  const step = shimmerStep(length, elapsed, stride)
  if (step === undefined) return levels
  // The band's trailing edge, which starts off the left of the word so its
  // leading edge is the first to enter.
  const trailing = step - (BAND.length - 1)
  BAND.forEach((level, offset) => {
    const at = trailing + offset
    if (at >= 0 && at < length) levels[at] = level
  })
  return levels
}

/**
 * Where along its sweep the shimmer is.
 * @param length - cells the band crosses.
 * @param elapsed - milliseconds since the work started.
 * @param stride - cells the band moves a beat.
 * @returns the band's leading cell while it is on the run, or undefined while it rests.
 */
export function shimmerStep(length: number, elapsed: number, stride = 1): number | undefined {
  if (length <= 0) return undefined
  const travel = Math.ceil((length + BAND.length - 1) / stride)
  const beat = Math.floor(Math.max(0, elapsed) / FRAME_MS) % (travel + SHIMMER_REST)
  return beat < travel ? beat * stride : undefined
}

/**
 * Pick the single-line dot wave at an elapsed time; each dot-column step holds two beats.
 * @param elapsed - milliseconds since the turn started.
 * @returns one line of three Braille cells for that moment.
 */
export function spinnerFrame(elapsed: number): string {
  return SPINNER[Math.floor(Math.max(0, elapsed) / (FRAME_MS * 2)) % SPINNER.length]!
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
  const running = live.flatMap(callsOf).find(call => call.result === undefined)
  if (running !== undefined) return { kind: 'running', tool: running.tool }
  const unfinished = last === undefined && lastCommitted !== undefined ? callsOf(lastCommitted).find(call => call.result === undefined) : undefined
  if (unfinished !== undefined) return { kind: 'running', tool: unfinished.tool }
  return undefined
}

/** Rows of streaming reasoning the header's thinking window draws at most. */
export const THINKING_ROWS = 3

/**
 * The newest rows of reasoning, while reasoning is what is streaming.
 *
 * Reasoning arrives faster than it can be read. Drawn whole it grows the
 * frame to its limit and then scrolls every row of it with each token; cut to
 * one line it shows the start of a long sentence and stops changing until the
 * sentence ends. A window of its newest rows shows where it has got to, and
 * holds its height: each paragraph wraps from its own start, so a row stays
 * put while plain text grows. Completing Markdown syntax may reflow the live
 * preview; its height never exceeds the window's row limit.
 * Blank lines are left out, since they would spend a row of it on nothing.
 * The full text commits to the transcript with the rest of the step.
 *
 * @param live - live rows in arrival order.
 * @param width - columns a row may take.
 * @param count - rows the window may draw.
 * @returns the newest rows, oldest first; empty unless reasoning is newest.
 */
export function thinkingRows(live: readonly Row[], width: number, count: number): readonly string[] {
  const last = live.at(-1)
  if (last?.kind !== 'reasoning' || count <= 0) return []
  const paragraphs = markdownLines(last.text, 'thought').map(line => line.text.replace(/\s+/g, ' ').trim()).filter(line => line !== '')
  const rows: string[] = []
  // Parsing reads the current block; wrapping visits only its visible tail.
  for (let index = paragraphs.length - 1; index >= 0 && rows.length < count; index--) {
    const wrapped = wrapAnsi(paragraphs[index]!, Math.max(1, width), { hard: true, trim: true }).split('\n')
    rows.unshift(...wrapped.slice(-(count - rows.length)))
  }
  return rows
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
    for (const call of callsOf(row)) {
      const verb = verbFor(call.tool)
      counts.set(verb, (counts.get(verb) ?? 0) + 1)
      if (call.result?.ok === false) failed++
    }
    if (row.kind === 'tool-result' && !row.ok) failed++
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
