/** Install progress on a terminal, shared by `bake update` and the standalone installers. */
import {
  ansi, colorDepthFor, formatSeconds, headerLine, liveRow, nextLine, PROGRESS_FRAME_MS, progressGlyphsFor, stepLine, summaryLine,
  type Run,
} from 'bake-tui-ui/install-progress.ts'

export interface ProgressTerminal {
  readonly isTTY?: boolean
  readonly columns?: number
  write(text: string): unknown
}

/** The closing lines of a finished install. */
export interface ProgressSummary {
  /** Bold, followed by how long the install took: `Bake 0.2.0 installed`. */
  readonly title: string
  /** Lines under it, such as `Run: bake`. */
  readonly next?: readonly string[]
}

/** Cells a step's label is padded to, wide enough for the installers' steps, so meters and notes line up. */
const LABEL_WIDTH = 18
/** Every line starts two cells in, clear of the shell prompt's column. */
const INDENT = '  '

interface Running {
  readonly label: string
  readonly done: string
  readonly started: number
  readonly notes: string[]
  fraction: number | undefined
  detail: string | undefined
}

/**
 * Draw an install as it runs: a heading, then one line for each step as it
 * finishes, under a live row redrawn in place for the step running. Only the
 * live row is ever redrawn, so resizing, scrollback, and Windows consoles
 * keep every printed line. The cursor stays visible. Nothing is drawn, and
 * every call does nothing, unless the terminal is a TTY that is not `dumb`,
 * outside CI, without `BAKE_NO_ANIMATION=1`. The owner calls `finish`,
 * `fail`, or `stop` on every path.
 * @param terminal - where the rows are drawn.
 * @param env - environment deciding whether to animate, the colours, and the glyphs.
 * @param heading - what the heading names after Bake: `installer` and the platform.
 */
export function startProgress(terminal: ProgressTerminal, env: Record<string, string | undefined>, heading: readonly string[]) {
  const animated = terminal.isTTY === true && env['TERM'] !== 'dumb'
    && !env['CI'] && env['BAKE_NO_ANIMATION'] !== '1'
  const depth = colorDepthFor(env)
  const glyphs = progressGlyphsFor(env)
  const started = Date.now()
  let running: Running | undefined
  let stopped = !animated
  // Some ptys report zero columns; treat that as unknown rather than drawing nothing.
  const width = (): number => (terminal.columns !== undefined && terminal.columns > 0 ? terminal.columns : 80) - 1 - INDENT.length
  const line = (runs: readonly Run[]): string => `${INDENT}${ansi(runs, depth)}`
  const draw = (): void => {
    if (running === undefined) return
    const now = Date.now()
    terminal.write(`\r\x1b[2K${line(liveRow({ label: running.label,
      ...running.fraction === undefined ? {} : { fraction: running.fraction },
      ...running.detail === undefined ? {} : { detail: running.detail },
    }, { time: now - started, width: width(), glyphs, labelWidth: LABEL_WIDTH, elapsed: now - running.started }))}`)
  }
  const settle = (outcome: 'done' | 'failed'): void => {
    if (running === undefined) return
    const notes = outcome === 'done' ? [...running.notes, formatSeconds(Date.now() - running.started)] : []
    terminal.write(`\r\x1b[2K${line(stepLine(outcome, outcome === 'done' ? running.done : running.label, notes,
      { width: width(), glyphs, labelWidth: LABEL_WIDTH }))}\n`)
    running = undefined
  }
  if (animated) terminal.write(`${line(headerLine(heading, width(), glyphs))}\n\n`)
  const timer = animated ? setInterval(draw, PROGRESS_FRAME_MS) : undefined
  timer?.unref()
  const end = (): boolean => {
    if (stopped) return false
    stopped = true
    clearInterval(timer)
    return true
  }
  return {
    /** Whether anything is drawn; a caller prints its own plain report otherwise. */
    animated,
    /**
     * Start a step, printing the one running as finished.
     * @param label - what it is doing: `Downloading`.
     * @param done - what its line says once it finishes: `Downloaded`.
     */
    step(label: string, done: string): void {
      if (stopped) return
      settle('done')
      running = { label, done, started: Date.now(), notes: [], fraction: undefined, detail: undefined }
      draw()
    },
    /**
     * Add facts to the running step's line, before its duration.
     * @param notes - such as the release's version, or where it was installed.
     */
    note(...notes: readonly string[]): void {
      if (!stopped) running?.notes.push(...notes)
    },
    /**
     * Say how far the running step is; without a fraction, its meter runs a comet.
     * @param fraction - from 0 to 1, or undefined when its length is not known.
     * @param detail - dim text after the meter, such as bytes received.
     */
    progress(fraction: number | undefined, detail?: string): void {
      if (stopped || running === undefined) return
      running.fraction = fraction
      running.detail = detail
    },
    /**
     * Print the running step as finished, then the summary.
     * @param summary - the closing lines.
     */
    finish(summary: ProgressSummary): void {
      if (!end()) return
      settle('done')
      terminal.write(`\n${line(summaryLine(summary.title, Date.now() - started, width(), glyphs))}\n`)
      for (const next of summary.next ?? []) terminal.write(`${line(nextLine(next, width(), glyphs))}\n`)
    },
    /** Print the running step as failed, before the caller's diagnostics. */
    fail(): void {
      if (end()) settle('failed')
    },
    /** Clear the live row, and print nothing more; safe after `finish` or `fail`. */
    stop(): void {
      if (end() && running !== undefined) terminal.write('\r\x1b[2K')
    },
  }
}
