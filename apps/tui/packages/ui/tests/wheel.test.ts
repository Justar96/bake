/** Wheel reports move one row alone and more in a fast spin, unless the terminal accelerates them itself. */
import { describe, expect, test } from 'bun:test'
import { WheelSteps, wheelReports } from '../src/wheel.ts'

/** Rows for reports at each time, all in one direction. */
const spin = (steps: WheelSteps, times: readonly number[], direction: -1 | 1 = -1): number[] =>
  times.map(time => steps.rows(direction, time))

describe('WheelSteps', () => {
  test('moves one row for a notch on its own, and for every report without a time', () => {
    const steps = new WheelSteps('notches')
    expect(spin(steps, [0, 1000, 2000])).toEqual([1, 1, 1])
    expect([steps.rows(1, undefined), steps.rows(1, undefined)]).toEqual([1, 1])
  })

  test('accelerates with the speed of a spin, up to six rows a report', () => {
    expect(spin(new WheelSteps('notches'), [0, 50, 100, 150])).toEqual([1, 2, 2, 2])
    expect(spin(new WheelSteps('notches'), [0, 20, 40, 60])).toEqual([1, 5, 5, 5])
    expect(spin(new WheelSteps('notches'), [0, 8, 16, 24])).toEqual([1, 6, 6, 6])
  })

  test('carries fractions, so a steady spin moves an even distance', () => {
    // 100 / 60 ms is two thirds of a row past one each report.
    const rows = spin(new WheelSteps('notches'), [0, 60, 120, 180, 240])
    expect(rows).toEqual([1, 1, 2, 2, 1])
    expect(rows.slice(1).reduce((sum, value) => sum + value, 0)).toBe(Math.floor(4 * 100 / 60))
  })

  test('starts over after a pause or a change of direction, and does not accelerate a split notch', () => {
    const steps = new WheelSteps('notches')
    expect(spin(steps, [0, 20, 40])).toEqual([1, 5, 5])
    expect(steps.rows(-1, 400)).toBe(1)
    expect(steps.rows(-1, 420)).toBe(5)
    expect(steps.rows(1, 440)).toBe(1)
    expect(steps.rows(1, 442)).toBe(1)
  })

  test('moves one row a report where the terminal accelerates the wheel itself', () => {
    expect(spin(new WheelSteps('lines'), [0, 8, 16, 24])).toEqual([1, 1, 1, 1])
  })
})

describe('wheelReports', () => {
  test('reads a local macOS terminal as already accelerated', () => {
    expect(wheelReports({}, 'darwin')).toBe('lines')
  })

  test('reads other platforms, and macOS over SSH, as one report per notch', () => {
    expect(wheelReports({}, 'linux')).toBe('notches')
    expect(wheelReports({}, 'win32')).toBe('notches')
    for (const name of ['SSH_CONNECTION', 'SSH_CLIENT', 'SSH_TTY']) {
      expect(wheelReports({ [name]: 'remote' }, 'darwin')).toBe('notches')
    }
  })
})
