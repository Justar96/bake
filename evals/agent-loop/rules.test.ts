/** The record's regression rule and failure categories. */
import { describe, expect, test } from 'bun:test'
import { failureOf, pairedChange, regressions, type RuleInput } from './rules.ts'

const quiet: RuleInput = { totalTokens: null, requestsChange: null, failures: [0, 0], toolErrors: [0, 0] }
/** Pairs of request counts, base then candidate. */
const pairs = (counts: [number, number][]) => counts.map(([base, candidate]) => [{ requests: base }, { requests: candidate }] as [{ requests: number }, { requests: number }])

describe('requests gate', () => {
  test('flags a rise above 10% whose whole interval is above zero', () => {
    const change = pairedChange(pairs(Array.from({ length: 24 }, () => [3, 4] as [number, number])), sample => sample.requests)!
    expect(change.pct).toBeCloseTo(33.3, 1)
    expect(change.lo).toBeGreaterThan(0)
    expect(regressions('m vs base', { ...quiet, requestsChange: change })).toEqual([`m vs base: requests ${change.pct}% [${change.lo}, ${change.hi}]`])
  })

  test('passes a rise of 10% or less, or one whose interval reaches zero', () => {
    expect(regressions('m', { ...quiet, requestsChange: { pct: 9.5, lo: 2, hi: 15 } })).toEqual([])
    expect(regressions('m', { ...quiet, requestsChange: { pct: 10, lo: 1, hi: 20 } })).toEqual([])
    const noisy = pairedChange(pairs([[3, 9], [3, 3], [3, 3], [4, 3], [3, 3], [5, 4]]), sample => sample.requests)!
    expect(noisy.pct).toBeGreaterThan(10)
    expect(noisy.lo).toBeLessThanOrEqual(0)
    expect(regressions('m', { ...quiet, requestsChange: noisy })).toEqual([])
  })

  test('keeps the token, failure, and tool-error rules', () => {
    expect(regressions('m', { ...quiet, totalTokens: { pct: 4, lo: 0.5, hi: 8 } })).toEqual(['m: total tokens 4% [0.5, 8]'])
    expect(regressions('m', { ...quiet, failures: [1, 3] })).toEqual(['m: failures 1 -> 3'])
    expect(regressions('m', { ...quiet, failures: [1, 2] })).toEqual([])
    expect(regressions('m', { ...quiet, toolErrors: [0, 1] })).toEqual(['m: tool errors 0 -> 1'])
  })
})

describe('failure categories', () => {
  test('a runaway guard abort is its own category', () => {
    expect(failureOf({ success: false, runawayAbort: true, code: 1, abortCause: null })).toBe('runaway')
  })

  test('other failures keep their categories', () => {
    expect(failureOf({ success: true })).toBeNull()
    expect(failureOf({ success: false, abortCause: 'request_limit', code: null })).toBe('request_limit')
    expect(failureOf({ success: false, code: 1, final: '' })).toBe('exit 1')
    expect(failureOf({ success: false, code: 0, final: '' })).toBe('empty final reply')
    expect(failureOf({ success: false, code: 0, final: 'done' })).toBe('validation failed')
    // A sample recorded before the runaway field existed reads as before.
    expect(failureOf({ success: false, code: 1, abortCause: null })).toBe('exit 1')
  })
})
