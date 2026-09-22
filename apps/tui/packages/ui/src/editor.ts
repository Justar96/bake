/** Grapheme-safe text editing; Ink owns key and bracketed-paste decoding. */
const segmenter = new Intl.Segmenter(undefined, { granularity: 'grapheme' })

/** Text and a UTF-16 cursor offset at a grapheme boundary. */
export interface Draft {
  readonly text: string
  readonly cursor: number
}

/**
 * Remove terminal controls while retaining pasted line breaks and tabs.
 * @param text - text delivered by Ink.
 * @returns text safe to display in the composer.
 */
export function composerText(text: string): string {
  return text.replace(/\r\n?/g, '\n').replace(/[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f-\u009f]/g, '')
}

/**
 * Place a cursor without splitting a visible character.
 * @param text - complete draft.
 * @param cursor - requested UTF-16 offset, defaulting to the end.
 * @returns a draft with the cursor at the next grapheme boundary.
 */
export function draftAt(text: string, cursor = text.length): Draft {
  for (const segment of segmenter.segment(text)) {
    if (segment.index >= cursor) return { text, cursor: segment.index }
  }
  return { text, cursor: text.length }
}

/**
 * Move by one grapheme or to the current logical line's boundary.
 * @param draft - current text and cursor.
 * @param direction - desired cursor movement.
 * @returns the draft with its new cursor; text remains unchanged.
 */
export function moveCursor(draft: Draft, direction: 'left' | 'right' | 'home' | 'end'): Draft {
  const { text, cursor } = draft
  if (direction === 'home') return { text, cursor: text.slice(0, cursor).lastIndexOf('\n') + 1 }
  if (direction === 'end') {
    const next = text.indexOf('\n', cursor)
    return { text, cursor: next < 0 ? text.length : next }
  }
  let previous = 0
  for (const segment of segmenter.segment(text)) {
    if (direction === 'right' && segment.index > cursor) return { text, cursor: segment.index }
    if (direction === 'left' && segment.index >= cursor) return { text, cursor: previous }
    previous = segment.index
  }
  return { text, cursor: direction === 'left' ? previous : text.length }
}

/**
 * Insert literal text at the cursor; pasted newlines never submit.
 * @param draft - current text and cursor.
 * @param value - text delivered by Ink or the clipboard.
 * @returns the edited draft, with its cursor following the inserted graphemes.
 */
export function insertText(draft: Draft, value: string): Draft {
  const inserted = composerText(value)
  return draftAt(draft.text.slice(0, draft.cursor) + inserted + draft.text.slice(draft.cursor), draft.cursor + inserted.length)
}

/**
 * Delete one adjacent visible grapheme, including combining marks.
 * @param draft - current text and cursor.
 * @param direction - Backspace removes left; Delete removes right.
 * @returns the edited draft, preserving the remaining graphemes.
 */
export function eraseAtCursor(draft: Draft, direction: 'backward' | 'forward'): Draft {
  const neighbor = moveCursor(draft, direction === 'backward' ? 'left' : 'right').cursor
  const start = Math.min(neighbor, draft.cursor)
  const end = Math.max(neighbor, draft.cursor)
  return draftAt(draft.text.slice(0, start) + draft.text.slice(end), start)
}

/**
 * Remove the last visible grapheme, including its combining marks.
 * @param text - the current value.
 * @returns the text before its final grapheme.
 */
export function eraseLast(text: string): string {
  return eraseAtCursor(draftAt(text), 'backward').text
}
