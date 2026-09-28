/** Status-line number formatting. */

import { describe, expect, it } from 'bun:test'
import { cacheHit, compactPercent, contextPercent, formatAge, formatContext, formatTokens, formatTotals } from '../src/format.ts'

describe('formatTokens', () => {
  it('keeps small counts exact', () => {
    expect(formatTokens(0)).toBe('0')
    expect(formatTokens(999)).toBe('999')
  })

  it('abbreviates thousands and millions', () => {
    expect(formatTokens(1_000)).toBe('1k')
    expect(formatTokens(12_340)).toBe('12.3k')
    expect(formatTokens(1_000_000)).toBe('1M')
    expect(formatTokens(1_250_000)).toBe('1.3M')
  })
})

describe('context readings', () => {
  it('reports the absolute count as used over capacity', () => {
    expect(formatContext({ used: 12_340, window: 1_000_000 })).toBe('12.3k/1M')
    expect(contextPercent({ used: 12_340, window: 1_000_000 })).toBe(1)
  })

  it('rounds the percentage down', () => {
    // A context that is merely close to full must not be shown as 100%. This is
    // the number a user decides to compact on.
    expect(contextPercent({ used: 999_999, window: 1_000_000 })).toBe(99)
    expect(contextPercent({ used: 1_000_000, window: 1_000_000 })).toBe(100)
  })

  it('reports an unknown capacity as zero percent rather than dividing by it', () => {
    expect(contextPercent({ used: 10, window: 0 })).toBe(0)
    expect(formatContext({ used: 10, window: 0 })).toBe('10/0')
  })

  it('names where automatic compaction starts as a share of the window, and nothing without a threshold', () => {
    expect(compactPercent({ used: 10, window: 128_000, compactAt: 102_400 })).toBe(80)
    expect(compactPercent({ used: 10, window: 200_000, compactAt: 169_999 })).toBe(85)
    expect(compactPercent({ used: 10, window: 128_000 })).toBeUndefined()
    expect(compactPercent({ used: 10, window: 0, compactAt: 5 })).toBeUndefined()
  })
})

describe('formatTotals', () => {
  it('shows input and output', () => {
    expect(formatTotals({ input: 12_340, output: 1_200 }, { input: 'in', output: 'out' })).toEqual(['in 12.3k', 'out 1.2k'])
  })
})

describe('cacheHit', () => {
  it('is absent when the provider reports no cache traffic', () => {
    expect(cacheHit({ input: 900, output: 100 })).toBeUndefined()
  })

  it('is the share of input read from cache, rounded down', () => {
    expect(cacheHit({ input: 1_900, output: 150, cached: 800 })).toBe(42)
    // A hit is never rounded up to a whole cache.
    expect(cacheHit({ input: 1_000, output: 0, cached: 999 })).toBe(99)
    expect(cacheHit({ input: 0, output: 10, cached: 0 })).toBe(0)
  })
})

describe('formatAge', () => {
  const words = { now: 'just now', minutes: 'm ago', hours: 'h ago', days: 'd ago' }
  const minute = 60_000

  it('names the coarsest non-zero unit', () => {
    expect(formatAge(0, 59_999, words)).toBe('just now')
    expect(formatAge(0, 5 * minute, words)).toBe('5m ago')
    expect(formatAge(0, 59 * minute, words)).toBe('59m ago')
    expect(formatAge(0, 60 * minute, words)).toBe('1h ago')
    expect(formatAge(0, 47 * 60 * minute, words)).toBe('1d ago')
  })

  it('reads a future time as now', () => {
    expect(formatAge(10 * minute, 0, words)).toBe('just now')
  })
})
