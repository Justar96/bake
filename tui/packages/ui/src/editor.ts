/** Text editing helpers; Ink owns key and bracketed-paste decoding. */

/**
 * Remove terminal controls while retaining pasted line breaks and tabs.
 * @param text - text delivered by Ink.
 * @returns text safe to display in the composer.
 */
export function composerText(text: string): string {
  return text.replace(/\r\n?/g, '\n').replace(/[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f-\u009f]/g, '')
}

/**
 * Remove the last visible grapheme, including its combining marks.
 * @param text - the current composer value.
 * @returns the text before its final grapheme.
 */
export function eraseLast(text: string): string {
  const segments = [...new Intl.Segmenter(undefined, { granularity: 'grapheme' }).segment(text)]
  return text.slice(0, segments.at(-1)?.index ?? 0)
}
