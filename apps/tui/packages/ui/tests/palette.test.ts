/** Palette roles that are computed, not written. Reading tones. */
import { describe, expect, it } from 'bun:test'
import { CACHE_FAIR, CACHE_GOOD, cacheTone, CONTEXT_FULL, CONTEXT_WARN, contextTone, PALETTE } from '../src/palette.ts'

describe('contextTone', () => {
  it('leaves room uncoloured, warns before the limit, and marks a nearly full context', () => {
    expect(contextTone(0)).toBeUndefined()
    expect(contextTone(CONTEXT_WARN - 1)).toBeUndefined()
    expect(contextTone(CONTEXT_WARN)).toBe(PALETTE.waiting)
    expect(contextTone(CONTEXT_FULL - 1)).toBe(PALETTE.waiting)
    expect(contextTone(CONTEXT_FULL)).toBe(PALETTE.failed)
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
