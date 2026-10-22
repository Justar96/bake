/**
 * What the agent is doing. A stable label and a looping dot indicator.
 *
 * The live region shows a turn's output. This module names the turn itself.
 * The word is chosen once when the turn starts and kept until it ends, so
 * the header stays stable instead of changing on every commit. The phase
 * and elapsed time change in place, on a row that does not move.
 *
 * Everything here is pure. The caller owns the clock.
 *
 * @module bake-tui-ui/activity
 */

import { callsOf, type Row, type ToolCallRow } from './rows.ts'
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

/** Current turn phase, derived from the rows on screen. */
export type Phase =
  | { readonly kind: 'thinking' }
  | { readonly kind: 'writing' }
  /** `more` counts the other calls running alongside `tool`, as a batch's do; absent for none. */
  | { readonly kind: 'running', readonly tool: string, readonly more?: number }

/** Milliseconds per shared animation beat. Moving parts advance together. */
export const FRAME_MS = 150

/** Braille dot bits by row, top to bottom, in a cell's left and right columns. */
const BRAILLE = [[0x01, 0x02, 0x04, 0x40], [0x08, 0x10, 0x20, 0x80]] as const

/**
 * Draw a six-by-four dot picture as three Braille cells.
 * @param picture - four rows of six `#` or `.`, joined by `/`.
 */
function braille(picture: string): string {
  const rows = picture.split('/')
  return Array.from({ length: 3 }, (_, cell) => {
    let dots = 0
    rows.forEach((row, y) => {
      for (let x = 0; x < 2; x++) if (row[cell * 2 + x] === '#') dots |= BRAILLE[x]![y]!
    })
    return String.fromCodePoint(0x2800 + dots)
  }).join('')
}

/**
 * Half a kneading stroke: a round ball squashed to an oval, pressed into a
 * low dome, its left edge lifted and folded over, then rounded up again.
 * No frame has a square corner, so the dough always reads as soft.
 */
const STROKE = [
  '..##../.####./.####./..##..',
  '....../.####./######/.####.',
  '....../....../.####./######',
  '....../.#..../.####./######',
  '....../.##.../.####./######',
  '....../..##../.####./.####.',
] as const

/**
 * Twelve frames of dough being kneaded, three Braille cells wide.
 *
 * The second stroke folds from the right, as a baker turns the dough
 * between folds, so the loop never repeats a half in the same direction.
 * The dough stays centred in its cells and on the bottom rows, so the shape
 * morphs in place and the word beside it never moves.
 */
export const SPINNER: readonly string[] = [
  ...STROKE,
  ...STROKE.map(picture => picture.split('/').map(row => [...row].reverse().join('')).join('/')),
].map(braille)

/** The ball of dough, used when motion is off or no clock is supplied. */
export const SPINNER_REST = SPINNER[0]!

/**
 * Kneading frame for an elapsed time. Each frame holds one beat.
 * @param elapsed - milliseconds since the turn started.
 * @returns three Braille cells for that moment.
 */
export function spinnerFrame(elapsed: number): string {
  return frameAt(SPINNER, elapsed)
}

/**
 * Nine frames of dough being laminated, the way compaction folds history
 * into a summary, three Braille cells wide.
 *
 * A flat sheet lifts its ends, stands them up, and folds them over to meet
 * in the middle: three layers in a compact block. The block is pressed and
 * rolled out flat again for the next fold. Both ends move together, so every
 * frame is its own mirror image and stays on the bottom rows, and the word
 * beside it never moves. Symmetric and square where the kneading is lopsided
 * and round, so the two never read as one another.
 */
export const FOLD_SPINNER: readonly string[] = [
  '....../....../....../######',
  '....../....../#....#/.####.',
  '....../#....#/.#..#./..##..',
  '....../.#..#./.#..#./..##..',
  '....../..##../.#..#./..##..',
  '....../..##../..##../..##..',
  '....../..##../..##../.####.',
  '....../....../.####./.####.',
  '....../....../..##../######',
].map(braille)

/** The folded block, pressed square, used when motion is off or no clock is supplied. */
export const FOLD_REST = FOLD_SPINNER[7]!

/**
 * Laminating frame for an elapsed time, on the kneading's beat.
 * @param elapsed - milliseconds since compaction started.
 * @returns three Braille cells for that moment.
 */
export function foldFrame(elapsed: number): string {
  return frameAt(FOLD_SPINNER, elapsed)
}

/** Which dough a running header works: kneading for a turn, laminating for compaction. */
export type Spinner = 'knead' | 'fold'

/** The frame of a loop that holds for `elapsed`, one frame a beat. */
function frameAt(frames: readonly string[], elapsed: number): string {
  return frames[Math.floor(Math.max(0, elapsed) / FRAME_MS) % frames.length]!
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
  const running = live.flatMap(callsOf).filter(call => call.result === undefined)
  if (running.length > 0) return runningPhase(running)
  const unfinished = last === undefined && lastCommitted !== undefined ? callsOf(lastCommitted).filter(call => call.result === undefined) : []
  return unfinished.length > 0 ? runningPhase(unfinished) : undefined
}

/**
 * The phase of calls without a result: the oldest names it, and the rest are
 * counted. A program waiting on its nested call is not counted beside it.
 * @param calls - the unfinished calls, oldest first, nested dispatches before their program.
 */
function runningPhase(calls: readonly ToolCallRow[]): Phase {
  const nested = new Set(calls.flatMap(call => (call.dispatches ?? []).some(child => child.result === undefined) ? [call.callId] : []))
  const more = calls.filter(call => !nested.has(call.callId)).length - 1
  return { kind: 'running', tool: calls[0]!.tool, ...more > 0 ? { more } : {} }
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
  return `${copy.phaseRunning} ${phase.tool}${phase.more === undefined ? '' : ` +${phase.more}`}`
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
  /** Elapsed time, action counts, and the answer's rate, already joined; empty for a turn with none of them. */
  readonly details: string
  /** The part of `details` a narrow header keeps once the counts and rate give way: the elapsed time, or empty. */
  readonly brief: string
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

/** Verbs included in the summary, edits first and delegations last. */
const COUNTED: readonly Verb[] = [VERB.edit, VERB.run, VERB.read, VERB.find, VERB.fetch, VERB.spawn]

/**
 * Summarize a finished turn from the rows it committed.
 *
 * Counts come from the transcript, not from a tally kept while the turn ran.
 * A call the log holds is counted once however its result arrived, and the
 * outcome is the recorded turn end. The final answer's generation speed
 * closes the details, `· 42 tok/s`, when its sample means something
 * ({@link rateLabel}); one summary row carries the turn's numbers.
 *
 * @param rows - rows the turn committed, in order.
 * @param copy - locale-owned labels.
 * @param elapsed - the turn's wall time, absent when no clock measured it.
 * @returns the outcome, its label, the joined details, and the brief form a narrow header keeps.
 */
export function turnSummary(rows: readonly Row[], copy: TuiCopy, elapsed: number | undefined): TurnSummary {
  const counts = new Map<Verb, number>()
  let failed = 0
  let outcome: Outcome = 'done'
  let reason: string | undefined
  let rate: string | undefined
  for (const row of rows) {
    for (const call of callsOf(row)) {
      const verb = verbFor(call.tool)
      counts.set(verb, (counts.get(verb) ?? 0) + 1)
      if (call.result?.ok === false) failed++
    }
    if (row.kind === 'tool-result' && !row.ok) failed++
    else if (row.kind === 'rate') rate = rateLabel(row, copy)
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
    ...rate === undefined ? [] : [rate],
  ]
  return { outcome, label, details: parts.join(' \u00b7 '), brief: elapsed === undefined ? '' : formatElapsed(elapsed) }
}

/** Fewest milliseconds a final answer's rate is measured over before the summary reports it. */
export const RATE_MIN_MS = 1000

/** Fewest output tokens a final answer's rate is measured over before the summary reports it. */
export const RATE_MIN_TOKENS = 64

/**
 * A final answer's generation speed as the turn's summary reports it, `42 tok/s`.
 *
 * A short answer's first and last tokens can arrive in one network read, and
 * four tokens over 74 ms is not a speed anyone can act on. A sample under
 * {@link RATE_MIN_MS} or {@link RATE_MIN_TOKENS}, or one without finite
 * numbers, reports nothing; the log
 * keeps the numbers either way.
 *
 * @param rate - output tokens and the milliseconds they took.
 * @param copy - locale-owned unit.
 * @returns the rate in whole tokens a second, or undefined for a sample too small.
 */
export function rateLabel(rate: { readonly tokens: number, readonly ms: number }, copy: TuiCopy): string | undefined {
  // A row from an older build or a malformed log can carry no numbers; `NaN tok/s` says nothing either.
  if (!Number.isFinite(rate.tokens) || !Number.isFinite(rate.ms) || rate.ms < RATE_MIN_MS || rate.tokens < RATE_MIN_TOKENS) return undefined
  const perSecond = Math.round(rate.tokens / (rate.ms / 1000))
  return Number.isFinite(perSecond) ? `${perSecond} ${copy.rateUnit}` : undefined
}
