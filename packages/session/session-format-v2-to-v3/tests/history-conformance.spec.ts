/**
 * Runs the shared cases in `conformance/session/history-cases.json` through
 * the released v0 or v1 codec feeding the real migration chain from that
 * version to v3. The development Rust `migrate_released_history` in
 * `rust/crates/bake-session` checks the same table over `decode_v0_v1_items`.
 * Each case first decodes alone, which must succeed, because Rust reads only a
 * completed decode. It then decodes its header, emits every row the decoder
 * admits into the chain stream, and finishes the decoder and the stream. A
 * refusal is located at the header, at the index of the decoded event the
 * chain was migrating, or at finish. The chain streams each event through
 * every stage before the next, so that refusal is the earliest one. The
 * output is stage output: the catalog's final check of the v3 artifact is not
 * run. A `rust` native-subset marker names a case Rust deliberately does not
 * decide; TypeScript still asserts its own outcome.
 */

import { readFileSync } from 'node:fs'
import path from 'node:path'
import { createSessionFormatChain, SessionFormatError, SessionFormatEventCollector } from 'bake-session-format'
import type {
  SessionFormatArtifactDecoder,
  SessionFormatCodec,
  SessionFormatEvent,
  SessionFormatEventRun,
  SessionFormatMigrationContext,
  SessionFormatMigrationStream,
} from 'bake-session-format'
import { releasedV0SessionFormatCodec, releasedV1SessionFormatCodec, sessionFormatV0ToV1 } from 'bake-session-format-v0-to-v1'
import { sessionFormatV1ToV2 } from 'bake-session-format-v1-to-v2'
import { describe, expect, it } from 'vitest'
import { assertReleasedV3Header, sessionFormatV2ToV3 } from '../src/index.ts'

const REPO = new URL('../../../../', import.meta.url)
const SCHEMA = 'bake/session-format-conformance/history-cases'
const ORACLE = "releasedV0SessionFormatCodec.createDecoder(header, 'strict') or releasedV1SessionFormatCodec feeding createSessionFormatChain({currentVersion: 3, migrations: [sessionFormatV0ToV1, sessionFormatV1ToV2, sessionFormatV2ToV3]}).createStream; decoder.finish, then stream.finish"
/** Both harnesses pin the table size, so a dropped case fails. */
const CASE_COUNT = 60
/** Native limits: an event without `time`, and one v0→v1 limit passing through under its stage's prefix. */
const LIMITS = ['untimed-event', 'v0-to-v1/legacy-goal-message']

type At = 'header' | 'finish' | number
type Outcome =
  | { outcome: 'migrated'; header: unknown; events: unknown[]; inheritedEventCount: number }
  | { outcome: 'refused'; at: At; message: string }

interface HistoryCase {
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

function parseCase(value: unknown): HistoryCase {
  if (!isObject(value) || typeof value.id !== 'string') throw new Error(`invalid case ${JSON.stringify(value)}`)
  const { id } = value
  const unknown = Object.keys(value).filter(key => !['id', 'version', 'header', 'rows', 'expect', 'rust', 'note'].includes(key))
  if (unknown.length > 0) throw new Error(`${id}: unknown keys ${unknown.join()}`)
  if (value.version !== 0 && value.version !== 1) throw new Error(`${id}: version must be 0 or 1`)
  if (typeof value.header !== 'string' || !Array.isArray(value.rows) || !value.rows.every(row => typeof row === 'string')) {
    throw new Error(`${id}: header and rows must be JSON text`)
  }
  if (value.note !== undefined && typeof value.note !== 'string') throw new Error(`${id}: invalid note`)
  let limit: HistoryCase['limit']
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

function loadTable(): HistoryCase[] {
  const table: unknown = JSON.parse(readFileSync(new URL('conformance/session/history-cases.json', REPO), 'utf8'))
  if (!isObject(table) || sortedKeys(table) !== 'cases,history,oracle,schema,version'
    || table.schema !== SCHEMA || table.version !== 3 || table.oracle !== ORACLE || !Array.isArray(table.cases)
    || !Array.isArray(table.history) || !table.history.every(entry => typeof entry === 'string')) {
    throw new Error('history-cases.json does not match its version-3 schema')
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

function codecOf(entry: HistoryCase): SessionFormatCodec {
  return entry.version === 0 ? releasedV0SessionFormatCodec : releasedV1SessionFormatCodec
}

interface Observed {
  outcome: Outcome
  /** The chain wrapped an engine error rather than a SessionFormatError. */
  engineError: boolean
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

function refused(at: At, error: unknown): Observed {
  if (!(error instanceof SessionFormatError)) throw error
  const engineError = error.cause !== undefined && !(error.cause instanceof SessionFormatError)
  return { outcome: { outcome: 'refused', at, message: error.message }, engineError }
}

function migrate(codec: SessionFormatCodec, header: unknown, rows: readonly unknown[]): Observed {
  const decoder: SessionFormatArtifactDecoder = codec.createDecoder(header, 'strict')
  const collector = new SessionFormatEventCollector()
  let stream: SessionFormatMigrationStream
  try {
    stream = chain.createStream(decoder.header, decoder.headerInheritedEventCount, collector)
  } catch (error) {
    return refused('header', error)
  }
  let emitted = 0
  let failure: { at: number; error: unknown } | undefined
  const forward: SessionFormatMigrationContext = {
    emitEvent(event: SessionFormatEvent) {
      try {
        stream.emitEvent(event)
      } catch (error) {
        failure = { at: emitted, error }
        throw error
      }
      emitted += 1
    },
    emitRun(run: SessionFormatEventRun) {
      try {
        stream.emitRun(run)
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
    inheritedEventCount = stream.finish()
  } catch (error) {
    return refused('finish', error)
  }
  return {
    outcome: { outcome: 'migrated', header: stream.header, events: collector.values, inheritedEventCount },
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

const cases = loadTable()

describe('shared v0 and v1 history read to v3 cases', () => {
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
      expect(ordered(actual.outcome), entry.id).toEqual(ordered(entry.expect))
      // Engine error text is pinned only where Rust names a limit.
      if (actual.engineError) expect(entry.limit, `${entry.id} engine error`).toBeDefined()
      expect(ordered([header, rows]), `${entry.id} inputs`).toEqual(before)
    })
  }
})
