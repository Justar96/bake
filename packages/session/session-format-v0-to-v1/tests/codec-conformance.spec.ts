/**
 * Runs the shared cases in `conformance/session/v1-codec-cases.json` through
 * the released v0 and v1 physical codecs: `createDecoder(header, recovery)`,
 * `decodeRow` for each row into a `SessionFormatEventCollector`, which expands
 * packed Assistant chunk runs, then `finish`. No migration runs. The
 * development Rust `decode_v0_v1_rows` in `rust/crates/bake-session` checks
 * the same table. A `rust` native-subset marker names a case Rust
 * deliberately does not decide; TypeScript still asserts its own outcome. A
 * `win32` outcome replaces `expect` on Windows hosts, where the header's `cwd`
 * is judged by `path.win32.isAbsolute`.
 */

import { readFileSync } from 'node:fs'
import path from 'node:path'
import { SessionFormatError, SessionFormatEventCollector } from 'bake-session-format'
import type { SessionFormatArtifactDecoder, SessionFormatRecovery } from 'bake-session-format'
import { describe, expect, it } from 'vitest'
import { releasedV0SessionFormatCodec, releasedV1SessionFormatCodec } from '../src/codec.ts'

const REPO = new URL('../../../../', import.meta.url)
const SCHEMA = 'bake/session-format-conformance/v1-codec-cases'
const ORACLE = 'releasedV0SessionFormatCodec or releasedV1SessionFormatCodec .createDecoder(header, recovery), decodeRow for each row into a SessionFormatEventCollector, then finish'
/** Both harnesses pin the table size, so a dropped case fails. */
const CASE_COUNT = 140
/**
 * Native limits: a fraction or exponent spelling where TypeScript compares or reads a number, a
 * `String` conversion of an array or object seq, Rust's own source budget, and a retained integer
 * that `JSON.parse` rounds.
 */
const LIMITS = ['header-float-lexeme', 'seq-float-lexeme', 'seq-diagnostic', 'source-float-lexeme', 'packed-float-lexeme', 'source-output-budget', 'unsafe-json-integer']

type At = 'header' | 'finish' | number
type Outcome =
  | { outcome: 'decoded'; header: unknown; inheritedEventCount: number; events: unknown[] }
  | { outcome: 'refused'; at: At; message: string }

interface CodecCase {
  id: string
  version: 0 | 1
  recovery: SessionFormatRecovery
  header: string
  rows: string[]
  expect: Outcome
  win32?: Outcome
  limit?: { limit: string; at: At }
}

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

function sortedKeys(value: Record<string, unknown>): string {
  return Object.keys(value).sort().join()
}

function isAt(value: unknown): value is At {
  return value === 'header' || value === 'finish' || Number.isSafeInteger(value) && (value as number) >= 0
}

function parseOutcome(value: unknown, id: string): Outcome {
  if (isObject(value)) {
    const keys = sortedKeys(value)
    if (value.outcome === 'decoded' && keys === 'events,header,inheritedEventCount,outcome'
      && isObject(value.header) && Array.isArray(value.events) && Number.isSafeInteger(value.inheritedEventCount)) {
      return value as Outcome
    }
    if (value.outcome === 'refused' && keys === 'at,message,outcome' && isAt(value.at) && typeof value.message === 'string') {
      return value as Outcome
    }
  }
  throw new Error(`${id}: invalid outcome ${JSON.stringify(value)}`)
}

function parseCase(value: unknown): CodecCase {
  if (!isObject(value) || typeof value.id !== 'string') throw new Error(`invalid case ${JSON.stringify(value)}`)
  const { id } = value
  const unknown = Object.keys(value).filter(key => !['id', 'version', 'recovery', 'header', 'rows', 'sourceBudget', 'expect', 'win32', 'rust', 'note'].includes(key))
  if (unknown.length > 0) throw new Error(`${id}: unknown keys ${unknown.join()}`)
  if (value.version !== 0 && value.version !== 1) throw new Error(`${id}: version must be 0 or 1`)
  if (value.recovery !== 'strict' && value.recovery !== 'recoverable') throw new Error(`${id}: invalid recovery`)
  if (typeof value.header !== 'string' || !Array.isArray(value.rows) || !value.rows.every(row => typeof row === 'string')) {
    throw new Error(`${id}: header and rows must be JSON text`)
  }
  if (value.sourceBudget !== undefined && (!Number.isSafeInteger(value.sourceBudget) || (value.sourceBudget as number) < 0)) {
    throw new Error(`${id}: sourceBudget must be a non-negative integer`)
  }
  if (value.note !== undefined && typeof value.note !== 'string') throw new Error(`${id}: invalid note`)
  let limit: CodecCase['limit']
  if (Object.hasOwn(value, 'rust')) {
    const { rust } = value
    if (!isObject(rust) || sortedKeys(rust) !== 'at,limit' || !LIMITS.includes(rust.limit as string) || !isAt(rust.at)) {
      throw new Error(`${id}: rust may only name a native-subset limit and its location`)
    }
    limit = { limit: rust.limit as string, at: rust.at }
  }
  return {
    id,
    version: value.version,
    recovery: value.recovery,
    header: value.header,
    rows: value.rows as string[],
    expect: parseOutcome(value.expect, id),
    ...(value.win32 === undefined ? {} : { win32: parseOutcome(value.win32, id) }),
    ...(limit === undefined ? {} : { limit }),
  }
}

function loadTable(): CodecCase[] {
  const table: unknown = JSON.parse(readFileSync(new URL('conformance/session/v1-codec-cases.json', REPO), 'utf8'))
  if (!isObject(table) || sortedKeys(table) !== 'cases,history,oracle,schema,version'
    || table.schema !== SCHEMA || table.version !== 1 || table.oracle !== ORACLE || !Array.isArray(table.cases)
    || !Array.isArray(table.history) || !table.history.every(entry => typeof entry === 'string')) {
    throw new Error('v1-codec-cases.json does not match its version-1 schema')
  }
  const cases = table.cases.map(parseCase)
  if (new Set(cases.map(entry => entry.id)).size !== cases.length) throw new Error('case ids must be unique')
  return cases
}

function refused(at: At, error: unknown): Outcome {
  if (!(error instanceof SessionFormatError)) throw error
  return { outcome: 'refused', at, message: error.message }
}

function decode(entry: CodecCase, header: unknown, rows: readonly unknown[]): Outcome {
  const codec = entry.version === 0 ? releasedV0SessionFormatCodec : releasedV1SessionFormatCodec
  let decoder: SessionFormatArtifactDecoder
  try {
    decoder = codec.createDecoder(header, entry.recovery)
  } catch (error) {
    return refused('header', error)
  }
  const collector = new SessionFormatEventCollector()
  for (const [index, row] of rows.entries()) {
    try {
      decoder.decodeRow(row, collector)
    } catch (error) {
      return refused(index, error)
    }
  }
  let inheritedEventCount: number
  try {
    inheritedEventCount = decoder.finish(collector)
  } catch (error) {
    return refused('finish', error)
  }
  expect(decoder.headerInheritedEventCount, `${entry.id} header cut`).toBe(inheritedEventCount)
  return { outcome: 'decoded', header: decoder.header, inheritedEventCount, events: collector.values }
}

/** Object members as ordered pairs, so equality also checks member order and `Object.is` numbers. */
function ordered(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(ordered)
  if (isObject(value)) return Object.entries(value).map(([key, item]) => [key, ordered(item)])
  return value
}

function deepFreeze<T>(value: T): T {
  if (typeof value === 'object' && value !== null) {
    for (const item of Object.values(value)) deepFreeze(item)
    Object.freeze(value)
  }
  return value
}

function cwdOf(entry: CodecCase): string | undefined {
  const header: unknown = JSON.parse(entry.header)
  return isObject(header) && typeof header.cwd === 'string' ? header.cwd : undefined
}

const cases = loadTable()
const platform = process.platform === 'win32' ? 'win32' : 'posix'

describe('shared released v0/v1 codec cases', () => {
  it('pin the table size and witness every native limit and mode', () => {
    expect(cases).toHaveLength(CASE_COUNT)
    expect(new Set(cases.flatMap(entry => entry.limit?.limit ?? []))).toEqual(new Set(LIMITS))
    for (const version of [0, 1]) {
      for (const recovery of ['strict', 'recoverable']) {
        expect(cases.some(entry => entry.version === version && entry.recovery === recovery), `v${version} ${recovery}`).toBe(true)
      }
    }
  })

  it('give a win32 outcome exactly where the cwd is absolute on one platform only', () => {
    for (const entry of cases) {
      const cwd = cwdOf(entry)
      const differs = cwd !== undefined && path.win32.isAbsolute(cwd) !== path.posix.isAbsolute(cwd)
      expect(entry.win32 !== undefined, entry.id).toBe(differs)
    }
    expect(cases.some(entry => entry.win32 !== undefined)).toBe(true)
  })

  for (const entry of cases) {
    it(entry.id, () => {
      const header: unknown = deepFreeze(JSON.parse(entry.header))
      const rows: unknown[] = deepFreeze(entry.rows.map(row => JSON.parse(row) as unknown))
      const before = ordered([header, rows])
      const actual = decode(entry, header, rows)
      const expected = platform === 'win32' ? entry.win32 ?? entry.expect : entry.expect
      expect(ordered(actual), entry.id).toEqual(ordered(expected))
      expect(ordered([header, rows]), `${entry.id} inputs`).toEqual(before)
    })
  }
})
