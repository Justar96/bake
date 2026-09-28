/** The loaf's frames, tones, and terminal row, shared by `/update`, `bake update`, and the installers. */
import { describe, expect, test } from 'bun:test'
import stringWidth from 'string-width'
import { ansi256, bakeLevel, crustTone, LOAF_FRAME_MS, loafDone, loafFrame, loafGlyphsFor, loafLine } from '../src/loaf.ts'
import { CRUST } from '../src/palette.ts'

const frames = (glyphs: 'unicode' | 'ascii', level = 0.5) =>
  Array.from({ length: 24 }, (_, step) => loafFrame(step * LOAF_FRAME_MS, level, glyphs))

describe('loafFrame', () => {
  test('keeps steam and loaf the same width in either glyph set, so the label never moves', () => {
    for (const glyphs of ['unicode', 'ascii'] as const) {
      for (const frame of [...frames(glyphs), loafDone(glyphs)]) {
        expect(stringWidth(frame.steam)).toBe(3)
        expect(stringWidth(frame.loaf)).toBe(7)
      }
    }
  })

  test('draws Unicode steam in Braille alone, and ASCII steam and loaf in ASCII alone', () => {
    for (const frame of frames('unicode')) expect(frame.steam).toMatch(/^[\u2800-\u28ff]{3}$/u)
    for (const frame of frames('ascii')) expect(frame.steam + frame.loaf).toMatch(/^[\x20-\x7e]+$/u)
  })

  test('moves the steam from frame to frame and repeats it', () => {
    for (const glyphs of ['unicode', 'ascii'] as const) {
      const steam = frames(glyphs).map(frame => frame.steam)
      expect(new Set(steam).size).toBeGreaterThan(3)
      expect(steam.every((cells, index) => index === 0 || cells !== steam[index - 1])).toBe(true)
      expect(steam.slice(12)).toEqual(steam.slice(0, 12))
    }
  })

  test('browns the ASCII crumb with the loaf, since the tone may not be drawn', () => {
    expect(loafFrame(0, 0, 'ascii').loaf).toBe('(.....)')
    expect(loafFrame(0, 1, 'ascii').loaf).toBe('(#####)')
    expect(loafDone('ascii').loaf).toBe('(#####)')
    expect(loafDone('unicode').steam.trim()).toBe('')
  })
})

describe('bakeLevel and crustTone', () => {
  test('only browns as an install advances, and finishes the crust', () => {
    const levels = [
      bakeLevel({ phase: 'check' }),
      ...[0, 25, 50, 100].map(received => bakeLevel({ phase: 'download', received, total: 100 })),
      bakeLevel({ phase: 'unpack' }), bakeLevel({ phase: 'verify' }), bakeLevel({ phase: 'done' }),
    ]
    expect(levels.every((level, index) => index === 0 || level >= levels[index - 1]!)).toBe(true)
    expect([levels[0], levels.at(-1)]).toEqual([0, 1])
    // A size the manifest got wrong cannot overbake the download.
    expect(bakeLevel({ phase: 'download', received: 500, total: 100 })).toBe(bakeLevel({ phase: 'download', received: 100, total: 100 }))
    expect(bakeLevel({ phase: 'download', received: 0, total: 0 })).toBeLessThan(bakeLevel({ phase: 'unpack' }))
  })

  test('runs from dough to crust and tolerates any number', () => {
    expect(crustTone(0)).toBe(CRUST[0]!)
    expect(crustTone(1)).toBe(CRUST.at(-1)!)
    expect(crustTone(-3)).toBe(CRUST[0]!)
    expect(crustTone(Number.NaN)).toBe(CRUST[0]!)
    expect(crustTone(9)).toBe(CRUST.at(-1)!)
  })

  test('maps a tone to the xterm-256 cube', () => {
    expect([ansi256('#000000'), ansi256('#ffffff'), ansi256('#ff0000')]).toEqual([16, 231, 196])
  })
})

describe('loafGlyphsFor', () => {
  test('draws Unicode only on a UTF-8 terminal that does not widen Ambiguous glyphs', () => {
    expect(loafGlyphsFor({ LANG: 'en_US.UTF-8', TERM: 'xterm-256color' })).toBe('unicode')
    expect(loafGlyphsFor({ LC_ALL: 'C', LANG: 'en_US.UTF-8', TERM: 'xterm' })).toBe('ascii')
    expect(loafGlyphsFor({ LANG: 'en_US.UTF-8', TERM: 'dumb' })).toBe('ascii')
    expect(loafGlyphsFor({ LANG: 'zh_CN.UTF-8', TERM: 'xterm' })).toBe('ascii')
    expect(loafGlyphsFor({})).toBe('ascii')
  })
})

describe('loafLine', () => {
  const frame = loafFrame(0, 0.5, 'unicode')

  test('writes plain text without colour and styles only the steam and loaf with it', () => {
    expect(loafLine(frame, 'Baking', { color: false, width: 80 })).toBe(`  ${frame.steam} ${frame.loaf}  Baking`)
    const styled = loafLine(frame, 'Baking', { color: true, width: 80 })
    expect(styled).toContain(`\u001b[38;5;${ansi256(frame.tone)}m${frame.loaf}\u001b[39m`)
    expect(styled.replace(/\u001b\[[\d;]*m/gu, '')).toBe(loafLine(frame, 'Baking', { color: false, width: 80 }))
  })

  test('cuts the label to fit, and draws nothing where even the loaf does not fit', () => {
    const cut = loafLine(frame, 'Downloading Bake 0.2.0… 48%', { color: false, width: 24 })
    expect(stringWidth(cut)).toBeLessThanOrEqual(24)
    expect(cut.endsWith('\u2026')).toBe(true)
    expect(loafLine(frame, 'Baking', { color: false, width: 10 })).toBe('')
  })
})
