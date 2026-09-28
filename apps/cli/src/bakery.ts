/** A single terminal row shared by updates and the standalone installers. */
import { LOAF_FRAME_MS, loafDone, loafFrame, loafGlyphsFor, loafLine } from '@dsh-tui/ui/loaf.ts'

export interface BakeryTerminal {
  readonly isTTY?: boolean
  readonly columns?: number
  write(text: string): unknown
}

/** Doneness a step without its own progress creeps toward, so the loaf never looks finished early. */
const CREEP_LIMIT = 0.8
/** Milliseconds for that creep to get most of the way there. */
const CREEP_MS = 20_000

/**
 * Bake a loaf on one row, in place, without scrolling or changing cursor
 * visibility: steam rises while the loaf browns with the install's progress.
 * A stage without a level keeps browning slowly on its own. The owner must
 * call finish on success, failure, and cancellation.
 * @param terminal - where the row is drawn.
 * @param env - environment deciding whether to animate, colour, and draw Unicode.
 * @param initial - the first label.
 */
export function startBakery(terminal: BakeryTerminal, env: Record<string, string | undefined>, initial: string) {
  const enabled = terminal.isTTY === true && env['TERM'] !== 'dumb'
    && !env['CI'] && env['BAKE_NO_ANIMATION'] !== '1'
  const color = env['NO_COLOR'] === undefined
  const glyphs = loafGlyphsFor(env)
  const started = Date.now()
  let label = initial
  let level: number | undefined
  let stopped = false
  let drawn = false
  // Some ptys report zero columns; treat that as unknown rather than drawing nothing.
  const width = () => (terminal.columns !== undefined && terminal.columns > 0 ? terminal.columns : 80) - 1
  const draw = () => {
    const elapsed = Date.now() - started
    const doneness = level ?? CREEP_LIMIT * (1 - Math.exp(-elapsed / CREEP_MS))
    terminal.write(`\r\x1b[2K${loafLine(loafFrame(elapsed, doneness, glyphs), label, { color, width: width() })}`)
    drawn = true
  }
  if (enabled) draw()
  const timer = enabled ? setInterval(draw, LOAF_FRAME_MS) : undefined
  timer?.unref()
  return {
    /**
     * Change the operation label without adding a scrollback line.
     * @param next - what the install is doing.
     * @param doneness - how brown the loaf is, from 0 to 1; absent, it keeps creeping.
     */
    stage(next: string, doneness?: number): void {
      if (stopped) return
      label = next
      if (doneness !== undefined) level = doneness
    },
    /** Clear the live row before diagnostics; leave the baked loaf only on success. */
    finish(success = false): void {
      if (stopped) return
      stopped = true
      clearInterval(timer)
      if (!drawn) return
      terminal.write('\r\x1b[2K')
      if (success) terminal.write(`${loafLine(loafDone(glyphs), 'Freshly baked.', { color, width: width() })}\n`)
    },
  }
}
