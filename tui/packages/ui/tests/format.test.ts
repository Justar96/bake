/** Status-line number formatting. */

import { describe, expect, it } from 'bun:test'
import { formatContext, formatTokens } from '../src/format.ts'

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
    expect(formatContext({ used: 12_340, window: 1_000_000 })).toBe('12.3k/1M (1%)')
  })

  it('rounds the percentage down', () => {
    // A context that is merely close to full must not read as 100%: this is
    // the number a user decides to compact on.
    expect(formatContext({ used: 999_999, window: 1_000_000 })).toBe('1M/1M (99%)')
    expect(formatContext({ used: 1_000_000, window: 1_000_000 })).toBe('1M/1M (100%)')
  })

  it('reports an unknown capacity as zero percent rather than dividing by it', () => {
    expect(formatContext({ used: 10, window: 0 })).toBe('10/0 (0%)')
  })
})
