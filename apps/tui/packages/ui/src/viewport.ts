/** A lazily measured transcript window. Only visible rows enter the Ink tree. */
import type { Budget } from './layout.ts'
import { lineHeight, wrappedRows, wrappedStarts } from './line.tsx'
import { present, type PresentedLine, type ResultBound } from './present.ts'
import type { Row } from './rows.ts'
import type { Transcript } from './transcript.ts'

/** A source row and the terminal-row offset within its presentation. */
export interface Position { readonly row: number, readonly offset: number }

/** A character in a displayed source row, retained while following is paused. */
export interface ReadingAnchor extends Position {
  readonly source: Row | undefined
  readonly line: number
  readonly character: number
  readonly columns: number
}

/** The portion of one presentation line intersecting the viewport. */
export interface VisibleLine {
  readonly key: string
  readonly line: PresentedLine
  readonly offset: number
  readonly height: number
}

interface Measured {
  readonly lines: readonly PresentedLine[]
  readonly heights: readonly number[]
  readonly height: number
}

/**
 * An index of source references with a bounded cache of presented rows.
 * Appends read only new batches; scrolling measures only rows it visits.
 * Create one per displayed session. Geometry is invalidated on width changes.
 */
export class Viewport {
  private source: Transcript | undefined
  private readonly rows: Row[] = []
  private live: readonly Row[] = []
  private readonly cache = new Map<Row, Measured>()
  private columns = 0
  private result: ResultBound | undefined

  constructor(heading: string) {
    this.rows.push({ kind: 'notice', tone: 'info', text: heading })
  }

  /** Capture an append or replacement without retaining a second copy of row text. */
  update(transcript: Transcript, live: readonly Row[]): void {
    if (transcript !== this.source) {
      const batches: Transcript[] = []
      let cursor: Transcript | undefined = transcript
      while (cursor !== undefined && cursor !== this.source) {
        batches.push(cursor)
        cursor = cursor.previous
      }
      if (cursor !== this.source) { this.rows.length = 1; this.cache.clear() }
      for (let index = batches.length - 1; index >= 0; index--) {
        for (const row of batches[index]!.rows) this.rows.push(row)
      }
      this.source = transcript
    }
    if (this.live !== live) {
      const retained = new Set(live)
      for (const row of this.live) if (!retained.has(row)) this.cache.delete(row)
    }
    this.live = live
  }

  /** The end sentinel is also the starting point for following the newest output. */
  get end(): Position { return { row: this.rows.length + this.live.length, offset: 0 } }

  /** Change width or presentation policy before measuring or moving. */
  configure(budget: Budget, result: ResultBound): void {
    if (budget.columns !== this.columns || result !== this.result) this.cache.clear()
    this.columns = budget.columns
    this.result = result
  }

  /** Move by physical terminal rows, clamping at either end of the transcript. */
  move(position: Position, distance: number, budget: Budget, result: ResultBound): Position {
    let row = Math.min(position.row, this.end.row)
    let offset = position.offset + distance
    while (offset < 0 && row > 0) {
      row--
      offset += this.measure(row, budget, result).height
    }
    while (row < this.end.row) {
      const height = this.measure(row, budget, result).height
      if (offset < height) break
      offset -= height
      row++
    }
    return { row, offset: row === this.end.row ? 0 : Math.max(0, offset) }
  }

  /** Capture a text position so rewrapping keeps the same part of a message visible. */
  anchor(position: Position, budget: Budget, result: ResultBound): ReadingAnchor {
    if (position.row === this.end.row) return { ...position, source: undefined, line: 0, character: 0, columns: budget.columns }
    const measured = this.measure(position.row, budget, result)
    let offset = position.offset
    let line = 0
    while (line < measured.lines.length - 1 && offset >= measured.heights[line]!) offset -= measured.heights[line++]!
    const content = measured.lines[line]
    return { ...position, source: this.rowAt(position.row), line, columns: budget.columns,
      character: content === undefined ? 0 : wrappedStarts(content, budget)[offset] ?? 0 }
  }

  /** Resolve a reading anchor, including live text split into committed fragments. */
  locate(anchor: ReadingAnchor, budget: Budget, result: ResultBound): Position {
    const row = Math.min(anchor.row, this.end.row)
    if (row === this.end.row) return { row, offset: 0 }
    if (anchor.source === undefined) return this.move({ row, offset: 0 }, 0, budget, result)
    if (this.rowAt(row) !== anchor.source) {
      // A resize and a stream publication can share a render. Convert the old
      // source's text anchor to the new width before walking its replacement.
      const offset = anchor.columns === budget.columns ? anchor.offset
        : this.offsetOf(anchor, this.measureRow(anchor.source, budget, result), budget)
      this.cache.delete(anchor.source)
      return this.move({ row, offset }, 0, budget, result)
    }
    return { row, offset: this.offsetOf(anchor, this.measure(row, budget, result), budget) }
  }

  private offsetOf(anchor: ReadingAnchor, measured: Measured, budget: Budget): number {
    const index = Math.min(anchor.line, measured.lines.length - 1)
    const content = measured.lines[index]
    if (content === undefined) return 0
    const starts = wrappedStarts(content, budget)
    const wrapped = Math.max(0, starts.findLastIndex(start => start <= anchor.character))
    return measured.heights.slice(0, index).reduce((sum, value) => sum + value, 0) + wrapped
  }

  /** Present at most `height` physical rows from a reading position. */
  window(position: Position, height: number, budget: Budget, result: ResultBound): VisibleLine[] {
    const visible: VisibleLine[] = []
    let remaining = height
    let skip = position.offset
    for (let row = position.row; row < this.end.row && remaining > 0; row++) {
      const measured = this.measure(row, budget, result)
      for (let index = 0; index < measured.lines.length && remaining > 0; index++) {
        const rows = measured.heights[index]!
        if (skip >= rows) { skip -= rows; continue }
        const count = Math.min(remaining, rows - skip)
        visible.push({ key: `${row}:${index}`, line: measured.lines[index]!, offset: skip, height: count })
        remaining -= count
        skip = 0
      }
    }
    return visible
  }

  private measure(index: number, budget: Budget, result: ResultBound): Measured {
    return this.measureRow(this.rowAt(index)!, budget, result)
  }

  private measureRow(row: Row, budget: Budget, result: ResultBound): Measured {
    const cached = this.cache.get(row)
    if (cached !== undefined) {
      this.cache.delete(row)
      this.cache.set(row, cached)
      return cached
    }
    const lines = present(row, result, line => wrappedRows(line, budget))
    const heights = lines.map(line => lineHeight(line, budget))
    const measured = { lines, heights, height: heights.reduce((sum, value) => sum + value, 0) }
    this.cache.set(row, measured)
    if (this.cache.size > 32) this.cache.delete(this.cache.keys().next().value!)
    return measured
  }

  private rowAt(index: number): Row | undefined {
    return index < this.rows.length ? this.rows[index] : this.live[index - this.rows.length]
  }
}
