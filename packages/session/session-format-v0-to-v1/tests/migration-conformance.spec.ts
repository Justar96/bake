/**
 * Runs the shared cases in `conformance/session/v0-to-v1-cases.json` through
 * the released v0 codec feeding the real v0→v1 migration chain. The
 * development Rust `migrate_v0_to_v1` in `rust/crates/bake-session` checks the
 * same table over `decode_v0_v1_rows`. Each case first decodes alone, which
 * must succeed, because Rust migrates only a completed decode. It then
 * decodes its header, emits every row the decoder admits into the chain
 * stream, and finishes the decoder and the stream. A refusal is located at
 * the index of the event the chain was migrating. Agreement covers that edge
 * output only: the whole-artifact relationship checks in `relationships.ts`
 * and later edges do not run. A `rust` native-subset marker names a case Rust
 * deliberately does not decide; TypeScript still asserts its own outcome.
 */

import { readFileSync } from 'node:fs'
import path from 'node:path'
import { createSessionFormatChain, SessionFormatError, SessionFormatEventCollector } from 'bake-session-format'
import type {
  SessionFormatArtifactDecoder,
  SessionFormatEvent,
  SessionFormatEventRun,
  SessionFormatMigrationContext,
  SessionFormatMigrationStream,
  SessionFormatRecovery,
} from 'bake-session-format'
import { describe, expect, it } from 'vitest'
import {
  RELEASED_V0_EVENT_DISPOSITIONS,
  assertReleasedV1Header,
  releasedV0SessionFormatCodec,
  sessionFormatV0ToV1,
} from '../src/index.ts'

const REPO = new URL('../../../../', import.meta.url)
const SCHEMA = 'bake/session-format-conformance/v0-to-v1-cases'
const ORACLE = 'releasedV0SessionFormatCodec.createDecoder(header, recovery) feeding createSessionFormatChain({currentVersion: 1, migrations: [sessionFormatV0ToV1]}).createStream; decoder.finish, then stream.finish'
/** Both harnesses pin the table size, so a dropped case fails. */
const CASE_COUNT = 95
/**
 * Native limits: a float spelling where TypeScript reads a seq, count, or `Map` key, a non-string
 * type that TypeScript coerces, a V8 TypeError text the chain wraps, and the unported legacy goal
 * message check.
 */
const LIMITS = ['seq-float-lexeme', 'type-coercion', 'object-prototype-type', 'payload-float-lexeme', 'reference-float-lexeme', 'legacy-goal-message']

type Outcome =
  | { outcome: 'migrated'; header: unknown; events: unknown[]; inheritedEventCount: number }
  | { outcome: 'refused'; at: number; message: string }

interface MigrationCase {
  id: string
  recovery: SessionFormatRecovery
  header: string
  rows: string[]
  expect: Outcome
  limit?: { limit: string; at: number }
}

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

function sortedKeys(value: Record<string, unknown>): string {
  return Object.keys(value).sort().join()
}

function isIndex(value: unknown): value is number {
  return Number.isSafeInteger(value) && (value as number) >= 0
}

function parseOutcome(value: unknown, id: string): Outcome {
  if (isObject(value)) {
    const keys = sortedKeys(value)
    if (value.outcome === 'migrated' && keys === 'events,header,inheritedEventCount,outcome'
      && isObject(value.header) && Array.isArray(value.events) && Number.isSafeInteger(value.inheritedEventCount)) {
      return value as Outcome
    }
    if (value.outcome === 'refused' && keys === 'at,message,outcome' && isIndex(value.at) && typeof value.message === 'string') {
      return value as Outcome
    }
  }
  throw new Error(`${id}: invalid outcome ${JSON.stringify(value)}`)
}

function parseCase(value: unknown): MigrationCase {
  if (!isObject(value) || typeof value.id !== 'string') throw new Error(`invalid case ${JSON.stringify(value)}`)
  const { id } = value
  const unknown = Object.keys(value).filter(key => !['id', 'recovery', 'header', 'rows', 'expect', 'rust', 'note'].includes(key))
  if (unknown.length > 0) throw new Error(`${id}: unknown keys ${unknown.join()}`)
  if (value.recovery !== 'strict' && value.recovery !== 'recoverable') throw new Error(`${id}: invalid recovery`)
  if (typeof value.header !== 'string' || !Array.isArray(value.rows) || !value.rows.every(row => typeof row === 'string')) {
    throw new Error(`${id}: header and rows must be JSON text`)
  }
  if (value.note !== undefined && typeof value.note !== 'string') throw new Error(`${id}: invalid note`)
  let limit: MigrationCase['limit']
  if (Object.hasOwn(value, 'rust')) {
    const { rust } = value
    if (!isObject(rust) || sortedKeys(rust) !== 'at,limit' || !LIMITS.includes(rust.limit as string) || !isIndex(rust.at)) {
      throw new Error(`${id}: rust may only name a native-subset limit and its event index`)
    }
    limit = { limit: rust.limit as string, at: rust.at }
  }
  return {
    id,
    recovery: value.recovery,
    header: value.header,
    rows: value.rows as string[],
    expect: parseOutcome(value.expect, id),
    ...(limit === undefined ? {} : { limit }),
  }
}

function loadTable(): { cases: MigrationCase[]; dispositions: unknown } {
  const table: unknown = JSON.parse(readFileSync(new URL('conformance/session/v0-to-v1-cases.json', REPO), 'utf8'))
  if (!isObject(table) || sortedKeys(table) !== 'cases,dispositions,history,oracle,schema,version'
    || table.schema !== SCHEMA || table.version !== 1 || table.oracle !== ORACLE || !Array.isArray(table.cases)
    || !Array.isArray(table.history) || !table.history.every(entry => typeof entry === 'string')) {
    throw new Error('v0-to-v1-cases.json does not match its version-1 schema')
  }
  const cases = table.cases.map(parseCase)
  if (new Set(cases.map(entry => entry.id)).size !== cases.length) throw new Error('case ids must be unique')
  return { cases, dispositions: table.dispositions }
}

const chain = createSessionFormatChain({
  currentVersion: 1,
  migrations: [sessionFormatV0ToV1],
  restoreCurrentHeader(header) {
    assertReleasedV1Header(header)
    return header
  },
})

interface Observed {
  outcome: Outcome
  /** The chain wrapped an engine error rather than a SessionFormatError. */
  engineError: boolean
}

/** The codec alone must admit every row and finish; Rust migrates only such a decode. */
function decodes(header: unknown, rows: readonly unknown[], recovery: SessionFormatRecovery): boolean {
  try {
    const decoder = releasedV0SessionFormatCodec.createDecoder(header, recovery)
    const collector = new SessionFormatEventCollector()
    for (const row of rows) decoder.decodeRow(row, collector)
    decoder.finish(collector)
    return true
  } catch (error) {
    if (error instanceof SessionFormatError) return false
    throw error
  }
}

function migrate(header: unknown, rows: readonly unknown[], recovery: SessionFormatRecovery): Observed {
  const decoder: SessionFormatArtifactDecoder = releasedV0SessionFormatCodec.createDecoder(header, recovery)
  const collector = new SessionFormatEventCollector()
  const stream: SessionFormatMigrationStream = chain.createStream(decoder.header, decoder.headerInheritedEventCount, collector)
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
    if (failure === undefined || failure.error !== error || !(error instanceof SessionFormatError)) throw error
    const engineError = error.cause !== undefined && !(error.cause instanceof SessionFormatError)
    return { outcome: { outcome: 'refused', at: failure.at, message: error.message }, engineError }
  }
  const inheritedEventCount = stream.finish()
  expect(collector.values).toHaveLength(emitted)
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

const { cases, dispositions } = loadTable()

describe('shared v0 to v1 migration cases', () => {
  it('pin the table size and witness every native limit', () => {
    expect(cases).toHaveLength(CASE_COUNT)
    expect(new Set(cases.flatMap(entry => entry.limit?.limit ?? []))).toEqual(new Set(LIMITS))
    expect(cases.some(entry => entry.recovery === 'recoverable')).toBe(true)
  })

  it('share the released v0 disposition table with Rust', () => {
    const actual = Object.fromEntries(Object.entries(RELEASED_V0_EVENT_DISPOSITIONS).map(([type, disposition]) => [type, {
      required: [...disposition.required],
      optional: [...disposition.optional],
      opaque: [...disposition.opaque],
    }]))
    expect(dispositions).toEqual(actual)
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
      const header: unknown = deepFreeze(JSON.parse(entry.header))
      const rows: unknown[] = deepFreeze(entry.rows.map(row => JSON.parse(row) as unknown))
      const before = ordered([header, rows])
      expect(decodes(header, rows, entry.recovery), `${entry.id} decodes`).toBe(true)
      const actual = migrate(header, rows, entry.recovery)
      expect(ordered(actual.outcome), entry.id).toEqual(ordered(entry.expect))
      // Engine error text is pinned only where Rust names the limit.
      expect(actual.engineError, `${entry.id} engine error`).toBe(entry.limit?.limit === 'object-prototype-type')
      expect(ordered([header, rows]), `${entry.id} inputs`).toEqual(before)
    })
  }
})
