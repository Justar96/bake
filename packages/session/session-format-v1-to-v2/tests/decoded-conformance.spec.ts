/**
 * Runs the shared cases in `conformance/session/v1-to-v2-decoded-cases.json`
 * through the released v1→v2 migration's decoded stage, the stage a chain
 * builds when v1 is its first source format. Each case first decodes alone
 * with the strict released v0 or v1 codec, which must succeed. It then calls
 * `sessionFormatV1ToV2.migrateHeader` and `assertReleasedV2Header`, builds the
 * stage with `sourceKind: 'decoded'`, and decodes the rows again into a
 * context that passes each event to `transformEvent` and each packed run to
 * `transformRun`, as production does, before finishing the stage. A refusal is
 * located at the header, at the index of the decoded event being migrated, or
 * at finish, and records whether the stage threw a `SessionFormatError`, a
 * `SessionFormatUnsupportedMigrationError`, or an engine error. The
 * development Rust `migrate_v1_to_v2_decoded` in `rust/crates/bake-session`
 * checks the same table. A `rust` native-subset marker names a case Rust
 * deliberately does not decide; TypeScript still asserts its own outcome.
 */

import { readFileSync } from 'node:fs'
import path from 'node:path'
import {
  SessionFormatError,
  SessionFormatEventCollector,
  SessionFormatUnsupportedMigrationError,
} from 'bake-session-format'
import type {
  SessionFormatCodec,
  SessionFormatEvent,
  SessionFormatEventRun,
  SessionFormatHeader,
  SessionFormatMigrationContext,
  SessionFormatMigrationStage,
} from 'bake-session-format'
import { releasedV0SessionFormatCodec, releasedV1SessionFormatCodec } from 'bake-session-format-v0-to-v1'
import { describe, expect, it } from 'vitest'
import { assertReleasedV2Header, sessionFormatV1ToV2 } from '../src/index.ts'

const REPO = new URL('../../../../', import.meta.url)
const SCHEMA = 'bake/session-format-conformance/v1-to-v2-decoded-cases'
const ORACLE = "sessionFormatV1ToV2.migrateHeader and assertReleasedV2Header over a strict releasedV0SessionFormatCodec or releasedV1SessionFormatCodec decoder, then createStage({ sourceKind: 'decoded' }); the decoder emits each event to transformEvent and each packed run to transformRun, then the stage finishes"
/** Both harnesses pin the table size, so a dropped case fails. */
const CASE_COUNT = 50
/**
 * Native limits: an Assistant chunk, whose packed run the expanded Rust input cannot distinguish; a
 * non-string type, which the disposition lookup coerces; and the payload-check and transformed-stage
 * limits this table witnesses under their step's prefix.
 */
const LIMITS = [
  'assistant-chunk',
  'non-string-type',
  'payload/object-prototype-type',
  'payload/payload-float-lexeme',
  'payload/legacy-goal-message',
  'transformed/unchecked-shape',
  'transformed/undefined-member',
]

type At = 'header' | 'finish' | number
type ErrorClass = 'format' | 'unsupported' | 'engine'
type Outcome =
  | { outcome: 'migrated'; header: unknown; events: unknown[]; inheritedEventCount: number }
  | { outcome: 'refused'; at: At; class: ErrorClass; message: string }

interface DecodedCase {
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
    if (value.outcome === 'refused' && keys === 'at,class,message,outcome' && isAt(value.at)
      && ['format', 'unsupported', 'engine'].includes(value.class as string) && typeof value.message === 'string') {
      return value as Outcome
    }
  }
  throw new Error(`${id}: invalid outcome ${JSON.stringify(value)}`)
}

function parseCase(value: unknown): DecodedCase {
  if (!isObject(value) || typeof value.id !== 'string') throw new Error(`invalid case ${JSON.stringify(value)}`)
  const { id } = value
  const unknown = Object.keys(value).filter(key => !['id', 'version', 'header', 'rows', 'expect', 'rust', 'note'].includes(key))
  if (unknown.length > 0) throw new Error(`${id}: unknown keys ${unknown.join()}`)
  if (value.version !== 0 && value.version !== 1) throw new Error(`${id}: version must be 0 or 1`)
  if (typeof value.header !== 'string' || !Array.isArray(value.rows) || !value.rows.every(row => typeof row === 'string')) {
    throw new Error(`${id}: header and rows must be JSON text`)
  }
  if (value.note !== undefined && typeof value.note !== 'string') throw new Error(`${id}: invalid note`)
  let limit: DecodedCase['limit']
  if (Object.hasOwn(value, 'rust')) {
    const { rust } = value
    if (!isObject(rust) || sortedKeys(rust) !== 'at,limit' || !LIMITS.includes(rust.limit as string) || !isAt(rust.at)) {
      throw new Error(`${id}: rust may only name a native-subset limit and its location`)
    }
    limit = { limit: rust.limit as string, at: rust.at }
  }
  const expected = parseOutcome(value.expect, id)
  if (expected.outcome === 'refused' && expected.class === 'engine' && limit === undefined) {
    throw new Error(`${id}: an engine error needs a rust marker`)
  }
  return {
    id,
    version: value.version,
    header: value.header,
    rows: value.rows as string[],
    expect: expected,
    ...(limit === undefined ? {} : { limit }),
  }
}

function loadTable(): DecodedCase[] {
  const table: unknown = JSON.parse(readFileSync(new URL('conformance/session/v1-to-v2-decoded-cases.json', REPO), 'utf8'))
  if (!isObject(table) || sortedKeys(table) !== 'cases,history,oracle,schema,version'
    || table.schema !== SCHEMA || table.version !== 1 || table.oracle !== ORACLE || !Array.isArray(table.cases)
    || !Array.isArray(table.history) || !table.history.every(entry => typeof entry === 'string')) {
    throw new Error('v1-to-v2-decoded-cases.json does not match its version-1 schema')
  }
  const cases = table.cases.map(parseCase)
  if (new Set(cases.map(entry => entry.id)).size !== cases.length) throw new Error('case ids must be unique')
  return cases
}

function codecOf(entry: DecodedCase): SessionFormatCodec {
  return entry.version === 0 ? releasedV0SessionFormatCodec : releasedV1SessionFormatCodec
}

/** The codec alone must admit every row and finish; Rust reads only such a decode. */
function decodes(codec: SessionFormatCodec, header: unknown, rows: readonly unknown[]): boolean {
  try {
    const decoder = codec.createDecoder(header, 'strict')
    const collector = new SessionFormatEventCollector()
    for (const row of rows) decoder.decodeRow(row, collector)
    decoder.finish(collector)
    return true
  } catch (error) {
    if (error instanceof SessionFormatError) return false
    throw error
  }
}

function refused(at: At, error: unknown): Outcome {
  if (error instanceof SessionFormatUnsupportedMigrationError) return { outcome: 'refused', at, class: 'unsupported', message: error.message }
  if (error instanceof SessionFormatError) return { outcome: 'refused', at, class: 'format', message: error.message }
  if (error instanceof TypeError || error instanceof Error && error.message.startsWith('unreachable variant in ')) {
    return { outcome: 'refused', at, class: 'engine', message: error.message }
  }
  throw error
}

function migrate(codec: SessionFormatCodec, header: unknown, rows: readonly unknown[]): Outcome {
  const decoder = codec.createDecoder(header, 'strict')
  let stage: SessionFormatMigrationStage
  let targetHeader: SessionFormatHeader
  try {
    targetHeader = sessionFormatV1ToV2.migrateHeader(decoder.header)
    assertReleasedV2Header(targetHeader)
    stage = sessionFormatV1ToV2.createStage({
      sourceHeader: decoder.header,
      targetHeader,
      sourceInheritedEventCount: decoder.headerInheritedEventCount,
      sourceKind: 'decoded',
    })
  } catch (error) {
    return refused('header', error)
  }
  const collector = new SessionFormatEventCollector()
  let emitted = 0
  let failure: { at: number; error: unknown } | undefined
  const forward: SessionFormatMigrationContext = {
    emitEvent(event: SessionFormatEvent) {
      try {
        stage.transformEvent(event, collector)
      } catch (error) {
        failure = { at: emitted, error }
        throw error
      }
      emitted += 1
    },
    emitRun(run: SessionFormatEventRun) {
      try {
        stage.transformRun(run, collector)
      } catch (error) {
        failure = { at: emitted, error }
        throw error
      }
      emitted += run.eventCount
    },
  }
  try {
    for (const row of rows) decoder.decodeRow(row, forward)
    decoder.finish(forward)
  } catch (error) {
    if (failure === undefined || failure.error !== error) throw error
    return refused(failure.at, error)
  }
  let inheritedEventCount: number
  try {
    inheritedEventCount = stage.finish(collector)
  } catch (error) {
    return refused('finish', error)
  }
  return { outcome: 'migrated', header: targetHeader, events: collector.values, inheritedEventCount }
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

const cases = loadTable()

describe('shared v1 to v2 decoded-stage cases', () => {
  it('pin the table size and witness every native limit', () => {
    expect(cases).toHaveLength(CASE_COUNT)
    expect(new Set(cases.flatMap(entry => entry.limit?.limit ?? []))).toEqual(new Set(LIMITS))
  })

  it('use a cwd that both path platforms judge alike', () => {
    for (const entry of cases) {
      const header: unknown = JSON.parse(entry.header)
      const cwd = isObject(header) && typeof header.cwd === 'string' ? header.cwd : undefined
      if (cwd !== undefined) expect(path.win32.isAbsolute(cwd), entry.id).toBe(path.posix.isAbsolute(cwd))
    }
  })

  for (const entry of cases) {
    it(entry.id, () => {
      const codec = codecOf(entry)
      const header: unknown = deepFreeze(JSON.parse(entry.header))
      const rows: unknown[] = deepFreeze(entry.rows.map(row => JSON.parse(row) as unknown))
      const before = ordered([header, rows])
      expect(decodes(codec, header, rows), `${entry.id} decodes`).toBe(true)
      const actual = migrate(codec, header, rows)
      expect(ordered(actual), entry.id).toEqual(ordered(entry.expect))
      expect(ordered([header, rows]), `${entry.id} inputs`).toEqual(before)
    })
  }
})
