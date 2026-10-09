/**
 * Runs the shared cases in `conformance/session/v1-to-v2-cases.json` through
 * the released v1→v2 migration's transformed stage. Each case decodes its
 * header and rows strictly with the released v0 or v1 codec, which must
 * succeed, then calls `sessionFormatV1ToV2.migrateHeader` and
 * `assertReleasedV2Header`, builds the stage with `sourceKind: 'transformed'`,
 * calls `transformEvent` for each decoded event into a
 * `SessionFormatEventCollector`, and finishes it. The development Rust
 * `migrate_v1_to_v2_transformed` in `rust/crates/bake-session` checks the same
 * table.
 *
 * This is the stage a chain runs after v0→v1. Production reads a v1 file with
 * the decoded stage instead, which first checks each payload; that stage is not
 * compared here. Packed chunk rows reach the stage as the decoder's expanded
 * `assistant/chunk` events, not through `transformRun`. A `rust` native-subset
 * marker names a case Rust deliberately does not decide; TypeScript still
 * asserts its own outcome, including the engine's `TypeError` text where the
 * stage's unchecked casts or the stream accumulator throw, and the `assertNever`
 * text for an unknown chunk type.
 */

import { readFileSync } from 'node:fs'
import path from 'node:path'
import { SessionFormatError, SessionFormatEventCollector } from 'bake-session-format'
import type { SessionFormatArtifactDecoder, SessionFormatHeader, SessionFormatMigrationStage } from 'bake-session-format'
import { RELEASED_V0_EVENT_TYPES, releasedV0SessionFormatCodec, releasedV1SessionFormatCodec } from 'bake-session-format-v0-to-v1'
import { describe, expect, it } from 'vitest'
import { assertReleasedV2Header, sessionFormatV1ToV2 } from '../src/index.ts'

const REPO = new URL('../../../../', import.meta.url)
const SCHEMA = 'bake/session-format-conformance/v1-to-v2-cases'
const ORACLE = "sessionFormatV1ToV2.migrateHeader and assertReleasedV2Header over a strict releasedV0SessionFormatCodec or releasedV1SessionFormatCodec decode, then createStage({ sourceKind: 'transformed' }), transformEvent for each decoded event into a SessionFormatEventCollector, then finish"
/** Both harnesses pin the table size, so a dropped case fails. */
const CASE_COUNT = 182
/**
 * Native limits: a chunk the stream accumulator refuses, a non-string event type, a value the stage
 * casts without checking, a fraction or exponent spelling the stage compares or the accumulator
 * reads, and an emitted `undefined` member.
 */
const LIMITS = ['chunk-shape', 'non-string-type', 'unchecked-shape', 'float-lexeme', 'undefined-member']
/**
 * Limits whose cases may end in an engine error rather than a SessionFormatError: the stage's
 * unchecked casts, and the accumulator's checks, which also read a fractional chunk time.
 */
const ENGINE_ERROR_LIMITS = ['chunk-shape', 'unchecked-shape', 'float-lexeme']

type At = 'header' | 'finish' | number
type Outcome =
  | { outcome: 'migrated'; header: unknown; events: unknown[]; inheritedEventCount: number }
  | { outcome: 'refused'; at: At; message: string }

interface MigrationCase {
  id: string
  version: 0 | 1
  header: string
  rows: string[]
  expect: Outcome
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
    if (value.outcome === 'refused' && keys === 'at,message,outcome' && isAt(value.at) && typeof value.message === 'string') {
      return value as Outcome
    }
  }
  throw new Error(`${id}: invalid outcome ${JSON.stringify(value)}`)
}

function parseCase(value: unknown): MigrationCase {
  if (!isObject(value) || typeof value.id !== 'string') throw new Error(`invalid case ${JSON.stringify(value)}`)
  const { id } = value
  const unknown = Object.keys(value).filter(key => !['id', 'version', 'header', 'rows', 'expect', 'rust', 'note'].includes(key))
  if (unknown.length > 0) throw new Error(`${id}: unknown keys ${unknown.join()}`)
  if (value.version !== 0 && value.version !== 1) throw new Error(`${id}: version must be 0 or 1`)
  if (typeof value.header !== 'string' || !Array.isArray(value.rows) || !value.rows.every(row => typeof row === 'string')) {
    throw new Error(`${id}: header and rows must be JSON text`)
  }
  if (value.note !== undefined && typeof value.note !== 'string') throw new Error(`${id}: invalid note`)
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
    version: value.version,
    header: value.header,
    rows: value.rows as string[],
    expect: parseOutcome(value.expect, id),
    ...(limit === undefined ? {} : { limit }),
  }
}

interface Table {
  vocabulary: { releasedV0EventTypes: unknown; objectPrototypeNames: unknown }
  cases: MigrationCase[]
}

function loadTable(): Table {
  const table: unknown = JSON.parse(readFileSync(new URL('conformance/session/v1-to-v2-cases.json', REPO), 'utf8'))
  if (!isObject(table) || sortedKeys(table) !== 'cases,history,oracle,schema,version,vocabulary'
    || table.schema !== SCHEMA || table.version !== 3 || table.oracle !== ORACLE || !Array.isArray(table.cases)
    || !Array.isArray(table.history) || !table.history.every(entry => typeof entry === 'string')
    || !isObject(table.vocabulary) || sortedKeys(table.vocabulary) !== 'objectPrototypeNames,releasedV0EventTypes') {
    throw new Error('v1-to-v2-cases.json does not match its version-3 schema')
  }
  const cases = table.cases.map(parseCase)
  if (new Set(cases.map(entry => entry.id)).size !== cases.length) throw new Error('case ids must be unique')
  return { vocabulary: table.vocabulary as Table['vocabulary'], cases }
}

interface Decoded {
  header: SessionFormatHeader
  inheritedEventCount: number
  events: unknown[]
}

/** The strict codec decode every case must pass before the stage runs. */
function decode(entry: MigrationCase, header: unknown, rows: readonly unknown[]): Decoded {
  const codec = entry.version === 0 ? releasedV0SessionFormatCodec : releasedV1SessionFormatCodec
  const decoder: SessionFormatArtifactDecoder = codec.createDecoder(header, 'strict')
  const collector = new SessionFormatEventCollector()
  for (const row of rows) decoder.decodeRow(row, collector)
  const inheritedEventCount = decoder.finish(collector)
  return { header: decoder.header, inheritedEventCount, events: collector.values }
}

interface Observed {
  outcome: Outcome
  /** The stage threw a `TypeError` or an `assertNever` error rather than a SessionFormatError. */
  engineError: boolean
}

function refused(at: At, error: unknown): Observed {
  if (error instanceof SessionFormatError) return { outcome: { outcome: 'refused', at, message: error.message }, engineError: false }
  if (error instanceof TypeError || error instanceof Error && error.message.startsWith('unreachable variant in ')) {
    return { outcome: { outcome: 'refused', at, message: error.message }, engineError: true }
  }
  throw error
}

function migrate(decoded: Decoded): Observed {
  let stage: SessionFormatMigrationStage
  let targetHeader: SessionFormatHeader
  try {
    targetHeader = sessionFormatV1ToV2.migrateHeader(decoded.header)
    assertReleasedV2Header(targetHeader)
    stage = sessionFormatV1ToV2.createStage({
      sourceHeader: decoded.header,
      targetHeader,
      sourceInheritedEventCount: decoded.inheritedEventCount,
      sourceKind: 'transformed',
    })
  } catch (error) {
    return refused('header', error)
  }
  const collector = new SessionFormatEventCollector()
  for (const [index, event] of decoded.events.entries()) {
    try {
      stage.transformEvent(event as Parameters<SessionFormatMigrationStage['transformEvent']>[0], collector)
    } catch (error) {
      return refused(index, error)
    }
  }
  let inheritedEventCount: number
  try {
    inheritedEventCount = stage.finish(collector)
  } catch (error) {
    return refused('finish', error)
  }
  return {
    outcome: { outcome: 'migrated', header: targetHeader, events: collector.values, inheritedEventCount },
    engineError: false,
  }
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

const { vocabulary, cases } = loadTable()

describe('shared v1 to v2 transformed-stage cases', () => {
  it('pin the table size and witness every native limit', () => {
    expect(cases).toHaveLength(CASE_COUNT)
    expect(new Set(cases.flatMap(entry => entry.limit?.limit ?? []))).toEqual(new Set(LIMITS))
  })

  it('share the released v0 vocabulary and the Object.prototype names', () => {
    expect(vocabulary.releasedV0EventTypes).toEqual([...RELEASED_V0_EVENT_TYPES].sort())
    expect([...vocabulary.objectPrototypeNames as string[]].sort()).toEqual(Object.getOwnPropertyNames(Object.prototype).sort())
  })

  it('judge every cwd the same on both platforms', () => {
    for (const entry of cases) {
      const cwd = cwdOf(entry)
      if (cwd !== undefined) expect(path.win32.isAbsolute(cwd), entry.id).toBe(path.posix.isAbsolute(cwd))
    }
  })

  for (const entry of cases) {
    it(entry.id, () => {
      const header: unknown = deepFreeze(JSON.parse(entry.header))
      const rows: unknown[] = deepFreeze(entry.rows.map(row => JSON.parse(row) as unknown))
      const before = ordered([header, rows])
      const decoded = decode(entry, header, rows)
      const decodedBefore = ordered(decoded.events)
      const actual = migrate(decoded)
      expect(ordered(actual.outcome), entry.id).toEqual(ordered(entry.expect))
      // Engine error text is pinned only where Rust names a limit.
      if (actual.engineError) expect(ENGINE_ERROR_LIMITS, `${entry.id} engine error`).toContain(entry.limit?.limit)
      expect(ordered([header, rows]), `${entry.id} inputs`).toEqual(before)
      expect(ordered(decoded.events), `${entry.id} decoded events`).toEqual(decodedBefore)
    })
  }
})
