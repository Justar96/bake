import { describe, expect, test } from 'bun:test'
import { readdirSync, readFileSync } from 'node:fs'
import { join } from 'node:path'
import {
  ConformanceInputError, type Fixture, MAX_DOCUMENT_BYTES, MAX_ITEMS, MAX_TEXT_BYTES,
  parseStrictJson, safePath, validateFixture, validateInput,
} from './fixture.ts'

const CONFORMANCE = join(import.meta.dirname, '..', '..', 'conformance')
const bytes = (text: string): Uint8Array => new TextEncoder().encode(text)
const parse = (text: string): unknown => parseStrictJson(bytes(text), 'test')
const input = (fields: Record<string, unknown> = {}): Record<string, unknown> => ({
  schema: 'bake/synthetic-conformance/input', version: 1, prompts: [], events: [], permissions: [], writes: [], ...fields,
})
const rejects = (action: () => unknown, message: string | RegExp): void => {
  expect(action).toThrow(ConformanceInputError)
  expect(action).toThrow(message)
}

describe('shared fixtures', () => {
  const fixtures = readdirSync(join(CONFORMANCE, 'fixtures')).filter(name => name.endsWith('.json'))

  test('every fixture validates and names its own file', () => {
    expect(fixtures.length).toBeGreaterThanOrEqual(2)
    for (const name of fixtures) {
      const fixture = validateFixture(parseStrictJson(readFileSync(join(CONFORMANCE, 'fixtures', name)), name))
      expect(`${fixture.id}.json`).toBe(name)
    }
  })

  const invalid: Record<string, RegExp> = {
    'duplicate-event-key.json': /repeats an object key/,
    'duplicate-permission.json': /repeats permission id/,
    'event-depth.json': /nests deeper than 32 containers/,
    'future-version.json': /version: must be 1/,
    'negative-zero.json': /not a plain safe integer/,
    'non-integer.json': /not a plain safe integer/,
    'reserved-console.json': /Windows reserved device/,
    'reserved-spaced-device.json': /Windows reserved device/,
    'unknown-field.json': /unknown field "unexpected"/,
    'unknown-permission.json': /unknown permission/,
    'unpaired-surrogate.json': /unpaired surrogate/,
    'unsafe-path.json': /dot-dot segment/,
    'windows-path-character.json': /character Windows forbids/,
  }

  test('every shared invalid input is rejected for its own reason', () => {
    expect(readdirSync(join(CONFORMANCE, 'invalid')).sort()).toEqual(Object.keys(invalid).sort())
    for (const [name, reason] of Object.entries(invalid)) {
      rejects(() => validateInput(parseStrictJson(readFileSync(join(CONFORMANCE, 'invalid', name)), name)), reason)
    }
  })
})

describe('strict JSON text', () => {
  test('keeps exact strings and safe integers', () => {
    expect(parse('{"a":["x\\r\\n ", -9007199254740991, 0, 9007199254740991]}'))
      .toEqual({ a: ['x\r\n ', -9007199254740991, 0, 9007199254740991] })
  })

  test.each(['1.0', '1e2', '-0', '1.5', '9007199254740992', '-9007199254740992'])('rejects number %s', (token) => {
    rejects(() => parse(`[${token}]`), /not a plain safe integer/)
  })

  test('rejects a byte order mark, invalid UTF-8, and malformed JSON', () => {
    rejects(() => parse('\uFEFF{}'), /not valid JSON/)
    rejects(() => parseStrictJson(new Uint8Array([0x22, 0xff, 0x22]), 'test'), /not valid UTF-8/)
    rejects(() => parse('{"a":1,}'), /not valid JSON/)
    rejects(() => parse(''), /not valid JSON/)
  })

  test('rejects unpaired surrogates in keys and values', () => {
    rejects(() => parse('{"\\udc00":1}'), /unpaired surrogate in a key/)
    rejects(() => parse('["\\ud800x"]'), /unpaired surrogate/)
  })

  test('rejects a repeated key at any depth but not equal keys in sibling objects', () => {
    rejects(() => parse('{"a":1,"a":1}'), /repeats an object key/)
    rejects(() => parse('{"x":[{"k":"\\"","\\u006b":2}]}'), /repeats an object key/)
    expect(parse('[{"a":"a:"},{"a":{"a":"a"}}]')).toEqual([{ a: 'a:' }, { a: { a: 'a' } }])
  })

  test('bounds the document size', () => {
    rejects(() => parseStrictJson(new Uint8Array(MAX_DOCUMENT_BYTES + 1), 'test'), /exceeds/)
  })
})

describe('paths', () => {
  test.each(['a.txt', 'src/nested/b.bin', 'ก/👩‍💻.txt', 'console.txt', 'com10'])('accepts %p', (path) => {
    expect(safePath(path, 'path')).toBe(path)
  })

  test.each([
    '', '/abs', 'a//b', 'a/', './a', 'a/../b', '..', 'a\\b', 'C:x', 'c:/x', '//server/share', 'a\u0000b', 'a\u001fb', 'a\u007fb',
    'CON', 'nul.txt', 'dir/Lpt1', 'com1.log', 'NUL .txt', 'aux  ', 'CONIN$', 'conout$.log', 'COM¹',
    'a<b', 'a>b', 'a"b', 'a|b', 'a?b', 'a*b', 'trailing.', 'trailing ', 'dir./a', 'x'.repeat(201), 'ก'.repeat(67),
  ])('rejects %p', (path) => {
    rejects(() => safePath(path, 'path'), /path/)
  })
})

describe('input', () => {
  const permission = { id: 'edit', path: 'a.txt', decision: 'allow' }

  test('accepts unknown event fields and keeps them', () => {
    const events = [{ type: 'x', data: { nested: [1, { extra: null }] }, unknown: 'kept' }]
    expect(validateInput(input({ events })).events).toEqual(events)
  })

  test('limits event nesting to 32 containers, counting the event object', () => {
    // Alternates arrays and objects, one container per level, inside the event object.
    const nest = (depth: number): Record<string, unknown> => {
      let value: unknown = 0
      for (let level = depth; level > 1; level--) value = level % 2 === 0 ? [value] : { nested: value }
      return { nested: value }
    }
    expect(validateInput(input({ events: [nest(32)] })).events).toHaveLength(1)
    rejects(() => validateInput(input({ events: [nest(33)] })), /nests deeper than 32 containers/)
  })

  test('accepts repeated writes to one path in order', () => {
    const writes = [{ path: 'a.txt', text: '1', permission: 'edit' }, { path: 'a.txt', text: '2', permission: 'edit' }]
    expect(validateInput(input({ permissions: [permission], writes })).writes).toEqual(writes)
  })

  test('requires every write to match its permission path', () => {
    rejects(() => validateInput(input({ permissions: [permission], writes: [{ path: 'b.txt', text: '', permission: 'edit' }] })),
      /differs from its permission path/)
  })

  test('rejects unknown and missing fields on structured records', () => {
    rejects(() => validateInput(input({ permissions: [{ ...permission, reason: 'x' }] })), /unknown field "reason"/)
    rejects(() => validateInput(input({ writes: [{ path: 'a.txt', permission: 'edit' }], permissions: [permission] })), /missing field "text"/)
    rejects(() => validateInput(input({ permissions: [{ ...permission, decision: 'ask' }] })), /"allow" or "deny"/)
    rejects(() => validateInput(input({ events: ['not an object'] })), /must be an object/)
    const missing = input()
    delete missing.events
    rejects(() => validateInput(missing), /missing field "events"/)
  })

  test('bounds item counts and string sizes, including event keys', () => {
    rejects(() => validateInput(input({ prompts: Array.from({ length: MAX_ITEMS + 1 }, () => '') })), /more than 64/)
    expect(validateInput(input({ prompts: ['x'.repeat(MAX_TEXT_BYTES)] })).prompts).toHaveLength(1)
    rejects(() => validateInput(input({ prompts: ['ก'.repeat(MAX_TEXT_BYTES / 3 + 1)] })), /UTF-8 bytes/)
    rejects(() => validateInput(input({ events: [{ ['k'.repeat(MAX_TEXT_BYTES + 1)]: 1 }] })), /UTF-8 bytes/)
  })
})

describe('fixture', () => {
  const fixture = JSON.parse(readFileSync(join(CONFORMANCE, 'fixtures', 'allow-write.json'), 'utf8')) as Fixture
  const variant = (change: (copy: Fixture) => void): Fixture => {
    const copy = structuredClone(fixture)
    change(copy)
    return copy
  }

  test('rejects initial paths that collide ignoring case or nest under a file', () => {
    rejects(() => validateFixture(variant(copy => copy.initialFiles.push({ path: 'CHECK.txt', hex: '' }))), /ignoring case/)
    rejects(() => validateFixture(variant(copy => copy.initialFiles.push({ path: 'check.txt/x', hex: '' }))), /as a directory/)
  })

  test('requires protected files to be initial files and hex to be lowercase bytes', () => {
    rejects(() => validateFixture(variant(copy => copy.protectedFiles.push('missing.txt'))), /not an initial file/)
    rejects(() => validateFixture(variant((copy) => { copy.initialFiles[0]!.hex = 'AB' })), /lowercase hex/)
    rejects(() => validateFixture(variant((copy) => { copy.initialFiles[0]!.hex = 'abc' })), /lowercase hex/)
    // The 64 KiB bound counts encoded bytes, so the hex text may be twice as long.
    expect(() => validateFixture(variant((copy) => { copy.initialFiles[0]!.hex = 'ab'.repeat(MAX_TEXT_BYTES) }))).not.toThrow()
    rejects(() => validateFixture(variant((copy) => { copy.initialFiles[0]!.hex = 'ab'.repeat(MAX_TEXT_BYTES + 1) })), /exceeds/)
  })

  test('rejects unknown fields in expected values and an unsafe id', () => {
    const extra = variant((copy) => { (copy.expected as Record<string, unknown>).exitCode = 0 })
    rejects(() => validateFixture(extra), /unknown field "exitCode"/)
    rejects(() => validateFixture(variant((copy) => { copy.id = '../x' })), /lowercase letters/)
  })
})
