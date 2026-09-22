/**
 * Streamed text printed to the transcript as it completes, ahead of its commit.
 *
 * A streaming answer drawn whole in the live region either outgrows the window
 * and is shown clipped, or grows the frame Ink rewrites on every token. Printed
 * a line at a time instead, it scrolls into the terminal's own history the way
 * the terminal would scroll it, and the frame keeps only the line still
 * arriving. That frame is small and nearly constant, so the composer under it
 * rests where it is.
 *
 * Printing is display, not record: the session log commits the message as it
 * always did, and {@link Printed.reconcile} drops from that commit whatever
 * already printed, so each line appears once. What printed from an attempt
 * that never commits stays in scrollback, which cannot be unwritten; the
 * controller follows it with a notice saying it was discarded.
 *
 * @module @dsh-tui/app/printed
 */

import type { Row } from '@dsh-tui/ui'
import type { KeyedRow } from './live.ts'

/** Text printed from one block, in stream order. */
interface Print {
  readonly key: number
  readonly kind: 'assistant' | 'reasoning'
  /** Characters of the block's text already printed, the newline ending them included. */
  consumed: number
  /** The printed prefix, compared against the committed text. */
  text: string
}

/** What one attempt has printed, and what it still leaves to the live region. */
export class Printed {
  private readonly prints: Print[] = []

  /** Whether anything from this attempt reached the transcript. */
  get any(): boolean { return this.prints.some(print => print.consumed > 0) }

  /**
   * Split the attempt's rows into lines that are complete and lines still arriving.
   *
   * Every text or reasoning block followed by another block is complete. The
   * last block is still streaming: an answer prints up to its last newline,
   * and reasoning stays whole in the live region, where the header's ticker
   * shows it. Nothing past the first tool call prints, because the call
   * commits through its own event, and printing text after it would put that
   * text above the call.
   *
   * @param rows - the attempt's rows, keyed by block, in stream order.
   * @returns rows to append to the transcript, and rows the live region draws.
   */
  split(rows: readonly KeyedRow[]): { readonly print: readonly Row[], readonly live: readonly Row[] } {
    const print: Row[] = []
    const live: Row[] = []
    let open = true
    for (const [index, { key, row }] of rows.entries()) {
      if (!open || (row.kind !== 'assistant' && row.kind !== 'reasoning')) {
        open = false
        live.push(row)
        continue
      }
      const record = this.record(key, row.kind)
      const complete = index < rows.length - 1
      const end = complete ? row.text.length : row.kind === 'assistant' ? row.text.lastIndexOf('\n') + 1 : 0
      // A run of whitespace waits for the text after it, so a paragraph break
      // prints with the paragraph it opens rather than as a row of its own.
      if (end > record.consumed && row.text.slice(record.consumed, end).trim() !== '') {
        print.push(this.take(record, row, end))
      } else if (complete) {
        record.consumed = row.text.length
      }
      const rest = row.text.slice(record.consumed)
      // The streaming answer keeps a row even while it holds no text, so the
      // header still reads `writing` between one line and the next.
      if (!complete) live.push(record.consumed === 0 ? row : { ...row, text: rest, continued: true })
    }
    return { print, live }
  }

  /**
   * Drop from a committed message what already printed.
   *
   * Matched by kind and prefix, in order. A block the commit changed — an
   * adapter's `block-end` may correct its deltas — prints again whole: once
   * too often is the failure that stays readable.
   *
   * @param rows - the message's projected rows.
   * @returns the rows still to append.
   */
  reconcile(rows: readonly Row[]): readonly Row[] {
    const prints = this.prints.filter(print => print.consumed > 0)
    let next = 0
    return rows.flatMap((row): Row[] => {
      const print = prints[next]
      if (print === undefined || print.kind !== row.kind || (row.kind !== 'assistant' && row.kind !== 'reasoning')) return [row]
      // The commit may drop the newline that ended the last printed line.
      if (!row.text.startsWith(print.text) && row.text !== print.text.slice(0, -1)) return [row]
      next++
      const rest = row.text.slice(print.text.length)
      return rest.trim() === '' ? [] : [{ ...row, text: rest, continued: true }]
    })
  }

  private record(key: number, kind: Print['kind']): Print {
    const existing = this.prints.find(print => print.key === key)
    if (existing !== undefined) return existing
    const record: Print = { key, kind, consumed: 0, text: '' }
    this.prints.push(record)
    return record
  }

  private take(record: Print, row: Row & { readonly kind: Print['kind'] }, end: number): Row {
    const first = record.consumed === 0
    // The newline ending a printed line is consumed with it, so what follows
    // starts on the next line; a newline after it is a paragraph break, and
    // is kept as the blank row it draws.
    const text = row.text.slice(record.consumed, row.text[end - 1] === '\n' ? end - 1 : end)
    record.consumed = end
    record.text = row.text.slice(0, end)
    return first ? { kind: row.kind, text } : { kind: row.kind, text, continued: true }
  }
}
