/** Immutable transcript snapshots sharing append-only batches of projected rows. */
import type { Row } from './rows.ts'

/** One transcript version; extending it retains earlier versions unchanged. */
export interface Transcript {
  readonly length: number
  readonly rows: readonly Row[]
  readonly previous?: Transcript
}

/** Empty history shared by fresh terminal views. */
export const emptyTranscript: Transcript = { length: 0, rows: [] }

/**
 * Extend history without reading or copying its committed prefix.
 * @param previous - preceding immutable version.
 * @param rows - new immutable rows, in display order; must not be mutated afterward.
 * @returns the extended version, or the same version for an empty append.
 */
export function appendTranscript(previous: Transcript, rows: readonly Row[]): Transcript {
  return rows.length === 0 ? previous : { length: previous.length + rows.length, rows, previous }
}

/**
 * Read only rows following a previously rendered count.
 * @param transcript - current immutable version.
 * @param start - zero-based row offset, defaulting to the entire transcript.
 * @returns rows in display order; work is proportional to the unread suffix.
 */
export function transcriptRows(transcript: Transcript, start = 0): Row[] {
  const batches: Transcript[] = []
  for (let batch: Transcript | undefined = transcript; batch !== undefined && batch.length > start; batch = batch.previous) {
    batches.push(batch)
  }
  const rows: Row[] = []
  for (let index = batches.length - 1; index >= 0; index--) {
    const batch = batches[index]!
    const offset = Math.max(0, start - (batch.length - batch.rows.length))
    for (let item = offset; item < batch.rows.length; item++) rows.push(batch.rows[item]!)
  }
  return rows
}
