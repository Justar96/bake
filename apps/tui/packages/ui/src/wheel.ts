/**
 * How far one mouse-wheel report scrolls the fullscreen transcript or an open
 * sheet.
 *
 * Terminals report the wheel in one of two ways. A local macOS terminal sends
 * one report per line, already accelerated by the system, so each report
 * moves one row. Elsewhere, and over SSH, where the client is unknown, a
 * terminal sends one report per notch. A single notch then moves one row, and
 * a fast spin moves more rows per report, up to six.
 *
 * Adapted from the `WheelScrollAccelerator` of pi (MIT, Mario Zechner,
 * https://github.com/earendil-works/pi).
 *
 * @module @dsh-tui/ui/wheel
 */

/** One report per line, or one per notch. */
export type WheelReports = 'lines' | 'notches'

/**
 * Reports closer together than this are one notch the terminal split (Ghostty
 * sends them about 4 ms apart) or a high-resolution device. Each moves one row
 * and does not accelerate.
 */
const BURST_MS = 5
/** A longer pause, or a change of direction, ends a spin. */
const SPIN_MS = 200
/** The average gap at which a report moves one row. Shorter gaps move proportionally more. */
const REFERENCE_MS = 100
/** The most rows one accelerated report moves. */
const MAX_ROWS = 6

/** How many times as far a report moves while Alt is held. */
export const ALT_WHEEL_FACTOR = 5

/**
 * Decide how this terminal reports the wheel.
 * @param env - the process environment; only the SSH variables are read.
 * @param platform - the operating system, as `process.platform` names it.
 * @returns `lines` for a local macOS terminal, otherwise `notches`.
 */
export function wheelReports(env: Readonly<Partial<Record<string, string>>>, platform: string): WheelReports {
  const remote = env['SSH_CONNECTION'] !== undefined || env['SSH_CLIENT'] !== undefined || env['SSH_TTY'] !== undefined
  return platform === 'darwin' && !remote ? 'lines' : 'notches'
}

/**
 * Rows per wheel report, following the speed of a spin. Notches 100 ms apart
 * move one row each, 50 ms apart two, and 20 ms apart five. Fractions carry
 * into the next report, so a steady spin moves an even distance.
 */
export class WheelSteps {
  private last = Number.NEGATIVE_INFINITY
  private direction = 0
  private gap: number | undefined
  private carry = 0

  /** @param reports - how the terminal reports the wheel. */
  constructor(private readonly reports: WheelReports) {}

  /**
   * @param direction - toward older output (-1) or newer (1).
   * @param now - the report's time in milliseconds; absent, each report stands alone.
   * @returns the rows this report moves, at least one.
   */
  rows(direction: -1 | 1, now: number | undefined): number {
    if (this.reports === 'lines' || now === undefined) return 1
    const gap = now - this.last
    const spinning = direction === this.direction && gap <= SPIN_MS
    this.last = now
    this.direction = direction
    if (!spinning) {
      this.gap = undefined
      this.carry = 0
      return 1
    }
    if (gap < BURST_MS) return 1
    this.gap = this.gap === undefined ? gap : (this.gap + gap) / 2
    const rows = Math.min(MAX_ROWS, Math.max(1, REFERENCE_MS / this.gap)) + this.carry
    const whole = Math.floor(rows)
    this.carry = rows - whole
    return whole
  }
}
