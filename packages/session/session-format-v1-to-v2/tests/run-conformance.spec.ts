/**
 * Runs the shared cases in `conformance/session/v1-to-v2-run-cases.json`
 * through the released v1→v2 migration's transformed stage as a chain reading
 * the file feeds it. Each case decodes its header and rows strictly with the
 * released v1 codec, which must succeed, keeping each emitted event and each
 * emitted packed Assistant chunk run in order. It then calls
 * `sessionFormatV1ToV2.migrateHeader` and `assertReleasedV2Header`, builds the
 * stage with `sourceKind: 'transformed'`, passes each event to `transformEvent`
 * and each run to `transformRun` with a `SessionFormatEventCollector`, and
 * finishes it. The development Rust `migrate_v1_to_v2_transformed_items` in
 * `rust/crates/bake-session` checks the same table.
 *
 * Each case also runs the expanded path, which passes a run's expanded events
 * to `transformEvent`, with a refusal located at the row that emitted the
 * event. The two paths must differ exactly in the cases flagged
 * `expandedDiverges`. Rows are not frozen: `transformRun` keeps a run's stream
 * record and appends later records' members to its arrays, as it does with
 * parsed rows in production. A `rust` native-subset marker names a case Rust
 * deliberately does not decide; TypeScript still asserts its own outcome.
 */

import { readFileSync } from 'node:fs'
import { SessionFormatError, SessionFormatEventCollector } from 'bake-session-format'
import type {
  SessionFormatEvent,
  SessionFormatEventRun,
  SessionFormatHeader,
  SessionFormatMigrationContext,
  SessionFormatMigrationStage,
} from 'bake-session-format'
import { releasedV1SessionFormatCodec } from 'bake-session-format-v0-to-v1'
import { describe, expect, it } from 'vitest'
import { assertReleasedV2Header, sessionFormatV1ToV2 } from '../src/index.ts'

const REPO = new URL('../../../../', import.meta.url)
const SCHEMA = 'bake/session-format-conformance/v1-to-v2-run-cases'
const ORACLE = "sessionFormatV1ToV2.migrateHeader and assertReleasedV2Header over a strict releasedV1SessionFormatCodec decode, then createStage({ sourceKind: 'transformed' }), transformEvent for each emitted event and transformRun for each emitted run into a SessionFormatEventCollector, then finish"
/** Both harnesses pin the table size, so a dropped case fails. */
const CASE_COUNT = 34
/**
 * Native limits `transformRun` reaches: a pending attempt's coordinate that is an object or array,
 * or spelled with a fraction or exponent, and an emitted attempt with an `undefined` member.
 */
const LIMITS = ['unchecked-shape', 'float-lexeme', 'undefined-member']

type At = 'header' | 'finish' | number
type Outcome =
  | { outcome: 'migrated'; header: unknown; events: unknown[]; inheritedEventCount: number }
  | { outcome: 'refused'; at: At; message: string }

interface RunCase {
  id: string
  header: string
  rows: string[]
  expect: Outcome
  limit?: { limit: string; at: At }
  expandedDiverges: boolean
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

function parseCase(value: unknown): RunCase {
  if (!isObject(value) || typeof value.id !== 'string') throw new Error(`invalid case ${JSON.stringify(value)}`)
  const { id } = value
  const unknown = Object.keys(value)
    .filter(key => !['id', 'header', 'rows', 'expect', 'rust', 'expandedDiverges', 'note'].includes(key))
  if (unknown.length > 0) throw new Error(`${id}: unknown keys ${unknown.join()}`)
  if (typeof value.header !== 'string' || !Array.isArray(value.rows) || !value.rows.every(row => typeof row === 'string')) {
    throw new Error(`${id}: header and rows must be JSON text`)
  }
  if (value.note !== undefined && typeof value.note !== 'string') throw new Error(`${id}: invalid note`)
  if (Object.hasOwn(value, 'expandedDiverges') && value.expandedDiverges !== true) {
    throw new Error(`${id}: expandedDiverges must be true when present`)
  }
  let limit: RunCase['limit']
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
    ...(limit === undefined ? {} : { limit }),
    expandedDiverges: value.expandedDiverges === true,
  }
}

function loadCases(): RunCase[] {
  const table: unknown = JSON.parse(readFileSync(new URL('conformance/session/v1-to-v2-run-cases.json', REPO), 'utf8'))
  if (!isObject(table) || sortedKeys(table) !== 'cases,history,oracle,schema,version'
    || table.schema !== SCHEMA || table.version !== 1 || table.oracle !== ORACLE || !Array.isArray(table.cases)
    || !Array.isArray(table.history) || !table.history.every(entry => typeof entry === 'string')) {
    throw new Error('v1-to-v2-run-cases.json does not match its version-1 schema')
  }
  const cases = table.cases.map(parseCase)
  if (new Set(cases.map(entry => entry.id)).size !== cases.length) throw new Error('case ids must be unique')
  return cases
}

type Item = { kind: 'event'; event: SessionFormatEvent } | { kind: 'run'; run: SessionFormatEventRun }

interface Decoded {
  header: SessionFormatHeader
  inheritedEventCount: number
  items: Item[]
}

/** The strict codec decode every case must pass, keeping the decoder's emitted events and runs. */
function decode(entry: RunCase): Decoded {
  const decoder = releasedV1SessionFormatCodec.createDecoder(JSON.parse(entry.header) as unknown, 'strict')
  const items: Item[] = []
  const context: SessionFormatMigrationContext = {
    emitEvent: (event) => { items.push({ kind: 'event', event }) },
    emitRun: (run) => { items.push({ kind: 'run', run }) },
  }
  for (const row of entry.rows) decoder.decodeRow(JSON.parse(row) as unknown, context)
  const inheritedEventCount = decoder.finish(context)
  return { header: decoder.header, inheritedEventCount, items }
}

function refused(at: At, error: unknown): Outcome {
  if (error instanceof SessionFormatError) return { outcome: 'refused', at, message: error.message }
  throw error
}

/** Run the stage, passing each run to `transformRun`, or its expanded events to `transformEvent`. */
function migrate(decoded: Decoded, runs: 'transformRun' | 'expanded'): Outcome {
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
  for (const [index, item] of decoded.items.entries()) {
    try {
      if (item.kind === 'event') stage.transformEvent(item.event, collector)
      else if (runs === 'transformRun') stage.transformRun(item.run, collector)
      else for (const event of item.run.expand()) stage.transformEvent(event, collector)
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
  return { outcome: 'migrated', header: targetHeader, events: collector.values, inheritedEventCount }
}

/** Object members as ordered pairs, so equality also checks member order and `Object.is` numbers. */
function ordered(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(ordered)
  if (isObject(value)) return Object.entries(value).map(([key, item]) => [key, ordered(item)])
  return value
}

const cases = loadCases()

describe('shared v1 to v2 packed-run cases', () => {
  it('pin the table size and witness every native limit', () => {
    expect(cases).toHaveLength(CASE_COUNT)
    expect(new Set(cases.flatMap(entry => entry.limit?.limit ?? []))).toEqual(new Set(LIMITS))
    expect(cases.some(entry => entry.expandedDiverges)).toBe(true)
  })

  for (const entry of cases) {
    it(entry.id, () => {
      const decoded = decode(entry)
      expect(decoded.items.some(item => item.kind === 'run'), `${entry.id} holds a packed run`).toBe(true)
      expect(ordered(migrate(decoded, 'transformRun')), entry.id).toEqual(ordered(entry.expect))
      // A fresh decode: transformRun appended to the first decode's run arrays.
      const expanded = ordered(migrate(decode(entry), 'expanded'))
      if (entry.expandedDiverges) expect(expanded, `${entry.id} expanded`).not.toEqual(ordered(entry.expect))
      else expect(expanded, `${entry.id} expanded`).toEqual(ordered(entry.expect))
    })
  }
})
