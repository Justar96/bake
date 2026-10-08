/**
 * Runs the shared cases in `conformance/session/v2-to-v3-cases.json` through
 * the strict released v2 codec feeding the real v2→v3 migration chain. The
 * development Rust `migrate_v2_rows` in `rust/crates/bake-session` checks the
 * same table. Each case decodes its header, emits every row the decoder
 * admits into the chain stream, then finishes the decoder and the stream.
 * Agreement covers that stage output only: the catalog's final
 * `restoreReleasedV3Artifact`, which checks relationships, the protected
 * system head, and vocabulary, is deliberately not run. A `rust` native-subset
 * marker names a case Rust deliberately does not decide; TypeScript still
 * asserts its own outcome. A `win32` outcome replaces `expect` on Windows
 * hosts, where the header's `cwd` is judged by `path.win32.isAbsolute`.
 */

import { readFileSync } from 'node:fs'
import path from 'node:path'
import { createSessionFormatChain, SessionFormatError, SessionFormatEventCollector } from 'bake-session-format'
import type { SessionFormatArtifactDecoder, SessionFormatEvent, SessionFormatEventRun, SessionFormatMigrationContext, SessionFormatMigrationStream } from 'bake-session-format'
import { sessionFormatV0ToV1 } from 'bake-session-format-v0-to-v1'
import { RELEASED_V2_EVENT_TYPES, sessionFormatV1ToV2 } from 'bake-session-format-v1-to-v2'
import { describe, expect, it } from 'vitest'
import { assertReleasedV3Header, releasedV2SessionFormatCodec, sessionFormatV2ToV3 } from '../src/index.ts'

const REPO = new URL('../../../../', import.meta.url)
const SCHEMA = 'bake/session-format-conformance/v2-to-v3-cases'
const ORACLE = "releasedV2SessionFormatCodec.createDecoder(header, 'strict') feeding createSessionFormatChain([sessionFormatV0ToV1, sessionFormatV1ToV2, sessionFormatV2ToV3]).createStream; decoder.finish, then stream.finish"
/** Both harnesses pin the table size, so a dropped case fails. */
const CASE_COUNT = 367
/**
 * Inherent native limits: a fraction or exponent spelling where TypeScript reads a count or
 * safe integer, Rust's own source budget, and a V8 TypeError text the chain wraps.
 */
const LIMITS = ['header-float-lexeme', 'time-float-lexeme', 'payload-float-lexeme', 'source-output-budget', 'object-prototype-type', 'content-kind-diagnostic', 'unsafe-json-integer']
/** Event types the migration admits beyond the released v2 dispositions. */
const FEEDBACK_TYPES = ['feedback/message-put', 'feedback/message-delete']
const EVERY_FAMILY = 'every-source-event-family'

type At = 'header' | 'finish' | number
type Outcome =
  | { outcome: 'migrated'; header: unknown; events: unknown[]; inheritedEventCount: number }
  | { outcome: 'refused'; layer: 'codec' | 'migration'; at: At; message: string }

interface MigrationCase {
  id: string
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
    if (value.outcome === 'migrated' && keys === 'events,header,inheritedEventCount,outcome'
      && isObject(value.header) && Array.isArray(value.events) && Number.isSafeInteger(value.inheritedEventCount)) {
      return value as Outcome
    }
    if (value.outcome === 'refused' && keys === 'at,layer,message,outcome' && isAt(value.at)
      && (value.layer === 'codec' || value.layer === 'migration') && typeof value.message === 'string') {
      return value as Outcome
    }
  }
  throw new Error(`${id}: invalid outcome ${JSON.stringify(value)}`)
}

function parseCase(value: unknown): MigrationCase {
  if (!isObject(value) || typeof value.id !== 'string') throw new Error(`invalid case ${JSON.stringify(value)}`)
  const { id } = value
  const unknown = Object.keys(value).filter(key => !['id', 'header', 'rows', 'sourceBudget', 'expect', 'win32', 'rust'].includes(key))
  if (unknown.length > 0) throw new Error(`${id}: unknown keys ${unknown.join()}`)
  if (typeof value.header !== 'string' || !Array.isArray(value.rows) || !value.rows.every(row => typeof row === 'string')) {
    throw new Error(`${id}: header and rows must be JSON text`)
  }
  if (value.sourceBudget !== undefined && (!Number.isSafeInteger(value.sourceBudget) || (value.sourceBudget as number) < 0)) {
    throw new Error(`${id}: sourceBudget must be a non-negative integer`)
  }
  let limit: MigrationCase['limit']
  if (Object.hasOwn(value, 'rust')) {
    const { rust } = value
    if (!isObject(rust) || sortedKeys(rust) !== 'at,limit' || !LIMITS.includes(rust.limit as string) || !isAt(rust.at)) {
      throw new Error(`${id}: rust may only name a native-subset limit and its location`)
    }
    limit = { limit: rust.limit as string, at: rust.at }
  }
  return {
    id,
    header: value.header,
    rows: value.rows as string[],
    expect: parseOutcome(value.expect, id),
    ...(value.win32 === undefined ? {} : { win32: parseOutcome(value.win32, id) }),
    ...(limit === undefined ? {} : { limit }),
  }
}

function loadTable(): MigrationCase[] {
  const table: unknown = JSON.parse(readFileSync(new URL('conformance/session/v2-to-v3-cases.json', REPO), 'utf8'))
  if (!isObject(table) || sortedKeys(table) !== 'cases,oracle,schema,version'
    || table.schema !== SCHEMA || table.version !== 1 || table.oracle !== ORACLE || !Array.isArray(table.cases)) {
    throw new Error('v2-to-v3-cases.json does not match its version-1 schema')
  }
  const cases = table.cases.map(parseCase)
  if (new Set(cases.map(entry => entry.id)).size !== cases.length) throw new Error('case ids must be unique')
  return cases
}

const chain = createSessionFormatChain({
  currentVersion: 3,
  migrations: [sessionFormatV0ToV1, sessionFormatV1ToV2, sessionFormatV2ToV3],
  restoreCurrentHeader(header) {
    assertReleasedV3Header(header)
    return header
  },
})

interface Observed {
  outcome: Outcome
  /** The chain wrapped an engine error rather than a SessionFormatError. */
  engineError: boolean
}

function refused(layer: 'codec' | 'migration', at: At, error: unknown): Observed {
  if (!(error instanceof SessionFormatError)) throw error
  const engineError = error.cause !== undefined && !(error.cause instanceof SessionFormatError)
  return { outcome: { outcome: 'refused', layer, at, message: error.message }, engineError }
}

/** Strict codec rows feed the chain; the error that escapes the stream is attributed to the migration layer. */
function migrate(header: unknown, rows: readonly unknown[]): Observed {
  let decoder: SessionFormatArtifactDecoder
  try {
    decoder = releasedV2SessionFormatCodec.createDecoder(header, 'strict')
  } catch (error) {
    return refused('codec', 'header', error)
  }
  const collector = new SessionFormatEventCollector()
  let stream: SessionFormatMigrationStream
  try {
    stream = chain.createStream(decoder.header, decoder.headerInheritedEventCount, collector)
  } catch (error) {
    return refused('migration', 'header', error)
  }
  let migrationError: unknown
  const forward: SessionFormatMigrationContext = {
    emitEvent(event: SessionFormatEvent) {
      try {
        stream.emitEvent(event)
      } catch (error) {
        migrationError = error
        throw error
      }
    },
    emitRun(run: SessionFormatEventRun) {
      try {
        stream.emitRun(run)
      } catch (error) {
        migrationError = error
        throw error
      }
    },
  }
  for (const [index, row] of rows.entries()) {
    try {
      decoder.decodeRow(row, forward)
    } catch (error) {
      return refused(error === migrationError ? 'migration' : 'codec', index, error)
    }
  }
  try {
    decoder.finish(forward)
  } catch (error) {
    return refused(error === migrationError ? 'migration' : 'codec', 'finish', error)
  }
  let inheritedEventCount: number
  try {
    inheritedEventCount = stream.finish()
  } catch (error) {
    return refused('migration', 'finish', error)
  }
  return { outcome: { outcome: 'migrated', header: stream.header, events: collector.values, inheritedEventCount }, engineError: false }
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

function cwdOf(entry: MigrationCase): string | undefined {
  const header: unknown = JSON.parse(entry.header)
  return isObject(header) && typeof header.cwd === 'string' ? header.cwd : undefined
}

function sourceTypes(entry: MigrationCase): string[] {
  return entry.rows.map(row => (JSON.parse(row) as { type: string }).type)
}

const cases = loadTable()
const platform = process.platform === 'win32' ? 'win32' : 'posix'

describe('shared v2 to v3 migration cases', () => {
  it('pin the table size and witness every native limit', () => {
    expect(cases).toHaveLength(CASE_COUNT)
    expect(new Set(cases.flatMap(entry => entry.limit?.limit ?? []))).toEqual(new Set(LIMITS))
  })

  it('cover every released source event family in one migrated history', () => {
    const families = [...RELEASED_V2_EVENT_TYPES, ...FEEDBACK_TYPES].sort()
    const every = cases.find(entry => entry.id === EVERY_FAMILY)!
    expect(every.expect.outcome).toBe('migrated')
    expect([...new Set(sourceTypes(every))].sort()).toEqual(families)
    for (const type of families) {
      const slug = type.replaceAll('/', '-')
      for (const suffix of ['unexpected-member', 'data-not-object']) {
        expect(cases.some(entry => entry.id === `family-${slug}-${suffix}`), `${type} ${suffix}`).toBe(true)
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
      const actual = migrate(header, rows)
      const expected = platform === 'win32' ? entry.win32 ?? entry.expect : entry.expect
      expect(ordered(actual.outcome), entry.id).toEqual(ordered(expected))
      // Engine error text is pinned only where Rust names the limit.
      expect(actual.engineError, `${entry.id} engine error`).toBe(entry.limit?.limit === 'object-prototype-type')
      expect(ordered([header, rows]), `${entry.id} inputs`).toEqual(before)
    })
  }
})
