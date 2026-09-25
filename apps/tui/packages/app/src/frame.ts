/**
 * Which frame the composer draws, decided from the terminal it will draw in.
 *
 * The rounded frame is box-drawing characters. Two properties of a terminal
 * decide whether they render as a frame or as broken glyphs. Both are resolved
 * here, once, at the package boundary. The presentation layer takes the answer
 * as a prop and does not read `process`.
 *
 * @module @dsh-tui/app/frame
 */

import type { FrameStyle } from '@dsh-tui/ui/layout.ts'
import type { Locale } from '@dsh-tui/ui/copy.ts'

/** Everything the decision reads, so it can be made without a terminal. */
export interface FrameRequest {
  /** The profile's choice, or `auto` to resolve it from the terminal. */
  readonly configured: FrameStyle | 'auto'
  /** The interface locale, which is the fallback signal when the environment names none. */
  readonly locale: Locale
  /** Process environment; only the locale and terminal-type variables are read. */
  readonly env: Readonly<Partial<Record<string, string>>>
}

/** Locale variables in the order POSIX resolves them for character handling. */
const CTYPE_VARIABLES = ['LC_ALL', 'LC_CTYPE', 'LANG'] as const

/** Languages whose terminals are commonly configured to draw Ambiguous characters two cells wide. */
const AMBIGUOUS_WIDE_LANGUAGES = ['zh', 'ja', 'ko'] as const

/**
 * The character-handling locale in force, as POSIX resolves it.
 * @param env - process environment.
 * @returns the first variable that is set and non-empty, lowercased, or undefined.
 */
function ctypeOf(env: FrameRequest['env']): string | undefined {
  for (const variable of CTYPE_VARIABLES) {
    const value = env[variable]
    if (value !== undefined && value !== '') return value.toLowerCase()
  }
  return undefined
}

/**
 * Choose the composer's frame.
 *
 * Three terminals cannot draw the rounded frame, and each fails differently.
 *
 * A terminal that is not encoding text as UTF-8 writes the bytes through as
 * mojibake, so the frame becomes punctuation on every row. `TERM=dumb` names
 * a terminal with no rendering beyond text, which is also the value harnesses
 * set when they capture output. A terminal configured to draw East Asian
 * Ambiguous characters two cells wide draws a full-width horizontal run at
 * twice the width Ink measured, so the frame wraps and Ink's row arithmetic
 * is wrong from then on. That last case is the damaging one. Every other
 * Ambiguous character on this surface sits alone in a fixed-width rail and
 * costs one row one column. A border run accumulates the error across the
 * whole line.
 *
 * The first two are read from the environment. The third cannot be detected.
 * It is a terminal preference, not a capability, so a CJK character locale
 * stands in for it. The profile's own setting overrides all three.
 *
 * @param request - the profile's choice and the environment to read.
 * @returns the frame style to draw.
 */
export function resolveFrame(request: FrameRequest): FrameStyle {
  if (request.configured !== 'auto') return request.configured
  const ctype = ctypeOf(request.env)
  // No variable set means the C locale, which is not UTF-8.
  if (ctype === undefined || !/utf-?8/.test(ctype)) return 'classic'
  const term = request.env['TERM']
  if (term === undefined || term === '' || term === 'dumb') return 'classic'
  if (AMBIGUOUS_WIDE_LANGUAGES.some(language => ctype.startsWith(language))) return 'classic'
  return request.locale === 'zh' ? 'classic' : 'round'
}
