/**
 * The loaf that bakes while Bake installs a release.
 *
 * One row: three cells of steam rising, a loaf that browns from dough to
 * crust as the install advances, and what the install is doing. The TUI's
 * `/update`, `bake update`, and both installers draw this same row, so an
 * install looks the same wherever it runs.
 *
 * Pure. The caller supplies the time, the progress, and what the terminal
 * can draw; nothing here reads the process, the clock, or the environment.
 *
 * @module @dsh-tui/ui/loaf
 */
import { CRUST } from './palette.ts'

/** Which glyphs a terminal can draw the loaf in. */
export type LoafGlyphs = 'unicode' | 'ascii'

/** Milliseconds between steam frames. */
export const LOAF_FRAME_MS = 120

/** Cells of steam before the loaf. */
const STEAM_CELLS = 3

/**
 * The loaf's dome. Seven cells, and the ASCII loaf is as wide, so a
 * terminal's glyphs never move the label.
 */
const DOME = '\u2584\u2586\u2588\u2588\u2588\u2586\u2584'

/** ASCII crumb from dough to crust, drawn inside `(` and `)`. */
const CRUMB = ['.', ':', '=', '#'] as const

/** Heights a wisp passes through, then gaps before the next rises. */
const WISP_PERIOD = 6

/** Braille dot bits by row, top to bottom, in the left and right columns. */
const DOTS = [[0x01, 0x02, 0x04, 0x40], [0x08, 0x10, 0x20, 0x80]] as const

/**
 * Steam at one step: in each cell a wisp rises from the bottom row to the
 * top, drifting between the cell's two columns, with a fainter dot trailing
 * a row below it. The cells start their wisps apart, so the steam never
 * rises in step.
 */
const STEAM: readonly string[] = Array.from({ length: WISP_PERIOD * 2 }, (_, step) =>
  Array.from({ length: STEAM_CELLS }, (_, cell) => {
    const height = (step + cell * 2) % WISP_PERIOD
    const column = ((step + cell) >> 1) & 1
    let dots = 0
    for (const [offset, rise] of [[0, height], [1, height - 1]] as const) {
      if (rise < 0 || rise > 3) continue
      dots |= DOTS[(column + offset) & 1]![3 - rise]!
    }
    return String.fromCodePoint(0x2800 + dots)
  }).join(''))

/** ASCII steam, rising through the same periods. */
const PUFFS = ['.', ':', "'", '`', ' ', ' '] as const
const ASCII_STEAM: readonly string[] = Array.from({ length: WISP_PERIOD }, (_, step) =>
  Array.from({ length: STEAM_CELLS }, (_, cell) => PUFFS[(step + cell * 2) % WISP_PERIOD]!).join(''))

/** How far one install step has baked. */
export type BakeStep =
  | { readonly phase: 'check' }
  | { readonly phase: 'download', readonly received: number, readonly total: number }
  | { readonly phase: 'unpack' }
  | { readonly phase: 'verify' }
  | { readonly phase: 'done' }

/**
 * How brown the loaf is for an install step, from 0 to 1.
 *
 * The download is the long step, so it takes most of the colour, and what
 * follows finishes the crust.
 * @param step - the install step.
 * @returns the loaf's doneness.
 */
export function bakeLevel(step: BakeStep): number {
  switch (step.phase) {
    case 'check': return 0
    case 'download': return 0.1 + 0.7 * (step.total <= 0 ? 1 : Math.min(1, Math.max(0, step.received / step.total)))
    case 'unpack': return 0.85
    case 'verify': return 0.95
    case 'done': return 1
  }
}

/**
 * The crust tone for a doneness.
 * @param level - from 0, dough, to 1, crust.
 * @returns a hex colour.
 */
export function crustTone(level: number): string {
  const clamped = Math.min(1, Math.max(0, Number.isFinite(level) ? level : 0))
  return CRUST[Math.round(clamped * (CRUST.length - 1))]!
}

/** One drawing of the row's glyphs. */
export interface LoafFrame {
  readonly steam: string
  readonly loaf: string
  readonly tone: string
}

/**
 * The steam and loaf at a moment.
 * @param time - milliseconds on any clock; only differences matter.
 * @param level - the loaf's doneness, from {@link bakeLevel}.
 * @param glyphs - what the terminal draws.
 * @returns the steam, the loaf, and the loaf's tone.
 */
export function loafFrame(time: number, level: number, glyphs: LoafGlyphs): LoafFrame {
  const step = Math.floor(Math.max(0, time) / LOAF_FRAME_MS)
  const tone = crustTone(level)
  if (glyphs === 'unicode') return { steam: STEAM[step % STEAM.length]!, loaf: DOME, tone }
  const crumb = CRUMB[Math.min(CRUMB.length - 1, Math.floor(Math.min(1, Math.max(0, level)) * CRUMB.length))]!
  return { steam: ASCII_STEAM[step % ASCII_STEAM.length]!, loaf: `(${crumb.repeat(5)})`, tone }
}

/** The finished loaf, without steam, for the line an install leaves behind. */
export function loafDone(glyphs: LoafGlyphs): LoafFrame {
  return { steam: ' '.repeat(STEAM_CELLS), loaf: glyphs === 'unicode' ? DOME : `(${CRUMB.at(-1)!.repeat(5)})`, tone: crustTone(1) }
}

/**
 * Whether a terminal draws the Unicode loaf.
 *
 * Braille is narrow everywhere. The block elements of the dome are East
 * Asian Ambiguous, so a terminal set to draw them two cells wide, which a
 * CJK character locale stands in for, gets the ASCII loaf, as it gets the
 * ASCII frame.
 * @param env - the process environment; only locale and terminal-type variables are read.
 * @returns the glyphs to draw.
 */
export function loafGlyphsFor(env: Readonly<Partial<Record<string, string>>>): LoafGlyphs {
  const ctype = [env['LC_ALL'], env['LC_CTYPE'], env['LANG']].find(value => value !== undefined && value !== '')?.toLowerCase()
  if (ctype === undefined || !/utf-?8/.test(ctype)) return 'ascii'
  if (env['TERM'] === undefined || env['TERM'] === '' || env['TERM'] === 'dumb') return 'ascii'
  return /^(zh|ja|ko)/.test(ctype) ? 'ascii' : 'unicode'
}

/**
 * The xterm-256 colour nearest a hex tone, for terminals written to directly.
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
 * The whole row as terminal text, for a stream written without Ink.
 * @param frame - the glyphs to draw.
 * @param label - what the install is doing.
 * @param options.color - write the loaf's tone and dim the steam.
 * @param options.width - cells the row may take; the label is cut to fit.
 * @returns the row, with no line ending.
 */
export function loafLine(frame: LoafFrame, label: string, options: { readonly color: boolean, readonly width: number }): string {
  const lead = `  ${frame.steam} ${frame.loaf}  `
  // Every glyph in the lead is one cell wide, so its length is its width.
  const room = Math.max(0, options.width - lead.length)
  const text = [...label].length > room ? `${[...label].slice(0, Math.max(0, room - 1)).join('')}${room > 0 ? '\u2026' : ''}` : label
  if (lead.length > options.width) return ''
  if (!options.color) return `${lead}${text}`
  return `  \u001b[2m${frame.steam}\u001b[22m \u001b[38;5;${ansi256(frame.tone)}m${frame.loaf}\u001b[39m  ${text}`
}
