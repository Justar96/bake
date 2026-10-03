import { runInNewContext } from 'node:vm'
import { describe, expect, it } from 'vitest'
import {
  assertPositiveInteger,
  errorMessage,
  hasIntrinsicConstructor,
  hasPlainArrayPrototype,
  isENOENT,
  isIntrinsicObjectPrototype,
  isRecord,
} from '../src/index.ts'

describe('isRecord', () => {
  it('accepts non-null, non-array objects, including class instances', () => {
    expect(isRecord({})).toBe(true)
    expect(isRecord(Object.create(null))).toBe(true)
    expect(isRecord(new Map())).toBe(true)
  })

  it('rejects arrays, null, and primitives', () => {
    expect(isRecord([])).toBe(false)
    expect(isRecord(null)).toBe(false)
    expect(isRecord(undefined)).toBe(false)
    expect(isRecord('text')).toBe(false)
    expect(isRecord(1)).toBe(false)
  })
})

describe('errorMessage', () => {
  it('renders an Error by its message and anything else by string coercion', () => {
    expect(errorMessage(new TypeError('bad input'))).toBe('bad input')
    expect(errorMessage('plain')).toBe('plain')
    expect(errorMessage(42)).toBe('42')
    expect(errorMessage(undefined)).toBe('undefined')
  })
})

describe('assertPositiveInteger', () => {
  it('accepts positive integers', () => {
    expect(() => { assertPositiveInteger('limit', 1) }).not.toThrow()
    expect(() => { assertPositiveInteger('limit', 2 ** 40) }).not.toThrow()
  })

  it.each([0, -1, 1.5, Number.NaN, Number.POSITIVE_INFINITY])('rejects %s with the named message', (value) => {
    expect(() => { assertPositiveInteger('tool-web: fetchTimeoutMs', value) })
      .toThrow(new Error('tool-web: fetchTimeoutMs must be a positive integer'))
  })
})

describe('isENOENT', () => {
  it('matches only an ENOENT code', () => {
    expect(isENOENT(Object.assign(new Error('missing'), { code: 'ENOENT' }))).toBe(true)
    expect(isENOENT({ code: 'ENOENT' })).toBe(true)
    expect(isENOENT(Object.assign(new Error('denied'), { code: 'EACCES' }))).toBe(false)
    expect(isENOENT(new Error('no code'))).toBe(false)
  })

  it('returns false for null, undefined, and primitives without throwing', () => {
    expect(isENOENT(null)).toBe(false)
    expect(isENOENT(undefined)).toBe(false)
    expect(isENOENT('ENOENT')).toBe(false)
    expect(isENOENT(0)).toBe(false)
  })
})

describe('realm-intrinsic prototype guards', () => {
  const foreign = runInNewContext('({ object: {}, array: [], ObjectPrototype: Object.prototype, ArrayPrototype: Array.prototype })') as {
    object: object
    array: unknown[]
    ObjectPrototype: object
    ArrayPrototype: object
  }

  it('recognizes this realm\'s intrinsic prototypes', () => {
    expect(hasIntrinsicConstructor(Object.prototype, 'Object')).toBe(true)
    expect(hasIntrinsicConstructor(Array.prototype, 'Array')).toBe(true)
    expect(hasIntrinsicConstructor(Array.prototype, 'Object')).toBe(false)
    expect(isIntrinsicObjectPrototype(Object.prototype)).toBe(true)
    expect(isIntrinsicObjectPrototype(Array.prototype)).toBe(false)
    expect(hasPlainArrayPrototype([1, 2])).toBe(true)
  })

  it('recognizes another vm realm\'s intrinsic prototypes', () => {
    expect(foreign.ObjectPrototype).not.toBe(Object.prototype)
    expect(hasIntrinsicConstructor(foreign.ObjectPrototype, 'Object')).toBe(true)
    expect(hasIntrinsicConstructor(foreign.ArrayPrototype, 'Array')).toBe(true)
    expect(isIntrinsicObjectPrototype(Object.getPrototypeOf(foreign.object) as object)).toBe(true)
    expect(hasPlainArrayPrototype(foreign.array)).toBe(true)
  })

  it('rejects forged prototypes whose constructor is not the matching native intrinsic', () => {
    const userDefined = Object.create(null) as object
    Object.defineProperty(userDefined, 'constructor', { value: function Object() {} })
    expect(hasIntrinsicConstructor(userDefined, 'Object')).toBe(false)
    expect(isIntrinsicObjectPrototype(userDefined)).toBe(false)

    const borrowed = Object.create(null) as object
    Object.defineProperty(borrowed, 'constructor', { value: Object })
    expect(hasIntrinsicConstructor(borrowed, 'Object')).toBe(false)

    const getter = Object.create(null) as object
    Object.defineProperty(getter, 'constructor', { get: () => Object })
    expect(hasIntrinsicConstructor(getter, 'Object')).toBe(false)
  })

  it('rejects array subclasses and arrays with a swapped prototype', () => {
    class Items extends Array<number> {}
    expect(hasPlainArrayPrototype(Items.from([1]))).toBe(false)

    const swapped: unknown[] = []
    Object.setPrototypeOf(swapped, Object.prototype)
    expect(hasPlainArrayPrototype(swapped)).toBe(false)

    const forged: unknown[] = []
    const fakeArrayPrototype: unknown[] = []
    Object.defineProperty(fakeArrayPrototype, 'constructor', { value: function Array() {} })
    Object.setPrototypeOf(forged, fakeArrayPrototype)
    expect(hasPlainArrayPrototype(forged)).toBe(false)
  })
})
