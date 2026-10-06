import { describe, expect, test } from 'bun:test'
import { compareAll, compareEvents, compareFiles, comparePermissions, comparePrompts, jsonEqual } from './compare.ts'

describe('comparators', () => {
  test('prompt bytes report the first differing byte without the text', () => {
    expect(comparePrompts(['กา\r\n '], ['กา\r\n ']).outcome).toBe('pass')
    const result = comparePrompts(['secret a'], ['secret b'])
    expect(result).toEqual({ comparator: 'prompt-bytes', outcome: 'fail', detail: 'prompt 0 differs at byte 7 (8 vs 8 bytes)' })
    expect(comparePrompts(['x '], ['x']).detail).toBe('prompt 0 differs at byte 1 (2 vs 1 bytes)')
    expect(comparePrompts([''], []).detail).toBe('prompt count 1 differs from 0')
    // Canonically equivalent but byte-different text still differs.
    expect(comparePrompts(['é'], ['é']).outcome).toBe('fail')
  })

  test('events keep order, ignore key order, and count unknown fields', () => {
    const start = { type: 'start', data: { a: 1, b: [1, 2] } }
    const finish = { type: 'finish' }
    expect(compareEvents([start, finish], [{ data: { b: [1, 2], a: 1 }, type: 'start' }, finish]).outcome).toBe('pass')
    expect(compareEvents([start, finish], [finish, start]).detail).toBe('event 0 differs')
    expect(compareEvents([finish], [{ ...finish, extra: null }]).outcome).toBe('fail')
    expect(compareEvents([start], [{ type: 'start', data: { a: 1, b: [2, 1] } }]).outcome).toBe('fail')
  })

  test('json equality separates null, missing, and differently typed values', () => {
    expect(jsonEqual({ a: null }, {})).toBe(false)
    expect(jsonEqual({}, { a: null })).toBe(false)
    expect(jsonEqual([1], { 0: 1 })).toBe(false)
    expect(jsonEqual('1', 1)).toBe(false)
    expect(jsonEqual(null, null)).toBe(true)
  })

  test('permissions compare in order', () => {
    const allow = { id: 'a', path: 'a.txt', decision: 'allow' as const }
    const deny = { id: 'b', path: 'b.txt', decision: 'deny' as const }
    expect(comparePermissions([allow, deny], [allow, deny]).outcome).toBe('pass')
    expect(comparePermissions([allow, deny], [deny, allow]).outcome).toBe('fail')
    expect(comparePermissions([allow], [{ ...allow, decision: 'deny' }]).detail).toBe('permission 0 differs')
  })

  test('final files compare as a set of exact paths and bytes', () => {
    const a = { path: 'a.txt', hex: '00' }
    const b = { path: 'dir/b.txt', hex: '' }
    expect(compareFiles([a, b], [b, a]).outcome).toBe('pass')
    expect(compareFiles([a], [{ ...a, hex: '01' }]).detail).toBe('"a.txt" has different bytes')
    expect(compareFiles([a], [a, b]).detail).toBe('"dir/b.txt" exists only on the right')
    expect(compareFiles([a], [{ ...a, path: 'A.txt' }]).outcome).toBe('fail')
  })

  test('compareAll keeps every result and marks missing sides unavailable', () => {
    const results = compareAll({ prompts: ['a'], files: [] }, { prompts: ['b'], events: [], permissions: [], files: [] })
    expect(results.map(result => [result.comparator, result.outcome])).toEqual([
      ['prompt-bytes', 'fail'], ['event-order', 'unavailable'], ['permissions', 'unavailable'], ['final-files', 'pass'],
    ])
  })
})
