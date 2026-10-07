/**
 * Runs the shared Session header cases in `conformance/session/header-cases.json`
 * through the real header parser, the `SessionLogScanner` constructor. The
 * development Rust reader in `rust/crates/bake-session` checks the same table,
 * so every TypeScript outcome here is the oracle for its Rust expectation.
 * A `rust` native-subset override names a case Rust deliberately does not
 * decide; TypeScript still asserts its own outcome. The spec reads only the
 * table and the one fixture log it names.
 */

import { readFileSync } from 'node:fs'
import path from 'node:path'
import { SessionFormatUnsupportedError } from 'bake-session-persistence'
import { describe, expect, it } from 'vitest'
import { scanLog, SessionLogScanner } from '../src/format.ts'

const REPO = new URL('../../../../', import.meta.url)
const SCHEMA = 'bake/session-format-conformance/header-cases'
const ORACLE = "new SessionLogScanner(record, 'strict') from packages/session/session-persistence-jsonl/src/format.ts"
/** The only log a case may take its first record from. */
const FIXTURE = 'conformance/runtime/request-reconstruction/tool-call-turn/session.jsonl'
const SOURCES = ['record', 'bytesHex', 'fixtureFirstRecord'] as const
const LIMITS = ['invalid-utf8', 'json-parser', 'float-lexeme', 'version-diagnostic']
const META_REQUIRED = ['version', 'id', 'createdAt', 'isSeeded', 'delegationDepth']
const META_OPTIONAL = ['cwd', 'parentSession', 'origin', 'agentPreset']
/** Each rejection reason and the exact message the parser throws for it. */
const REASONS = new Map([
  ['empty or header-less session log', 'framing'],
  ['corrupt session log: header line is not valid JSON', 'json'],
  ['corrupt session log: first line is not a JSON object', 'not-object'],
  ['session header uses retired policy baseline fields', 'retired-policy-fields'],
  ['corrupt session log: first line is not a session header', 'not-session-header'],
])

type Outcome =
  | { outcome: 'admitted' }
  // The scanner threw a TypeError; only a native-subset case may expect it.
  | { outcome: 'type-error' }
  | { outcome: 'unsupported'; newer: boolean }
  | { outcome: 'rejected'; reason: string }

interface HeaderCase {
  id: string
  record: Buffer
  ts: { posix: Outcome; win32: Outcome }
  /** Rust declines the case with a native-subset refusal. */
  subset: boolean
  meta?: Record<string, unknown>
}

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

function sortedKeys(value: Record<string, unknown>): string[] {
  return Object.keys(value).sort()
}

function parseOutcome(value: unknown, context: string): Outcome {
  if (isObject(value)) {
    const keys = sortedKeys(value).join()
    if (value.outcome === 'admitted' && keys === 'outcome') return { outcome: 'admitted' }
    if (value.outcome === 'type-error' && keys === 'outcome') return { outcome: 'type-error' }
    if (value.outcome === 'unsupported' && keys === 'newer,outcome' && typeof value.newer === 'boolean') {
      return { outcome: 'unsupported', newer: value.newer }
    }
    if (value.outcome === 'rejected' && keys === 'outcome,reason' && [...REASONS.values()].includes(value.reason as string)) {
      return { outcome: 'rejected', reason: value.reason as string }
    }
  }
  throw new Error(`${context}: invalid outcome ${JSON.stringify(value)}`)
}

function recordBytes(entry: Record<string, unknown>, id: string): Buffer {
  const sources = SOURCES.filter(key => Object.hasOwn(entry, key))
  if (sources.length !== 1) throw new Error(`${id}: exactly one record source`)
  const source = entry[sources[0]!]
  if (typeof source !== 'string') throw new Error(`${id}: source must be a string`)
  if (sources[0] === 'record') return Buffer.from(`${source}\n`)
  if (sources[0] === 'bytesHex') {
    if (!/^(?:[0-9a-f]{2})*$/.test(source)) throw new Error(`${id}: bytesHex must be lowercase hex`)
    return Buffer.from(source, 'hex')
  }
  if (source !== FIXTURE) throw new Error(`${id}: unsupported fixture path ${source}`)
  const log = readFileSync(new URL(FIXTURE, REPO))
  return log.subarray(0, log.indexOf(0x0A) + 1)
}

function parseCase(value: unknown): HeaderCase {
  if (!isObject(value) || typeof value.id !== 'string') throw new Error(`invalid case ${JSON.stringify(value)}`)
  const { id } = value
  const unknown = Object.keys(value).filter(key => !['id', 'ts', 'rust', 'meta', ...SOURCES].includes(key))
  if (unknown.length > 0) throw new Error(`${id}: unknown keys ${unknown.join()}`)
  const ts = isObject(value.ts) && sortedKeys(value.ts).join() === 'posix,win32'
    ? { posix: parseOutcome(value.ts.posix, id), win32: parseOutcome(value.ts.win32, id) }
    : { posix: parseOutcome(value.ts, id), win32: parseOutcome(value.ts, id) }
  const subset = Object.hasOwn(value, 'rust')
  if (subset) {
    const { rust } = value
    if (!isObject(rust) || sortedKeys(rust).join() !== 'limit,outcome' || rust.outcome !== 'native-subset'
      || !LIMITS.includes(rust.limit as string)) {
      throw new Error(`${id}: rust may only name a native-subset limit`)
    }
  }
  if (!subset && (ts.posix.outcome === 'type-error' || ts.win32.outcome === 'type-error')) {
    throw new Error(`${id}: a TypeError outcome needs a native-subset override`)
  }
  const admits = !subset && (ts.posix.outcome === 'admitted' || ts.win32.outcome === 'admitted')
  if (Object.hasOwn(value, 'meta') !== admits) throw new Error(`${id}: meta is required exactly when the case admits`)
  const { meta } = value
  if (meta !== undefined) {
    if (!isObject(meta) || !META_REQUIRED.every(key => Object.hasOwn(meta, key))
      || !Object.keys(meta).every(key => META_REQUIRED.includes(key) || META_OPTIONAL.includes(key))) {
      throw new Error(`${id}: invalid meta keys`)
    }
  }
  return { id, record: recordBytes(value, id), ts, subset, ...meta === undefined ? {} : { meta } }
}

function loadTable(): { cases: HeaderCase[]; absolutePaths: { path: string; posix: boolean; win32: boolean }[] } {
  const table: unknown = JSON.parse(readFileSync(new URL('conformance/session/header-cases.json', REPO), 'utf8'))
  if (!isObject(table) || sortedKeys(table).join() !== 'absolutePaths,cases,oracle,schema,version'
    || table.schema !== SCHEMA || table.version !== 1 || table.oracle !== ORACLE
    || !Array.isArray(table.cases) || !Array.isArray(table.absolutePaths)) {
    throw new Error('header-cases.json does not match its version-1 schema')
  }
  const cases = table.cases.map(parseCase)
  const ids = new Set(cases.map(entry => entry.id))
  if (ids.size !== cases.length) throw new Error('header case ids must be unique')
  const absolutePaths = table.absolutePaths.map((row: unknown) => {
    if (!isObject(row) || sortedKeys(row).join() !== 'path,posix,win32' || typeof row.path !== 'string'
      || typeof row.posix !== 'boolean' || typeof row.win32 !== 'boolean') {
      throw new Error(`invalid absolutePaths row ${JSON.stringify(row)}`)
    }
    return { path: row.path, posix: row.posix, win32: row.win32 }
  })
  return { cases, absolutePaths }
}

/** Classifies the scanner constructor's outcome; an unmapped error fails the case. */
function scannerOutcome(record: Buffer): Outcome {
  try {
    new SessionLogScanner(record, 'strict')
    return { outcome: 'admitted' }
  } catch (error) {
    if (!(error instanceof Error)) throw error
    if (error instanceof SessionFormatUnsupportedError) {
      // The message embeds the Session id, so only its fixed ending identifies the direction.
      if (error.message.endsWith('written by a newer harness — upgrade the harness to open it')) return { outcome: 'unsupported', newer: true }
      if (error.message.endsWith('and this build ships no upgrade path for it')) return { outcome: 'unsupported', newer: false }
      throw error
    }
    if (error instanceof TypeError && error.message === 'Cannot convert object to primitive value') {
      return { outcome: 'type-error' }
    }
    const reason = REASONS.get(error.message)
    if (reason === undefined) throw error
    return { outcome: 'rejected', reason }
  }
}

const { cases, absolutePaths } = loadTable()
const platform = process.platform === 'win32' ? 'win32' : 'posix'

describe('shared Session header cases', () => {
  it('cover native-subset and per-platform rows', () => {
    expect(cases.some(entry => entry.subset)).toBe(true)
    expect(cases.some(entry => entry.ts.posix.outcome !== entry.ts.win32.outcome)).toBe(true)
  })

  for (const entry of cases) {
    it(entry.id, () => {
      const expected = entry.ts[platform]
      expect(scannerOutcome(entry.record), entry.id).toEqual(expected)
      // A seeded header's metadata is observable only after a complete log; its constructor outcome suffices.
      if (expected.outcome === 'admitted' && entry.meta !== undefined && entry.meta.isSeeded === false) {
        expect(scanLog(entry.record).meta, entry.id).toEqual(entry.meta)
      }
    })
  }

  it('match both Node path flavors on every host, and the host flavor in the scanner', () => {
    for (const row of absolutePaths) {
      expect(path.posix.isAbsolute(row.path), row.path).toBe(row.posix)
      expect(path.win32.isAbsolute(row.path), row.path).toBe(row.win32)
      expect(path.isAbsolute(row.path), row.path).toBe(row[platform])
    }
  })
})
