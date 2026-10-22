import { describe, expect, it } from 'vitest'
import type { PtcJsonValue } from 'bake-ptc-runtime'
import { OutputLedger, jsonStringBytesUpTo, jsonValueBytesUpTo, truncateJsonStringBytes } from '../src/output.ts'

describe('JSON byte accounting', () => {
  it('accounts every JSON escape and cuts only between complete code points', () => {
    const prefix = '"\\\b\t\n\f\r\u0000😀\ud800€a'
    const text = `${prefix}z`
    const budget = Buffer.byteLength(JSON.stringify(prefix), 'utf8')
    expect(jsonStringBytesUpTo(prefix, budget)).toBe(budget)
    expect(jsonStringBytesUpTo(prefix, budget - 1)).toBeUndefined()
    expect(truncateJsonStringBytes(text, budget)).toBe(prefix)
    expect(truncateJsonStringBytes('x', 1)).toBe('')
  })

  it('matches JSON serialization for every lossless value branch and stops at the cap', () => {
    const value = { empty: {}, nil: null, yes: true, no: false, number: 1.5, big: 1e21, text: '"\n😀', array: [1, 'x'] }
    const bytes = Buffer.byteLength(JSON.stringify(value), 'utf8')
    expect(jsonValueBytesUpTo(value, bytes)).toBe(bytes)
    expect(jsonValueBytesUpTo(value, bytes - 1)).toBeUndefined()
    expect(jsonValueBytesUpTo([], 2)).toBe(2)
    expect(jsonValueBytesUpTo([0, 0], 4)).toBeUndefined()
    expect(jsonValueBytesUpTo({ long: null }, 2)).toBeUndefined()
  })

  it('meters deeply nested arrays without recursive stack growth', () => {
    let value: PtcJsonValue = null
    for (let depth = 0; depth < 5_000; depth++) value = [value]
    expect(jsonValueBytesUpTo(value, 10_004)).toBe(10_004)
    expect(jsonValueBytesUpTo(value, 10_003)).toBeUndefined()
  })
})

describe('OutputLedger', () => {
  it('charges separators and escapes, and reports the fitting prefix and remaining budget', () => {
    const ledger = new OutputLedger(20)
    expect(ledger.admit('first')).toBe(true)
    expect(ledger.admit('second')).toBe(true)
    expect(ledger.remaining).toBe(2)
    expect(ledger.admit('third')).toBe(false)
    expect(ledger.fittingPrefix('third')).toBe('')
    expect(ledger.success(['first', 'second'])).toEqual({ logs: ['first', 'second'] })
    expect(ledger.success(['first', 'second'], 1)).toEqual({ logs: ['first', 'second'], value: 1 })
    expect(ledger.success(['first', 'second'], 'too long').error?.kind).toBe('output-limit')
    const roomy = new OutputLedger(30)
    expect(roomy.admit('first')).toBe(true)
    expect(roomy.fittingPrefix('abcdefghijklmnopqrstuvwxyz')).toBe('abcdefghijklmnopqr')
    expect(Buffer.byteLength(JSON.stringify(['first', 'abcdefghijklmnopqr']))).toBe(30)
  })

  it('retains a bounded prefix when logs or a failure diagnostic exceed the limit', () => {
    for (const maxBytes of [4, 8, 40, 80]) {
      const result = new OutputLedger(maxBytes).limit(['a', 'b', '你好🙂'.repeat(50)])
      expect(result.error?.kind).toBe('output-limit')
      const bytes = Buffer.byteLength(JSON.stringify(result.logs)) + Buffer.byteLength(JSON.stringify(result.error?.message))
      expect(bytes).toBeLessThanOrEqual(maxBytes)
    }
    const ledger = new OutputLedger(80)
    expect(ledger.failure([], { kind: 'exception', message: 'short' })).toEqual({ logs: [], error: { kind: 'exception', message: 'short' } })
    expect(ledger.failure([], { kind: 'exception', message: 'x'.repeat(100) }).error?.kind).toBe('output-limit')
  })
})
