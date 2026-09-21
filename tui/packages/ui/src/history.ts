/** On-demand input recall from immutable transcript rows and the current inbox view. */
import type { Transcript } from './transcript.ts'

/**
 * Visit human input newest first without rescanning or copying the complete transcript.
 * @param transcript - the session's committed presentation snapshot.
 * @param pending - projected human inbox messages in display order.
 * @returns a lazy traversal; plugin context, assistant output, and unlogged secrets are absent.
 */
export function* inputHistory(transcript: Transcript, pending: readonly { readonly text: string }[]): Generator<string> {
  for (let index = pending.length - 1; index >= 0; index--) {
    if (pending[index]!.text.trim() !== '') yield pending[index]!.text
  }
  for (let batch: Transcript | undefined = transcript; batch !== undefined; batch = batch.previous) {
    for (let index = batch.rows.length - 1; index >= 0; index--) {
      const row = batch.rows[index]!
      if (row.kind === 'user' && row.text.trim() !== '') yield row.text
    }
  }
}
