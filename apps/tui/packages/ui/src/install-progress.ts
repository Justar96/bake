/**
 * What an install draws while Bake installs a release.
 *
 * Each finished step prints once: a check, what was done, and a dim note
 * ending in how long it took. The step still running is one live row: an
 * orbit, four Braille dots circling a square two cells wide; what the step
 * is doing; and a meter. A step whose length is known fills the meter from
 * the running orange to amber, a glint sweeping across the filled part and
 * its leading edge pulsing. A step without a known length runs a comet back
 * and forth along the track, easing at either end, instead of inventing a
 * percentage. The installers, `bake update`, and the terminal's `/update`
 * draw these same rows, so an install looks the same wherever it runs.
 *
 * Pure. The caller supplies the time, the progress, the width, and what the
 * terminal can draw; nothing here reads the process, the clock, or the
 * environment. Every glyph is one cell wide, so a row's length is its width.
 *
 * @module @dsh-tui/ui/install-progress
 */
import { PALETTE, PROGRESS_TONES } from './palette.ts'

/** Which glyphs a terminal draws the rows in. */
export type ProgressGlyphs = 'unicode' | 'ascii'

/** Milliseconds between frames written straight to a terminal. */
export const PROGRESS_FRAME_MS = 80

/** One styled piece of a row. */
export interface Run {
  readonly text: string
  /** `#rrggbb`; absent, the terminal's own colour. */
  readonly color?: string
  readonly dim?: boolean
  readonly bold?: boolean
}

/** The step an install is running, as its live row shows it. */
export interface InstallStep {
  /** What the step is doing, such as `Downloading`. */
  readonly label: string
  /** How far it is, from 0 to 1; absent when its length is not known. */
  readonly fraction?: number
  /** Dim text after the meter, such as `4.8 / 10.0 MB`. */
  readonly detail?: string
}

/** Cells between a row's parts. */
const GAP = 2
/** Cells of the orbit. */
const ORBIT_CELLS = 2
/** Narrowest meter worth drawing; below it the meter gives way. */
const MIN_METER = 10
/** Widest meter; a wider terminal leaves the rest of the row empty. */
const MAX_METER = 32
/** Milliseconds for the glint to cross the filled part of the meter. */
const SWEEP_MS = 1600
/** Cells either side of the glint's centre that it lights. */
const GLINT_BAND = 4
/** Milliseconds for the comet to run there and back. */
const COMET_MS = 1800

/** The perimeter of a four-by-four dot square, clockwise from its top left, as `[x, y]`. */
const ORBIT: readonly (readonly [number, number])[] = [
  [0, 0], [1, 0], [2, 0], [3, 0], [3, 1], [3, 2], [3, 3], [2, 3], [1, 3], [0, 3], [0, 2], [0, 1],
]
/** Dots the orbit lights: its head and the trail behind it. */
const ORBIT_LENGTH = 4
/** Braille dot bits by column, then row, top to bottom. */
const DOTS = [[0x01, 0x02, 0x04, 0x40], [0x08, 0x10, 0x20, 0x80]] as const
/** The ASCII orbit: a bar turning, then a space, as wide as the Braille one. */
const TURN = ['-', '\\', '|', '/'] as const

const GLYPHS = {
  unicode: { fill: '\u2501', edge: '\u2578', track: '\u2500', done: '\u2713', failed: '\u2717', separator: ' \u00b7 ', ellipsis: '\u2026' },
  ascii: { fill: '=', edge: '>', track: '-', done: '+', failed: 'x', separator: ' - ', ellipsis: '...' },
} as const

const clamp01 = (value: number): number => Math.min(1, Math.max(0, Number.isFinite(value) ? value : 0))

/**
 * Blend two tones.
 * @param from - `#rrggbb` at 0.
 * @param to - `#rrggbb` at 1.
 * @param amount - how far towards `to`, from 0 to 1.
 * @returns the blended `#rrggbb`.
 */
export function mix(from: string, to: string, amount: number): string {
  const at = clamp01(amount)
  const channel = (hex: string, index: number): number => Number.parseInt(hex.slice(1 + index * 2, 3 + index * 2), 16)
  return `#${[0, 1, 2].map(index => Math.round(channel(from, index) + (channel(to, index) - channel(from, index)) * at)
    .toString(16).padStart(2, '0')).join('')}`
}

/**
 * The orbit at a moment: four dots circling a square across two Braille
 * cells, the cell holding the head in amber and the other in orange.
 * @param time - milliseconds on any clock; only differences matter.
 * @param glyphs - what the terminal draws.
 * @returns two cells.
 */
export function orbit(time: number, glyphs: ProgressGlyphs): readonly Run[] {
  const step = Math.floor(Math.max(0, time) / PROGRESS_FRAME_MS)
  if (glyphs === 'ascii') return [{ text: `${TURN[step % TURN.length]!} `, color: PROGRESS_TONES.from, bold: true }]
  const cells = [0, 0]
  const head = step % ORBIT.length
  for (let back = 0; back < ORBIT_LENGTH; back++) {
    const [x, y] = ORBIT[(head - back + ORBIT.length) % ORBIT.length]!
    cells[x >> 1]! |= DOTS[x & 1]![y]!
  }
  const lead = ORBIT[head]![0] >> 1
  return cells.map((bits, cell) => ({ text: String.fromCodePoint(0x2800 + bits),
    color: cell === lead ? PROGRESS_TONES.to : PROGRESS_TONES.from, bold: true }))
}

/**
 * The meter at a moment.
 *
 * With a fraction it fills from the left, each cell taking the gradient's
 * tone for its place on the whole track, so colour reads as how far along
 * the fill is; a glint sweeps across the filled cells, and a half-filled
 * cell at the edge pulses. Without one, a comet runs along the track and
 * back, its tail fading behind it. The glyphs carry both under `NO_COLOR`:
 * filled cells and the comet are heavy, the track is light.
 * @param time - milliseconds on any clock.
 * @param fraction - how far the step is, from 0 to 1; undefined for the comet.
 * @param width - cells the meter takes.
 * @param glyphs - what the terminal draws.
 * @returns the meter's runs, exactly `width` cells.
 */
export function meter(time: number, fraction: number | undefined, width: number, glyphs: ProgressGlyphs): readonly Run[] {
  const cells = Math.max(0, Math.floor(width))
  const { fill, edge, track } = GLYPHS[glyphs]
  const runs: Run[] = []
  const now = Math.max(0, time)
  const tone = (at: number): string => mix(PROGRESS_TONES.from, PROGRESS_TONES.to, cells <= 1 ? 0 : at / (cells - 1))
  if (fraction !== undefined) {
    const exact = clamp01(fraction) * cells
    const full = Math.floor(exact)
    const partial = full < cells && exact - full >= 0.5
    const centre = (now % SWEEP_MS) / SWEEP_MS * (full + GLINT_BAND * 2) - GLINT_BAND
    for (let at = 0; at < full; at++) {
      const glint = Math.max(0, 1 - Math.abs(at - centre) / GLINT_BAND) ** 2 * 0.75
      runs.push({ text: fill, color: mix(tone(at), PROGRESS_TONES.glint, glint) })
    }
    if (partial) {
      const pulse = (1 - Math.cos(2 * Math.PI * (now % SWEEP_MS) / SWEEP_MS)) / 2
      runs.push({ text: edge, color: mix(tone(full), PROGRESS_TONES.glint, 0.2 + 0.5 * pulse) })
    }
    for (let at = full + (partial ? 1 : 0); at < cells; at++) runs.push({ text: track, dim: true })
    return merged(runs)
  }
  const phase = (now % COMET_MS) / COMET_MS
  const forward = phase < 0.5
  const eased = (1 - Math.cos(Math.PI * (forward ? phase * 2 : (1 - phase) * 2))) / 2
  const head = eased * Math.max(0, cells - 1)
  const tail = Math.max(3, Math.round(cells / 4))
  for (let at = 0; at < cells; at++) {
    const behind = forward ? head - at : at - head
    const strength = behind < -0.5 || behind > tail ? 0 : behind <= 0 ? 1 : (1 - behind / tail) ** 1.5
    if (strength < 0.15) { runs.push({ text: track, dim: true }); continue }
    runs.push({ text: glyphs === 'ascii' && strength < 0.35 ? track : fill, color: strength > 0.7
      ? mix(PROGRESS_TONES.to, PROGRESS_TONES.glint, (strength - 0.7) / 0.3 * 0.6)
      : mix(PROGRESS_TONES.track, PROGRESS_TONES.from, strength / 0.7) })
  }
  return merged(runs)
}

/**
 * The live row for the running step: the orbit, the label, the meter, then
 * the percentage, the detail, and the elapsed time. A narrow row gives up the
 * elapsed time, then the detail, then the percentage, then the meter, and
 * cuts the label last.
 * @param step - the running step.
 * @param options.time - milliseconds on any clock.
 * @param options.width - cells the row may take.
 * @param options.glyphs - what the terminal draws.
 * @param options.labelWidth - cells the label is padded to, so meters line up with the log's notes.
 * @param options.elapsed - milliseconds the step has run, when shown.
 * @returns the row's runs, no wider than `width`.
 */
export function liveRow(step: InstallStep, options: {
  readonly time: number
  readonly width: number
  readonly glyphs: ProgressGlyphs
  readonly labelWidth?: number
  readonly elapsed?: number
}): readonly Run[] {
  const { time, width, glyphs } = options
  const lead = [...orbit(time, glyphs), { text: ' ' }]
  const label = plain(step.label, glyphs)
  const labelCells = Math.max(cellsOf(label), options.labelWidth ?? 0)
  const tails: Run[] = [
    ...step.fraction === undefined ? [] : [{ text: `${Math.floor(clamp01(step.fraction) * 100)}%`.padStart(4) }],
    ...step.detail === undefined || step.detail === '' ? [] : [{ text: plain(step.detail, glyphs), dim: true }],
    ...options.elapsed === undefined ? [] : [{ text: formatSeconds(options.elapsed), dim: true }],
  ]
  for (let kept = tails.length; kept >= 0; kept--) {
    const shown = tails.slice(0, kept)
    const fixed = ORBIT_CELLS + 1 + labelCells + GAP + shown.reduce((sum, run) => sum + GAP + cellsOf(run.text), 0)
    if (width - fixed < MIN_METER) continue
    return [...lead, { text: label.padEnd(labelCells) }, { text: ' '.repeat(GAP) },
      ...meter(time, step.fraction, Math.min(MAX_METER, width - fixed), glyphs),
      ...shown.flatMap(run => [{ text: ' '.repeat(GAP) }, run])]
  }
  return fit([...lead, { text: label }], width, glyphs)
}

/**
 * A finished step, printed once: a check or a cross, what was done, and its notes.
 * @param outcome - whether the step finished or failed.
 * @param label - what was done, such as `Downloaded`, or the step that failed.
 * @param notes - dim facts about it, such as its size and how long it took.
 * @param options.width - cells the line may take; notes are cut to fit.
 * @param options.glyphs - what the terminal draws.
 * @param options.labelWidth - cells the label is padded to, so notes line up.
 * @returns the line's runs.
 */
export function stepLine(outcome: 'done' | 'failed', label: string, notes: readonly string[], options: {
  readonly width: number
  readonly glyphs: ProgressGlyphs
  readonly labelWidth?: number
}): readonly Run[] {
  const { glyphs } = options
  const text = plain(label, glyphs)
  const mark: Run = outcome === 'done'
    ? { text: GLYPHS[glyphs].done, color: PALETTE.done, bold: true }
    : { text: GLYPHS[glyphs].failed, color: PALETTE.failed, bold: true }
  const shown = notes.filter(note => note !== '').map(note => plain(note, glyphs))
  // The mark takes the orbit's two cells, so a step's label stays where it was drawn while it ran.
  return fit([mark, { text: ' '.repeat(ORBIT_CELLS) },
    { text: shown.length === 0 ? text : text.padEnd(Math.max(cellsOf(text), options.labelWidth ?? 0)), ...outcome === 'failed' ? { color: PALETTE.failed } : {} },
    ...shown.length === 0 ? [] : [{ text: ' '.repeat(GAP) }, { text: shown.join(GLYPHS[glyphs].separator), dim: true }]],
  options.width, glyphs)
}

/**
 * The line an install opens with: Bake's name, then what is running, dim.
 * @param parts - such as `installer` and the platform.
 * @param width - cells the line may take.
 * @param glyphs - what the terminal draws.
 * @returns the line's runs.
 */
export function headerLine(parts: readonly string[], width: number, glyphs: ProgressGlyphs): readonly Run[] {
  const detail = parts.filter(part => part !== '').map(part => plain(part, glyphs)).join(GLYPHS[glyphs].separator)
  return fit([{ text: 'BAKE', color: PALETTE.running, bold: true },
    ...detail === '' ? [] : [{ text: ' '.repeat(GAP) }, { text: detail, dim: true }]], width, glyphs)
}

/**
 * The line an install closes with: what was installed, bold, then how long it took.
 * @param text - such as `Bake 0.2.0 installed`.
 * @param elapsed - milliseconds the whole install took.
 * @param width - cells the line may take.
 * @param glyphs - what the terminal draws.
 * @returns the line's runs.
 */
export function summaryLine(text: string, elapsed: number | undefined, width: number, glyphs: ProgressGlyphs): readonly Run[] {
  return fit([{ text: plain(text, glyphs), bold: true },
    ...elapsed === undefined ? [] : [{ text: ` in ${formatSeconds(elapsed)}`, dim: true }]], width, glyphs)
}

/**
 * A line under the summary. What follows `run:` is the command to type, so
 * it is drawn bold in Bake's orange and the words leading to it dim.
 * @param text - such as `Run: bake`.
 * @param width - cells the line may take.
 * @param glyphs - what the terminal draws.
 * @returns the line's runs.
 */
export function nextLine(text: string, width: number, glyphs: ProgressGlyphs): readonly Run[] {
  const shown = plain(text, glyphs)
  const command = /^(.*\brun: )(\S.*)$/iu.exec(shown)
  return fit(command === null ? [{ text: shown }]
    : [{ text: command[1]!, dim: true }, { text: command[2]!, color: PALETTE.running, bold: true }], width, glyphs)
}

/**
 * A duration as a person reads it: `<0.1s`, `0.4s`, `12s`, `1m 04s`.
 * @param ms - milliseconds.
 * @returns the duration.
 */
export function formatSeconds(ms: number): string {
  const seconds = Math.max(0, ms) / 1000
  if (seconds < 0.1) return '<0.1s'
  if (seconds < 10) return `${seconds.toFixed(1)}s`
  if (seconds < 60) return `${Math.floor(seconds)}s`
  return `${Math.floor(seconds / 60)}m ${String(Math.floor(seconds % 60)).padStart(2, '0')}s`
}

/**
 * Whether a terminal draws the Unicode rows.
 *
 * Braille is narrow everywhere. The box-drawing meter and the check are East
 * Asian Ambiguous, so a terminal set to draw them two cells wide, which a CJK
 * character locale stands in for, gets ASCII, as it gets the ASCII frame.
 * Windows Terminal draws them without a UTF-8 locale in the environment.
 * @param env - the process environment; only locale and terminal variables are read.
 * @returns the glyphs to draw.
 */
export function progressGlyphsFor(env: Readonly<Partial<Record<string, string>>>): ProgressGlyphs {
  const ctype = [env['LC_ALL'], env['LC_CTYPE'], env['LANG']].find(value => value !== undefined && value !== '')?.toLowerCase()
  if (ctype !== undefined && /^(zh|ja|ko)/.test(ctype)) return 'ascii'
  if (env['WT_SESSION'] !== undefined && env['WT_SESSION'] !== '') return 'unicode'
  if (ctype === undefined || !/utf-?8/.test(ctype)) return 'ascii'
  if (env['TERM'] === undefined || env['TERM'] === '' || env['TERM'] === 'dumb') return 'ascii'
  return 'unicode'
}

/** How many colours a terminal written to directly shows. */
export type ColorDepth = 'truecolor' | 'ansi256' | 'none'

/**
 * The colours to write for an environment: none under `NO_COLOR`, 24-bit
 * where the terminal says it draws them, and the 256-colour cube otherwise.
 * @param env - the process environment.
 * @returns the depth.
 */
export function colorDepthFor(env: Readonly<Partial<Record<string, string>>>): ColorDepth {
  if (env['NO_COLOR'] !== undefined) return 'none'
  const declared = env['COLORTERM']?.toLowerCase()
  if (declared === 'truecolor' || declared === '24bit' || (env['WT_SESSION'] !== undefined && env['WT_SESSION'] !== '')) return 'truecolor'
  return 'ansi256'
}

/**
 * The xterm-256 colour nearest a hex tone.
 * @param hex - `#rrggbb`.
 * @returns an index into the 6x6x6 colour cube.
 */
export function ansi256(hex: string): number {
  const levels = [0, 95, 135, 175, 215, 255]
  const nearest = (value: number): number => levels.reduce((best, level, index) =>
    Math.abs(level - value) < Math.abs(levels[best]! - value) ? index : best, 0)
  const [r, g, b] = [1, 3, 5].map(at => nearest(Number.parseInt(hex.slice(at, at + 2), 16)))
  return 16 + 36 * r! + 6 * g! + b!
}

/**
 * Runs as terminal text, for a stream written without Ink. Neighbouring runs
 * that come out the same at this depth share one escape.
 * @param runs - the row.
 * @param depth - the colours the terminal shows.
 * @returns the text, with no line ending.
 */
export function ansi(runs: readonly Run[], depth: ColorDepth): string {
  if (depth === 'none') return runs.map(run => run.text).join('')
  const pieces: { code: string, text: string }[] = []
  for (const run of runs) {
    const codes = [...run.bold === true ? ['1'] : [], ...run.dim === true ? ['2'] : []]
    if (run.color !== undefined) {
      const channel = (index: number): number => Number.parseInt(run.color!.slice(1 + index * 2, 3 + index * 2), 16)
      codes.push(depth === 'truecolor' ? `38;2;${channel(0)};${channel(1)};${channel(2)}` : `38;5;${ansi256(run.color)}`)
    }
    const code = codes.join(';')
    const last = pieces.at(-1)
    if (last !== undefined && last.code === code) last.text += run.text
    else pieces.push({ code, text: run.text })
  }
  return pieces.map(piece => piece.code === '' ? piece.text : `\u001b[${piece.code}m${piece.text}\u001b[0m`).join('')
}

/** Neighbouring runs of one style as one. */
function merged(runs: readonly Run[]): readonly Run[] {
  const out: Run[] = []
  for (const run of runs) {
    const last = out.at(-1)
    if (last !== undefined && last.color === run.color && last.dim === run.dim && last.bold === run.bold) out[out.length - 1] = { ...last, text: last.text + run.text }
    else out.push(run)
  }
  return out
}

/** Code points, which is cells for this module's glyphs. */
function cellsOf(text: string): number {
  return [...text].length
}

/** Text the ASCII rows can draw: the Unicode punctuation they share, spelled in ASCII. */
function plain(text: string, glyphs: ProgressGlyphs): string {
  return glyphs === 'unicode' ? text : text.replace(/\u2192/gu, '->').replace(/\u00b7/gu, '-').replace(/\u2026/gu, '...')
}

/**
 * Runs cut to a width, the last one ending in an ellipsis where text was cut.
 * @param runs - the line.
 * @param width - cells it may take.
 * @param glyphs - which ellipsis to draw.
 * @returns the runs that fit.
 */
function fit(runs: readonly Run[], width: number, glyphs: ProgressGlyphs): readonly Run[] {
  const room = Math.max(0, Math.floor(width))
  if (runs.reduce((sum, run) => sum + cellsOf(run.text), 0) <= room) return runs
  const ellipsis = GLYPHS[glyphs].ellipsis
  const budget = Math.max(0, room - cellsOf(ellipsis))
  const out: Run[] = []
  let used = 0
  for (const run of runs) {
    const cells = [...run.text]
    if (used + cells.length <= budget) { out.push(run); used += cells.length; continue }
    const kept = cells.slice(0, budget - used).join('')
    out.push({ ...run, text: `${kept}${room >= cellsOf(ellipsis) ? ellipsis : ''}` })
    return out
  }
  return out
}
