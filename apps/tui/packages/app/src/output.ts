/**
 * Terminal streams that present each rendered frame in one write, drawn over
 * the previous frame instead of after erasing it.
 *
 * Ink prints `Static` output by erasing the whole dynamic region, writing the
 * new rows, and drawing the region again, as separate writes. A terminal that
 * honours synchronized output (DEC mode 2026) shows only the result. One that
 * does not — Apple Terminal, and multiplexers that do not forward the mode —
 * can present the screen between them, with the composer, the status line,
 * and the turn header gone. A streaming answer prints a row each time a line
 * finishes, so the input's frame blinks out and back once per line.
 *
 * Every write Ink makes in one synchronous render is held and written
 * together, in order, so a frame reaches the terminal as one write. Within
 * it, an erase of the dynamic region becomes a move to its top, each row that
 * follows is cleared as it is drawn, and whatever remains of the old frame
 * below the new one is cleared at the end. The final screen is the one the
 * erase would have left; at no point is more than the row being drawn blank.
 *
 * The frame also starts on the terminal's bottom row, and returns there when
 * Ink clears the screen to replay history, so the composer rests at the
 * bottom from the first frame rather than following history down to it. The
 * interface keeps it there by never drawing a frame shorter than the history
 * printed above it can make up.
 *
 * @module @dsh-tui/app/output
 */

const CSI = '\u001B['
const ERASE_LINE = `${CSI}2K`
const CLEAR_LINE = `${CSI}K`
const CLEAR_BELOW = `${CSI}J`
/** Ink's `clearTerminal`: erase the screen and its scrollback, then home the cursor. */
const CLEAR_TERMINAL = `${CSI}2J${CSI}3J${CSI}H`
/** End of a synchronized update: what it presents must already be complete. */
const END_SYNC = `${CSI}?2026l`
/** `ansi-escapes` `cursorNextLine`: down one row to its first column, without scrolling. */
const NEXT_LINE = `${CSI}E`

/**
 * `ansi-escapes` `eraseLines(n)`, as Ink's log writes it to clear the dynamic
 * region: erase a line and move up, `n - 1` times, then erase the top line
 * and return to its first column.
 */
const ERASE_REGION = /^(?:\u001B\[2K\u001B\[1A)*\u001B\[2K\u001B\[G$/

/**
 * Anything that moves the cursor between rows, clears the screen, or resets
 * the terminal. The rows after one are not the rows the erase would have
 * cleared, so an overwrite in progress closes before it.
 */
const MOVES = /\u001B\[\d*[ABEFHfJ]|\u001B\[\d*;\d*[Hf]|\u001Bc|\u001B\[\?1049[hl]/

/**
 * Join one render's writes, drawing each erased region over rather than out.
 *
 * Pure, so the rewrite can be checked against byte sequences without a
 * terminal. Writes it does not recognize pass through unchanged.
 *
 * @param chunks - the writes of one synchronous render, in order.
 * @returns the bytes to write in their place.
 */
export function overwrite(chunks: readonly string[]): string {
  let out = ''
  // Whether rows are being drawn over an erased region, which still holds
  // the previous frame below the cursor.
  let open = false
  const close = (): void => {
    if (open) out += CLEAR_BELOW
    open = false
  }
  for (const chunk of chunks) {
    if (ERASE_REGION.test(chunk)) {
      close()
      const rows = chunk.split(ERASE_LINE).length - 1
      // Cleared as the first row is drawn: erasing it after its text would
      // also erase a full-width row's last cell, where the cursor rests
      // until the next character wraps it.
      out += `${rows > 1 ? `${CSI}${rows - 1}A` : ''}${CSI}G${CLEAR_LINE}`
      open = true
    } else if (open && chunk !== END_SYNC && !MOVES.test(chunk)) {
      out += chunk.replaceAll('\n', `\n${CLEAR_LINE}`)
    } else {
      close()
      out += chunk
    }
  }
  close()
  return out
}

/**
 * Move the cursor to the bottom row, where the next frame is drawn upward from.
 * Cursor-down stops at the last row, so this never scrolls.
 * @param rows - the terminal's height.
 * @returns the move, or nothing for a stream without a height.
 */
const toBottom = (rows: number | undefined): string => rows === undefined || rows < 1 ? '' : `${CSI}${rows}B`

/**
 * Return the cursor to the bottom row after each screen clear.
 *
 * Ink clears the terminal and replays history when a frame would overflow
 * it, as it does once after a narrowing. Replayed from the top, history
 * shorter than the screen leaves the frame part way down it; replayed from
 * the bottom row, it scrolls up to meet the frame there.
 *
 * @param text - one render's bytes.
 * @param rows - the terminal's height.
 * @returns the bytes with each replay starting on the bottom row.
 */
export function anchor(text: string, rows: number | undefined): string {
  return text.replaceAll(CLEAR_TERMINAL, CLEAR_TERMINAL + toBottom(rows))
}

/**
 * Step past unchanged rows with a newline, which scrolls at the bottom row.
 *
 * Ink's incremental renderer rewrites changed rows followed by a newline but
 * steps over an unchanged row with Cursor Next Line, which stops at the
 * bottom row instead of scrolling. The frame rests on the bottom row, so when
 * it grows and an unchanged row — a blank, which the composer surface's
 * padding makes common — falls past the old bottom, that row is never added:
 * the frame is drawn a row short, Ink's line count no longer matches the
 * screen, and its next erase takes a row of history with it. Everywhere
 * above the bottom row the two moves are the same.
 *
 * @param text - one render's bytes, after {@link overwrite}, which reads
 *   Cursor Next Line as a move.
 * @returns the bytes with each Cursor Next Line written as a carriage return
 *   and newline.
 */
export const scrolling = (text: string): string => text.replaceAll(NEXT_LINE, '\r\n')

type Write = NodeJS.WriteStream['write']
type Callback = (error?: Error | null) => void

/** Stdout and stderr for Ink, sharing one queue so their order is kept. */
export interface FrameOutput {
  readonly out: NodeJS.WriteStream
  readonly err: NodeJS.WriteStream
  /** Write whatever is held now; for teardown, which cannot wait for a microtask. */
  flush(): void
}

/**
 * Wrap a terminal's streams so each render reaches it as one write.
 *
 * Writes are held until the current task finishes, which is when Ink has
 * finished writing a frame, and then written in order: consecutive writes to
 * one stream are joined, and stdout's are joined through {@link overwrite},
 * {@link anchor}, and {@link scrolling}; the first of them starts on the
 * bottom row.
 * Every other property of each stream is the stream's own, so size, TTY
 * state, and resize events reach Ink unchanged.
 *
 * @param out - the terminal's stdout.
 * @param err - the terminal's stderr, which shares the screen with it.
 * @param styles - retain SGR styling; false implements NO_COLOR for rendered
 *   text without changing cursor, erase, paste, or synchronization controls.
 * @returns the wrapped streams and a synchronous flush.
 */
export function frameOutput(out: NodeJS.WriteStream, err: NodeJS.WriteStream, styles = true): FrameOutput {
  const queue: { stream: NodeJS.WriteStream, chunk: string | Uint8Array, callback?: Callback }[] = []
  let scheduled = false
  let started = false
  const flush = (): void => {
    scheduled = false
    const held = queue.splice(0)
    for (let start = 0; start < held.length;) {
      const stream = held[start]!.stream
      let end = start
      while (end < held.length && held[end]!.stream === stream && typeof held[end]!.chunk === 'string') end++
      if (end === start) {
        // Bytes rather than text: written as they are, in their place.
        const { chunk, callback } = held[start]!
        stream.write(chunk, callback)
        start++
        continue
      }
      const run = held.slice(start, end)
      const chunks = run.map(entry => entry.chunk as string)
      const callbacks = run.flatMap(entry => entry.callback === undefined ? [] : [entry.callback])
      let text = chunks.join('')
      if (stream === out) {
        text = scrolling(anchor(overwrite(chunks), out.rows))
        if (!started) text = toBottom(out.rows) + text
        started = true
      }
      // Ink's Chalk version does not honor NO_COLOR on a TTY. Filter only
      // text styling here; stripping all ANSI would break terminal ownership.
      if (!styles) text = text.replace(/\x1b\[[\d;:]*m/g, '')
      stream.write(text, error => {
        for (const callback of callbacks) callback(error)
      })
      start = end
    }
  }
  const wrap = (stream: NodeJS.WriteStream): NodeJS.WriteStream => {
    const write = ((chunk: string | Uint8Array, encoding?: BufferEncoding | Callback, callback?: Callback): boolean => {
      const done = typeof encoding === 'function' ? encoding : callback
      queue.push({ stream, chunk, ...done === undefined ? {} : { callback: done } })
      if (!scheduled) {
        scheduled = true
        queueMicrotask(flush)
      }
      return true
    }) as Write
    return new Proxy(stream, {
      get(target, property) {
        if (property === 'write') return write
        const value: unknown = Reflect.get(target, property, target)
        return typeof value === 'function' ? value.bind(target) : value
      },
      set: (target, property, value) => Reflect.set(target, property, value, target),
    })
  }
  return { out: wrap(out), err: wrap(err), flush }
}
