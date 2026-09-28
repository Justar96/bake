/**
 * UI presentation of `present` calls: the delivered files by name. The full
 * paths stay in the result text, `Presented <path>` a line, and descriptions
 * stay in the durable delivery event. Pure over the logged arguments, so a
 * replayed session draws the same card.
 * @module @deepseek-ai/dsh-tool-present/src/presentation
 */

import type { GenericCallView } from '@deepseek-ai/dsh-tools'

/** Files a title names before counting the rest. */
const NAMED_FILES = 3

/** Characters a title may take: a card header or a log line. */
const TITLE_CHARS = 80

/**
 * The last segment of a path, in either separator convention.
 * @param path - a model-supplied path.
 * @returns its file name, or the trimmed path when it has no segment.
 */
function fileName(path: string): string {
  const segments = path.trim().split(/[\\/]+/).filter(segment => segment !== '')
  return segments.at(-1) ?? path.trim()
}

/**
 * Cut text to a number of characters, marking the cut with an ellipsis.
 * Counted in code points, so a cut never splits a surrogate pair; a UI
 * measures display width itself.
 * @param text - one line of text.
 * @param limit - the most characters to keep, ellipsis included.
 * @returns the text, or its first characters and `…`.
 */
function clip(text: string, limit: number): string {
  const characters = Array.from(text)
  return characters.length <= limit ? text : `${characters.slice(0, limit - 1).join('').trimEnd()}\u2026`
}

/**
 * The pending card: the file names in call order, `report.md, notes.md`, and
 * `+N` for files past the first {@link NAMED_FILES}.
 * @param files - the validated file entries.
 * @returns a generic card titled by the files it delivers.
 */
export function presentFilesCall(files: readonly { readonly path: string }[]): GenericCallView {
  const names = files.slice(0, NAMED_FILES).map(file => fileName(file.path).replace(/\s+/g, ' '))
  const rest = files.length - names.length
  const title = `${names.join(', ')}${rest > 0 ? ` +${rest}` : ''}`
  return { card: 'generic', title: clip(title, TITLE_CHARS), kind: 'other' }
}
