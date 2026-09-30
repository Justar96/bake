/**
 * Compact placeholders for long pasted text and pasted images.
 *
 * A placeholder is ordinary draft text, such as `[Pasted text #1 +42 lines]`
 * or `[Image #2]`, that the composer registers when it inserts it. The cursor
 * steps over a registered placeholder as one character, and one Backspace or
 * Delete removes it whole. Pasted text is expanded back into the draft when it
 * is submitted; an image placeholder stays in the text beside its attachment.
 * The format is fixed English because the submitted text carries it.
 */

/** Pasted text longer than this many UTF-16 units collapses into a placeholder. */
export const PASTE_COLLAPSE_CHARS = 800

/** Pasted text with at least this many lines collapses into a placeholder. */
export const PASTE_COLLAPSE_LINES = 3

/** What a registered placeholder stands for. */
export type PasteAtom =
  | { readonly kind: 'text', readonly text: string }
  | { readonly kind: 'image', readonly key: string }

/**
 * Whether a paste is long enough to collapse.
 * @param text - the paste as `composerText` leaves it.
 * @returns true above {@link PASTE_COLLAPSE_CHARS} or at {@link PASTE_COLLAPSE_LINES} lines.
 */
export function collapses(text: string): boolean {
  return text.length > PASTE_COLLAPSE_CHARS || text.split('\n').length >= PASTE_COLLAPSE_LINES
}

/**
 * The placeholder a collapsed paste shows in the draft.
 * @param id - the draft's paste number.
 * @param text - the pasted text.
 * @returns `[Pasted text #id +N lines]`, or `[Pasted text #id N chars]` for one long line.
 */
export function pastedTextToken(id: number, text: string): string {
  const breaks = text.split('\n').length - 1
  return breaks > 0 ? `[Pasted text #${id} +${breaks} lines]` : `[Pasted text #${id} ${text.length} chars]`
}

/**
 * The placeholder a pasted image shows in the draft.
 * @param id - the draft's paste number.
 * @returns `[Image #id]`.
 */
export function imageToken(id: number): string {
  return `[Image #${id}]`
}

/**
 * Replace every registered text placeholder with the text it stands for.
 * @param text - the draft.
 * @param atoms - registered placeholders.
 * @returns the text to submit; image placeholders stay.
 */
export function expandPastes(text: string, atoms: ReadonlyMap<string, PasteAtom>): string {
  let result = text
  for (const [token, atom] of atoms) if (atom.kind === 'text') result = result.split(token).join(atom.text)
  return result
}

const IMAGE_EXTENSION = /\.(?:png|jpe?g|webp|gif)$/iu

/**
 * Read a paste as one image file path, the way terminals deliver a dropped file.
 *
 * Terminals quote a dropped path, escape its spaces with backslashes, or send
 * a `file://` URL. Only a paste that is exactly one such path is read; a path
 * inside a sentence stays text.
 *
 * @param text - the paste.
 * @returns the unquoted path, or undefined when the paste is not one image path.
 */
export function imagePath(text: string): string | undefined {
  let path = text.trim()
  if (path === '' || /[\n\r]/u.test(path)) return undefined
  const quoted = /^(['"])(.*)\1$/u.exec(path)
  if (quoted !== null) path = quoted[2]!
  // A Windows drive path keeps its separators; elsewhere a backslash escapes.
  else if (!/^[A-Za-z]:\\/u.test(path)) path = path.replace(/\\(.)/gu, '$1')
  if (path.startsWith('file://')) {
    try { path = decodeURIComponent(new URL(path).pathname) } catch { return undefined }
  }
  return IMAGE_EXTENSION.test(path) ? path : undefined
}
