/** Palette roles that are computed rather than written: the shimmer ramp and reading tones. */
import { describe, expect, it } from 'bun:test'
import { ACCENT, CACHE_FAIR, CACHE_GOOD, cacheTone, PALETTE } from '../src/palette.ts'
import { SHIMMER_LEVELS } from '../src/activity.ts'

describe('shimmer ramp', () => {
  it('runs from the running orange to the glint, one tone per shimmer level', () => {
    expect(ACCENT.ramp).toHaveLength(SHIMMER_LEVELS + 1)
    expect(ACCENT.ramp[0]).toBe(PALETTE.running)
    expect(ACCENT.ramp.at(-1)).toBe(PALETTE.glint)
    // Each step brightens every channel toward the glint, so no step dips.
    const channels = (hex: string) => [1, 3, 5].map(index => Number.parseInt(hex.slice(index, index + 2), 16))
    for (let step = 1; step < ACCENT.ramp.length; step++) {
      const [before, after] = [channels(ACCENT.ramp[step - 1]!), channels(ACCENT.ramp[step]!)]
      expect(after.every((value, index) => value >= before[index]!)).toBe(true)
    }
  })
})

describe('cacheTone', () => {
  it('reads a high hit as good, a middling one as fair, and a low one as poor', () => {
    expect(cacheTone(100)).toBe(PALETTE.done)
    expect(cacheTone(CACHE_GOOD)).toBe(PALETTE.done)
    expect(cacheTone(CACHE_GOOD - 1)).toBe(PALETTE.waiting)
    expect(cacheTone(CACHE_FAIR)).toBe(PALETTE.waiting)
    expect(cacheTone(CACHE_FAIR - 1)).toBe(PALETTE.failed)
    expect(cacheTone(0)).toBe(PALETTE.failed)
  })
})
