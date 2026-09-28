/** Palette roles that are computed, not written. Reading tones. */
import { describe, expect, it } from 'bun:test'
import {
  CACHE_FAIR, CACHE_GOOD, cacheTone, CONTEXT_FULL, CONTEXT_HOT, CONTEXT_HOT_MAX, CONTEXT_LEAD, CONTEXT_RAMP, CONTEXT_STEP, contextTone, PALETTE,
} from '../src/palette.ts'

describe('contextTone', () => {
  const [soft, yellow, orange, red] = CONTEXT_RAMP

  it('leaves room uncoloured, then warms a step every ten points to red at 90%', () => {
    expect(contextTone(0)).toBeUndefined()
    expect(contextTone(59)).toBeUndefined()
    expect([60, 69].map(percent => contextTone(percent))).toEqual([soft, soft])
    expect([70, 79].map(percent => contextTone(percent))).toEqual([yellow, yellow])
    expect([CONTEXT_HOT, 89].map(percent => contextTone(percent))).toEqual([orange, orange])
    expect([CONTEXT_FULL, 100, 130].map(percent => contextTone(percent))).toEqual([red, red, red])
  })

  it('turns orange ten points below a known compaction mark, and red from 90% whatever the mark', () => {
    expect(contextTone(49, 80)).toBeUndefined()
    expect(contextTone(50, 80)).toBe(soft)
    expect(contextTone(60, 80)).toBe(yellow)
    expect(contextTone(80 - CONTEXT_LEAD, 80)).toBe(orange)
    expect(contextTone(89, 80)).toBe(orange)
    expect(contextTone(CONTEXT_FULL, 80)).toBe(red)
    // A low mark moves the whole ramp down with it.
    expect([29, 30, 40, 50].map(percent => contextTone(percent, 60))).toEqual([undefined, soft, yellow, orange])
    // A mark at the window still leaves a step of orange before red.
    expect(contextTone(CONTEXT_HOT_MAX - 1, 100)).toBe(yellow)
    expect(contextTone(CONTEXT_HOT_MAX, 100)).toBe(orange)
    expect(contextTone(CONTEXT_HOT_MAX - 2 * CONTEXT_STEP - 1, 100)).toBeUndefined()
  })

  it('warms through tones of its own, none of them a state\'s', () => {
    expect(new Set(CONTEXT_RAMP).size).toBe(CONTEXT_RAMP.length)
    for (const tone of CONTEXT_RAMP) expect(Object.values(PALETTE)).not.toContain(tone)
  })
})

describe('cacheTone', () => {
  it('leaves a healthy hit in the normal foreground, and warns of a middling one and a low one', () => {
    expect(cacheTone(100)).toBeUndefined()
    expect(cacheTone(CACHE_GOOD)).toBeUndefined()
    expect(cacheTone(CACHE_GOOD - 1)).toBe(PALETTE.waiting)
    expect(cacheTone(CACHE_FAIR)).toBe(PALETTE.waiting)
    expect(cacheTone(CACHE_FAIR - 1)).toBe(PALETTE.failed)
    expect(cacheTone(0)).toBe(PALETTE.failed)
  })
})
