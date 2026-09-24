/** Status-line number formatting. */

import { describe, expect, it } from 'bun:test'
import { cacheHit, formatAge, formatContext, formatTokens, formatTotals } from '../src/format.ts'

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

describe('formatContext', () => {
  it('reports occupancy as used, capacity, and percent', () => {
    expect(formatContext({ used: 12_340, window: 1_000_000 })).toBe('~12.3k/1M (1%)')
  })

  it('rounds the percentage down', () => {
    // A context that is merely close to full must not read as 100%: this is
    // the number a user decides to compact on.
    expect(formatContext({ used: 999_999, window: 1_000_000 })).toBe('~1M/1M (99%)')
    expect(formatContext({ used: 1_000_000, window: 1_000_000 })).toBe('~1M/1M (100%)')
  })

  it('reports an unknown capacity as zero percent rather than dividing by it', () => {
    expect(formatContext({ used: 10, window: 0 })).toBe('~10/0 (0%)')
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
