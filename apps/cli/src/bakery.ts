/** A single terminal row shared by updates and the standalone installers. */

export interface BakeryTerminal {
  readonly isTTY?: boolean
  readonly columns?: number
  write(text: string): unknown
}

/**
 * Animate an ASCII brick oven without scrolling or changing cursor visibility.
 * The owner must call finish on success, failure, and cancellation.
 */
export function startBakery(terminal: BakeryTerminal, env: Record<string, string | undefined>, initial: string) {
  const enabled = terminal.isTTY === true && env['TERM'] !== 'dumb'
    && !env['CI'] && env['BAKE_NO_ANIMATION'] !== '1'
  const color = env['NO_COLOR'] === undefined
  let label = initial
  let tick = 0
  let stopped = false
  let drawn = false
  const ovens = ['[##  _____  ##]', '[##  (___)  ##]', '[## (~~~~~) ##]', '[## (~~~~~) ##]']
  const heat = ['. . ', ' : .', '. : ', ' . :']
  // Some ptys report zero columns; treat that as unknown rather than drawing nothing.
  const width = () => (terminal.columns !== undefined && terminal.columns > 0 ? terminal.columns : 80) - 1
  const draw = () => {
    const frame = Math.floor(tick++ / 3) % ovens.length
    const text = `  ${ovens[frame]} ${heat[tick % heat.length]} ${label}`.slice(0, width())
    terminal.write(`\r\x1b[2K${color ? '\x1b[38;5;215m' : ''}${text}${color ? '\x1b[0m' : ''}`)
    drawn = true
  }
  if (enabled) draw()
  const timer = enabled ? setInterval(draw, 180) : undefined
  timer?.unref()
  return {
    /** Change the operation label without adding a scrollback line. */
    stage(next: string): void { if (!stopped) label = next },
    /** Clear the live row before diagnostics; leave a warm loaf only on success. */
    finish(success = false): void {
      if (stopped) return
      stopped = true
      clearInterval(timer)
      if (!drawn) return
      terminal.write('\r\x1b[2K')
      if (success) {
        const text = '  (~~~~~)  Freshly baked.'.slice(0, width())
        terminal.write(`${color ? '\x1b[38;5;215m' : ''}${text}${color ? '\x1b[0m' : ''}\n`)
      }
    },
  }
}
