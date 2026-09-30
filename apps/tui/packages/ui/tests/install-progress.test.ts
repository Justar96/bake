/** The install rows shared by `/update`, `bake update`, and the installers: their motion, layout, glyphs, and colours. */
import { describe, expect, test } from 'bun:test'
import stringWidth from 'string-width'
import {
  ansi, ansi256, colorDepthFor, formatSeconds, headerLine, liveRow, meter, mix, nextLine, orbit, PROGRESS_FRAME_MS,
  progressGlyphsFor, stepLine, summaryLine, type ProgressGlyphs, type Run,
} from '../src/install-progress.ts'
import { PALETTE, PROGRESS_TONES } from '../src/palette.ts'

const text = (runs: readonly Run[]): string => runs.map(run => run.text).join('')
const frames = (draw: (time: number) => readonly Run[], count = 24) =>
  Array.from({ length: count }, (_, step) => text(draw(step * PROGRESS_FRAME_MS)))
const both = ['unicode', 'ascii'] as const satisfies readonly ProgressGlyphs[]

describe('orbit', () => {
  test('takes two cells in either glyph set, so a label never moves', () => {
    for (const glyphs of both) for (const frame of frames(time => orbit(time, glyphs))) expect(stringWidth(frame)).toBe(2)
  })

  test('circles four Braille dots around a square, a new frame each step, and repeats', () => {
    const drawn = frames(time => orbit(time, 'unicode'))
    for (const frame of drawn) {
      expect(frame).toMatch(/^[\u2800-\u28ff]{2}$/u)
      const dots = [...frame].reduce((sum, cell) => sum + (cell.codePointAt(0)! - 0x2800).toString(2).replaceAll('0', '').length, 0)
      expect(dots).toBe(4)
    }
    expect(drawn.every((frame, index) => index === 0 || frame !== drawn[index - 1])).toBe(true)
    expect(drawn.slice(12)).toEqual(drawn.slice(0, 12))
  })

  test('lights the cell holding the head in amber and the other in orange', () => {
    const tones = orbit(0, 'unicode').map(run => run.color)
    expect(new Set(tones)).toEqual(new Set([PROGRESS_TONES.to, PROGRESS_TONES.from]))
  })

  test('turns an ASCII bar where Braille is not drawn', () => {
    expect(frames(time => orbit(time, 'ascii'), 4)).toEqual(['- ', '\\ ', '| ', '/ '])
  })
})

describe('meter', () => {
  test('is exactly as wide as asked, at any fraction, time, or glyph set', () => {
    for (const glyphs of both) {
      for (const width of [0, 1, 7, 10, 32]) {
        for (const fraction of [undefined, 0, 0.26, 0.5, 0.99, 1, 7, -1, Number.NaN]) {
          for (const time of [0, 400, 1700, 99_999]) expect(stringWidth(text(meter(time, fraction, width, glyphs)))).toBe(width)
        }
      }
    }
  })

  test('fills heavy cells for the fraction done, a half cell at its edge, and leaves the light track', () => {
    expect(text(meter(0, 0, 10, 'unicode'))).toBe('\u2500'.repeat(10))
    expect(text(meter(0, 0.55, 10, 'unicode'))).toBe(`${'\u2501'.repeat(5)}\u2578${'\u2500'.repeat(4)}`)
    expect(text(meter(0, 1, 10, 'unicode'))).toBe('\u2501'.repeat(10))
    expect(text(meter(0, 0.55, 10, 'ascii'))).toBe('=====>----')
  })

  test('warms from orange to amber along the track, and sweeps a glint across the filled part', () => {
    const cells = meter(0, 1, 32, 'unicode').flatMap(run => [...run.text].map(() => run.color))
    // At time 0 the glint sits before the fill, so the ends carry the gradient's own tones.
    expect([cells[0], cells.at(-1)]).toEqual([PROGRESS_TONES.from, PROGRESS_TONES.to])
    const sweeps = frames(time => meter(time, 1, 32, 'unicode'), 20).length
    const coloured = Array.from({ length: 20 }, (_, step) => JSON.stringify(meter(step * PROGRESS_FRAME_MS, 1, 32, 'unicode')))
    expect(new Set(coloured).size).toBeGreaterThan(sweeps / 2)
    expect(meter(0, 0.5, 32, 'unicode').filter(run => run.dim === true).map(run => run.color)).toEqual([undefined])
  })

  test('runs a comet back and forth while a step has no length, never a fill', () => {
    const drawn = frames(time => meter(time, undefined, 20, 'unicode'), 30)
    expect(new Set(drawn).size).toBeGreaterThan(10)
    for (const frame of drawn) expect(frame).toMatch(/\u2501/u)
    // It reaches both ends of the track.
    expect(drawn.some(frame => frame.startsWith('\u2501'))).toBe(true)
    expect(drawn.some(frame => frame.endsWith('\u2501'))).toBe(true)
    for (const frame of frames(time => meter(time, undefined, 20, 'ascii'), 30)) expect(frame).toMatch(/^[=-]{20}$/u)
  })
})

describe('liveRow', () => {
  const step = { label: 'Downloading', fraction: 0.482, detail: '4.8 / 10.0 MB' }

  test('draws the orbit, the label, the meter, the percentage, the detail, and the elapsed time', () => {
    const row = text(liveRow(step, { time: 0, width: 100, glyphs: 'unicode', labelWidth: 18, elapsed: 2100 }))
    expect(row).toMatch(/^[\u2800-\u28ff]{2} Downloading {9}[\u2501\u2578\u2500]{32} {3}48% {2}4\.8 \/ 10\.0 MB {2}2\.1s$/u)
  })

  test('never draws wider than it may, giving up the time, the detail, the percentage, then the meter', () => {
    for (let width = 4; width <= 120; width++) {
      for (const glyphs of both) {
        expect(stringWidth(text(liveRow(step, { time: 0, width, glyphs, labelWidth: 18, elapsed: 2100 })))).toBeLessThanOrEqual(width)
      }
    }
    const at = (width: number) => text(liveRow(step, { time: 0, width, glyphs: 'unicode', labelWidth: 18, elapsed: 2100 }))
    expect(at(80)).toMatch(/2\.1s$/u)
    expect(at(56)).not.toContain('2.1s')
    expect(at(56)).toMatch(/MB$/u)
    expect(at(45)).toMatch(/48%$/u)
    expect(at(36)).not.toContain('%')
    expect(at(36)).toMatch(/\u2501/u)
    expect(at(30)).not.toMatch(/[\u2501\u2500]/u)
  })

  test('shows no percentage for a step of unknown length', () => {
    expect(text(liveRow({ label: 'Unpacking' }, { time: 0, width: 80, glyphs: 'unicode' }))).not.toContain('%')
  })
})

describe('stepLine, headerLine, summaryLine, and nextLine', () => {
  test('marks a finished step green and a failed one red, keeping the label where the orbit left it', () => {
    const done = stepLine('done', 'Downloaded', ['18.4 MB', '2.1s'], { width: 80, glyphs: 'unicode', labelWidth: 18 })
    expect(text(done)).toBe(`\u2713  Downloaded          18.4 MB \u00b7 2.1s`)
    expect(done[0]).toMatchObject({ color: PALETTE.done })
    const failed = stepLine('failed', 'Downloading', [], { width: 80, glyphs: 'unicode', labelWidth: 18 })
    expect(text(failed)).toBe('\u2717  Downloading')
    expect(failed[0]).toMatchObject({ color: PALETTE.failed })
    expect(text(stepLine('done', 'Downloaded', ['18.4 MB', '2.1s'], { width: 80, glyphs: 'ascii', labelWidth: 18 })))
      .toBe('+  Downloaded          18.4 MB - 2.1s')
  })

  test('cuts a long note to the width, ending in an ellipsis', () => {
    const cut = text(stepLine('done', 'Installed', ['~/.local/share/bake/versions/0.2.0-3f9a2c1e7b04'], { width: 40, glyphs: 'unicode', labelWidth: 18 }))
    expect(stringWidth(cut)).toBe(40)
    expect(cut.endsWith('\u2026')).toBe(true)
  })

  test('opens with Bake\'s name and closes with what was installed, how long it took, and the command to run', () => {
    const header = headerLine(['installer', 'linux-x64'], 80, 'unicode')
    expect(text(header)).toBe('BAKE  installer \u00b7 linux-x64')
    expect(header[0]).toMatchObject({ bold: true, color: PALETTE.running })
    expect(text(headerLine(['update', 'v0.1.0 \u2192 v0.2.0'], 80, 'ascii'))).toBe('BAKE  update - v0.1.0 -> v0.2.0')
    expect(text(summaryLine('Bake 0.2.0 installed', 3400, 80, 'unicode'))).toBe('Bake 0.2.0 installed in 3.4s')
    const next = nextLine('Add ~/.local/bin to PATH, then run: bake', 80, 'unicode')
    expect(next.at(-1)).toMatchObject({ text: 'bake', bold: true, color: PALETTE.running })
    expect(text(nextLine('Restart your shell', 80, 'unicode'))).toBe('Restart your shell')
  })

  test('formats durations as a person reads them', () => {
    expect([formatSeconds(0), formatSeconds(99), formatSeconds(420), formatSeconds(12_900), formatSeconds(64_000)])
      .toEqual(['<0.1s', '<0.1s', '0.4s', '12s', '1m 04s'])
  })
})

describe('terminal capabilities', () => {
  test('draws Unicode on a UTF-8 terminal or Windows Terminal, and ASCII where Ambiguous glyphs may be wide', () => {
    expect(progressGlyphsFor({ LANG: 'en_US.UTF-8', TERM: 'xterm-256color' })).toBe('unicode')
    expect(progressGlyphsFor({ WT_SESSION: 'a1b2' })).toBe('unicode')
    expect(progressGlyphsFor({ LC_ALL: 'C', LANG: 'en_US.UTF-8', TERM: 'xterm' })).toBe('ascii')
    expect(progressGlyphsFor({ LANG: 'en_US.UTF-8', TERM: 'dumb' })).toBe('ascii')
    expect(progressGlyphsFor({ LANG: 'zh_CN.UTF-8', TERM: 'xterm', WT_SESSION: 'a1b2' })).toBe('ascii')
    expect(progressGlyphsFor({})).toBe('ascii')
  })

  test('writes 24-bit colour where the terminal says so, the 256-colour cube otherwise, and none under NO_COLOR', () => {
    expect(colorDepthFor({ COLORTERM: 'truecolor' })).toBe('truecolor')
    expect(colorDepthFor({ WT_SESSION: 'a1b2' })).toBe('truecolor')
    expect(colorDepthFor({ TERM: 'xterm-256color' })).toBe('ansi256')
    expect(colorDepthFor({ COLORTERM: 'truecolor', NO_COLOR: '' })).toBe('none')
  })

  test('writes runs as escapes, sharing one escape across runs that look the same, and only text without colour', () => {
    const runs: Run[] = [{ text: 'a', color: '#f97316', bold: true }, { text: 'b', color: '#f97316', bold: true }, { text: 'c' }, { text: 'd', dim: true }]
    expect(ansi(runs, 'truecolor')).toBe('\u001b[1;38;2;249;115;22mab\u001b[0mc\u001b[2md\u001b[0m')
    expect(ansi(runs, 'ansi256')).toBe(`\u001b[1;38;5;${ansi256('#f97316')}mab\u001b[0mc\u001b[2md\u001b[0m`)
    expect(ansi(runs, 'none')).toBe('abcd')
    expect([ansi256('#000000'), ansi256('#ffffff'), ansi256('#ff0000')]).toEqual([16, 231, 196])
  })

  test('mixes tones and tolerates any amount', () => {
    expect(mix('#000000', '#ffffff', 0.5)).toBe('#808080')
    expect(mix('#102030', '#ffffff', -2)).toBe('#102030')
    expect(mix('#102030', '#ffffff', Number.NaN)).toBe('#102030')
    expect(mix('#102030', '#ffffff', 9)).toBe('#ffffff')
  })
})
