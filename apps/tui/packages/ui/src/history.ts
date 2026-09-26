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
