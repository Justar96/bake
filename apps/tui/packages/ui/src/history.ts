/** On-demand input recall from immutable transcript rows and the current inbox view. */
import type { Transcript } from './transcript.ts'

/**
 * Visit human input newest first without rescanning or copying the complete transcript.
 *
 * Commands are recalled alongside prompts. Logged arguments reconstruct their
 * line; redacted commands require a process-local submitted line. A redacted
 * line without one is skipped, including after session resume. Login lines
 * with arguments never receive a local recall line.
 *
 * @param transcript - the session's committed presentation snapshot.
 * @param pending - projected human inbox messages in display order.
 * @returns a lazy traversal. Plugin context, assistant output, and login arguments are absent.
 */
export function* inputHistory(transcript: Transcript, pending: readonly { readonly text: string }[]): Generator<string> {
  for (let index = pending.length - 1; index >= 0; index--) {
    if (pending[index]!.text.trim() !== '') yield pending[index]!.text
  }
  for (let batch: Transcript | undefined = transcript; batch !== undefined; batch = batch.previous) {
    for (let index = batch.rows.length - 1; index >= 0; index--) {
      const row = batch.rows[index]!
      if (row.kind === 'command') {
        if (row.recall !== undefined) yield row.recall
        else if (row.inputOmitted !== true) yield `/${row.name}${row.args}`
      }
      else if (row.kind === 'user' && row.text.trim() !== '') yield row.text
    }
  }
}

/**
 * Where the caret lands in an entry recall has just shown.
 *
 * Inside a draft of several rows, Up and Down first move the caret between
 * rows, and recall only from the first or the last row. An older entry opens
 * with the caret at its start, on its first row, so the next Up keeps walking
 * back instead of climbing through the entry. A newer one opens at its end, on
 * its last row, for the next Down. A one-row entry is both, so its caret stays
 * where it was: at the end, where typing appends, unless an earlier visit in
 * the same browse moved it.
 *
 * @param entry - the recalled text and the caret it was loaded or left with.
 * @param direction - older for Up, newer for Down.
 * @param rows - screen rows the entry occupies in the composer.
 * @returns the caret's UTF-16 offset in the entry's text.
 */
export function recallCursor(entry: { readonly text: string, readonly cursor: number }, direction: 'older' | 'newer', rows: number): number {
  if (rows <= 1) return entry.cursor
  return direction === 'older' ? 0 : entry.text.length
}
