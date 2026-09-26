/**
 * What the agent is doing. A stable label and a looping dot indicator.
 *
 * The live region shows a turn's output. This module names the turn itself.
 * The word is chosen once when the turn starts and kept until it ends, so
 * the header stays stable instead of changing on every commit. The phase,
 * elapsed time, and newest reasoning row change in place, on rows that do
 * not move.
 *
 * Everything here is pure. The caller owns the clock.
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

/** Current turn phase, derived from the rows on screen. */
export type Phase =
  | { readonly kind: 'thinking' }
  | { readonly kind: 'writing' }
  | { readonly kind: 'running', readonly tool: string }

/** Milliseconds per shared animation beat. Moving parts advance together. */
export const FRAME_MS = 150

/**
 * Six frames of a six-column, three-row dot wave moving right.
 *
 * Six-dot Braille packs two columns per cell. The fourth dot row is unused.
 * Each step wraps one dot column across the edge, so the diagonal continues
 * through the seam.
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

/** Centered wave, used when motion is off or no clock is supplied. */
export const SPINNER_REST = SPINNER[0]!

/**
 * Dot-wave frame for an elapsed time. Each column step holds two beats.
 * @param elapsed - milliseconds since the turn started.
 * @returns three Braille cells for that moment.
 */
export function spinnerFrame(elapsed: number): string {
  return SPINNER[Math.floor(Math.max(0, elapsed) / (FRAME_MS * 2)) % SPINNER.length]!
}

/**
 * Pick the turn's word from the locale's list.
 *
 * The choice is deterministic in `seed`, so one turn keeps one word and a
 * snapshot keeps the same word on every run. Consecutive turns usually differ.
 *
 * @param copy - locale-owned labels. `activityWords` is `|`-separated.
 * @param seed - a value fixed for the turn, such as its session and position.
 * @returns one word, without trailing punctuation.
 */
export function activityWord(copy: TuiCopy, seed: string): string {
  const words = copy.activityWords.split('|').filter(word => word !== '')
  let hash = 0
  for (const character of seed) hash = (hash * 31 + character.codePointAt(0)!) >>> 0
  return words[hash % words.length] ?? copy.working
}

/**
 * Derive the turn's phase from the rows it has produced.
 *
 * The last live row is the newest thing the model said. A call without a
 * result is a tool still running, whether the live region holds it or it
 * committed unfinished while nothing is streaming.
 *
 * @param live - live rows. Running actions, then streaming blocks.
 * @param lastCommitted - the newest committed row, if any.
 * @returns the phase, or undefined while the turn is waiting on the model.
 */
export function phaseOf(live: readonly Row[], lastCommitted: Row | undefined): Phase | undefined {
  const last = live.at(-1)
  if (last?.kind === 'reasoning') return { kind: 'thinking' }
  if (last?.kind === 'assistant') return { kind: 'writing' }
  // The oldest call still without a result. A finished call can sit behind
  // it in the live region, so the newest row is not necessarily the one running.
  const running = live.flatMap(callsOf).find(call => call.result === undefined)
  if (running !== undefined) return { kind: 'running', tool: running.tool }
  const unfinished = last === undefined && lastCommitted !== undefined ? callsOf(lastCommitted).find(call => call.result === undefined) : undefined
  if (unfinished !== undefined) return { kind: 'running', tool: unfinished.tool }
  return undefined
}

/** Rows of streaming reasoning the header's thinking window draws at most. */
export const THINKING_ROWS = 3

/**
 * Newest reasoning rows, while reasoning is what is streaming.
 *
 * Reasoning arrives faster than it can be read. Drawing it in full grows the
 * frame to its limit and then scrolls every row on each token. One line shows
 * the start of a long sentence and stops changing until the sentence ends.
 * A window of the newest rows shows the current position and holds its height.
 * Each paragraph wraps from its own start, so a row stays put while plain text
 * grows. Completing Markdown syntax may reflow the live preview. The height
 * never exceeds the window's row limit. Blank lines are omitted. They would
 * spend a row on nothing. The full text commits to the transcript with the
 * rest of the step.
 *
 * @param live - live rows in arrival order.
 * @param width - columns a row may take.
 * @param count - rows the window may draw.
 * @returns the newest rows, oldest first. Empty unless reasoning is newest.
 */
export function thinkingRows(live: readonly Row[], width: number, count: number): readonly string[] {
  const last = live.at(-1)
  if (last?.kind !== 'reasoning' || count <= 0) return []
  // Parsing the whole block on every delta is quadratic in a long thought,
  // and the window draws its last rows only. Parse the newest paragraphs,
  // and more of them only while they draw fewer rows than the window holds.
  for (let paragraphs = count; ; paragraphs *= 2) {
    const start = paragraphStart(last.text, paragraphs)
    const rows = windowRows(last.text.slice(start), width, count)
    if (rows.length >= count || start === 0) return rows
  }
}

/** The newest `count` rows of a Markdown thought, wrapping only those it draws. */
function windowRows(text: string, width: number, count: number): string[] {
  const paragraphs = markdownLines(text, 'thought').map(line => line.text.replace(/\s+/g, ' ').trim()).filter(line => line !== '')
  const rows: string[] = []
  for (let index = paragraphs.length - 1; index >= 0 && rows.length < count; index--) {
    const wrapped = wrapAnsi(paragraphs[index]!, Math.max(1, width), { hard: true, trim: true }).split('\n')
    rows.unshift(...wrapped.slice(-(count - rows.length)))
  }
  return rows
}

/** A line that opens or closes a fenced code block. */
const FENCE = /^ {0,3}(?:`{3,}|~{3,})/gmu

/**
 * Where the `count`th paragraph from the end starts, so the text after it
 * parses as it does within the whole. A paragraph starts after a blank line;
 * a start inside a fenced block moves back to the fence that opened it.
 * @returns 0 when the text has no more paragraphs than that.
 */
function paragraphStart(text: string, count: number): number {
  let start = text.length
  for (let found = 0; found < count; found++) {
    // A blank line at the very start has no paragraph before it.
    const blank = start < 1 ? -1 : text.lastIndexOf('\n\n', start - 1)
    if (blank <= 0) return 0
    start = blank
  }
  start += 2
  const fences = [...text.slice(0, start).matchAll(FENCE)]
  return fences.length % 2 === 0 ? start : fences.at(-1)!.index
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
 * Format elapsed seconds compactly. `8s`, `1m 05s`.
 * @param ms - elapsed milliseconds.
 * @returns the duration text.
 */
export function formatElapsed(ms: number): string {
  const seconds = Math.floor(Math.max(0, ms) / 1000)
  if (seconds < 60) return `${seconds}s`
  return `${Math.floor(seconds / 60)}m ${String(seconds % 60).padStart(2, '0')}s`
}

/** How a finished turn ended, as the summary's glyph and colour report it. */
export type Outcome = 'done' | 'stopped' | 'failed'

/** The line a finished turn leaves above the input until the next turn starts. */
export interface TurnSummary {
  readonly outcome: Outcome
  /** Locale-owned word for the outcome. */
  readonly label: string
  /** Elapsed time and action counts, already joined; empty for a turn with neither. */
  readonly details: string
}

/**
 * Newest turn the transcript records as ended.
 *
 * The slice starts after the previous turn-end notice and includes this
 * turn's own. A replayed session has no clock that watched the last turn,
 * but the log still records how it ended and what it did. That is what the
 * summary shows.
 *
 * @param rows - committed rows, in order.
 * @returns the turn's rows, or undefined when no turn has ended or a newer
 *   user turn has started since.
 */
export function lastTurn(rows: readonly Row[]): readonly Row[] | undefined {
  const end = rows.findLastIndex(row => row.kind === 'notice' && row.placement === 'turn-end')
  if (end === -1 || rows.slice(end + 1).some(row => row.kind === 'user')) return undefined
  const previous = rows.findLastIndex((row, index) => index < end && row.kind === 'notice' && row.placement === 'turn-end')
  return rows.slice(previous + 1, end + 1)
}

/** Verbs included in the summary, edits first. */
const COUNTED: readonly Verb[] = [VERB.edit, VERB.run, VERB.read, VERB.find, VERB.fetch]

/**
 * Summarize a finished turn from the rows it committed.
 *
 * Counts come from the transcript, not from a tally kept while the turn ran.
 * A call the log holds is counted once however its result arrived, and the
 * outcome is the recorded turn end.
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
  // A stopped turn gives a short reason. Interrupted, blocked, or out of
  // tokens. An error message can run to paragraphs, so it stays in the
  // transcript and the header says only that the turn failed.
  const label = outcome === 'done' ? copy.turnCompleted : outcome === 'stopped' ? reason ?? copy.cancelled : copy.summaryFailed
  const parts = [
    ...elapsed === undefined ? [] : [formatElapsed(elapsed)],
    ...COUNTED.flatMap(verb => counts.has(verb) ? [`${PAST[verb]} ${counts.get(verb)!}`] : []),
    ...failed === 0 ? [] : [`${failed} ${copy.summaryFailures}`],
  ]
  return { outcome, label, details: parts.join(' \u00b7 ') }
}
