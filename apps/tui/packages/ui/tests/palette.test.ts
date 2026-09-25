/** Palette roles that are computed, not written. Reading tones. */
import { describe, expect, it } from 'bun:test'
import { CACHE_FAIR, CACHE_GOOD, cacheTone, PALETTE } from '../src/palette.ts'

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
