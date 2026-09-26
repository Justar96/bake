/** Bounded presentation batches over append-only transcript snapshots. */
import type { Budget } from './layout.ts'
import { lineHeight, wrappedRows } from './line.tsx'
import { present, type PresentedLine, type ResultBound } from './present.ts'
import type { Row } from './rows.ts'
import type { Transcript } from './transcript.ts'

/** Lines admitted together to one persistent Ink Static instance. */
export interface ReplayBatch {
  /** Number of presentation lines admitted before this batch, excluding the session heading. */
  readonly start: number
  readonly lines: readonly PresentedLine[]
  /** Terminal rows occupied by this batch at its admission width. */
  readonly height: number
}

const MAX_LINES = 512
const MAX_HEIGHT = 1024
const MAX_TEXT = 128 * 1024

/**
 * Read each transcript row once and retain only its unfinished presentation.
 * Create a new cursor when the displayed session or its committed history is replaced.
 */
export class ReplayCursor {
  private readonly batches: (readonly Row[])[] = []
  private captured = 0
  private row = 0
  private pending: readonly PresentedLine[] = []
  private line = 0
  private admitted = 0

  /** Whether every line of the captured snapshot has been admitted, regardless of stdout drainage. */
  get caughtUp(): boolean { return this.batches.length === 0 && this.line === this.pending.length }

  /**
   * Consume the next display batch. The caller must print it before advancing again.
   *
   * Appends join the cursor after its captured snapshot is consumed. Presentation
   * uses the supplied result bound when a row is first read; remaining lines are
   * measured at the width supplied to each call. One line larger than either the
   * height or text limit is returned alone so that replay always makes progress.
   *
   * @param transcript - the current snapshot, extending every preceding one.
   * @param budget - geometry for the next batch.
   * @param result - tool and reasoning preview bounds and syntax highlighting.
   * @returns at most 512 presentation lines, or undefined when no unprinted lines remain.
   */
  next(transcript: Transcript, budget: Budget, result: ResultBound): ReplayBatch | undefined {
    const lines: PresentedLine[] = []
    let height = 0
    let text = 0
    while (lines.length < MAX_LINES && height < MAX_HEIGHT && text < MAX_TEXT) {
      if (this.line === this.pending.length) {
        this.pending = []
        this.line = 0
        const row = this.nextRow(transcript)
        if (row === undefined) break
        this.pending = present(row, result, line => wrappedRows(line, budget))
        if (this.pending.length === 0) continue
      }
      const line = this.pending[this.line]!
      const rows = lineHeight(line, budget)
      const units = line.text.length + line.marker.length + line.verb.length + (line.gutter?.length ?? 0)
      if (lines.length > 0 && (height + rows > MAX_HEIGHT || text + units > MAX_TEXT)) break
      lines.push(line)
      height += rows
      text += units
      this.line++
    }
    if (this.line === this.pending.length) {
      this.pending = []
      this.line = 0
    }
    if (lines.length === 0) return undefined
    const start = this.admitted
    this.admitted += lines.length
    return { start, lines, height }
  }

  private nextRow(transcript: Transcript): Row | undefined {
    if (this.batches.length === 0) {
      if (transcript.length <= this.captured) return undefined
      for (let batch: Transcript | undefined = transcript; batch !== undefined && batch.length > this.captured; batch = batch.previous) {
        this.batches.push(batch.rows)
      }
      this.captured = transcript.length
    }
    const rows = this.batches.at(-1)!
    const row = rows[this.row++]!
    if (this.row === rows.length) {
      this.batches.pop()
      this.row = 0
    }
    return row
  }
}
