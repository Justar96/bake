/**
 * Runs the shared `sourceEventSeqs` field cases in
 * `conformance/session/source-event-seqs-cases.json` through the released v2
 * codec's row decoder, which owns the field's range decoding for the current
 * format. The development Rust decoder in `rust/crates/bake-session` checks the
 * same table. Each case primes rows 0 through `seq - 1`, then decodes one row
 * that carries the field, so agreement covers source-reference decoding only,
 * not V3 event, payload, or replay admission. A `rust` native-subset override
 * names a case Rust deliberately does not decide; TypeScript still asserts its
 * own outcome. The fixture check decodes the unchanged request-reconstruction
 * log through the real V3 codec and compares its decoded references.
 */

import { readFileSync } from 'node:fs'
import { SessionFormatError, SessionFormatEventCollector } from 'bake-session-format'
import type { SessionFormatEvent } from 'bake-session-format'
import { describe, expect, it } from 'vitest'
import { releasedV2SessionFormatCodec, releasedV3SessionFormatCodec } from '../src/index.ts'

const REPO = new URL('../../../../', import.meta.url)
const SCHEMA = 'bake/session-format-conformance/source-event-seqs-cases'
const ORACLE = "releasedV2SessionFormatCodec.createDecoder(header, 'strict').decodeRow(row) from packages/session/session-format-v1-to-v2/src/codec.ts, after rows 0 through seq - 1"
const FIXTURE = 'conformance/runtime/request-reconstruction/tool-call-turn/session.jsonl'
/** Both harnesses pin the table size, so a dropped case fails. */
const CASE_COUNT = 78
/** Bounds the priming rows each case decodes. */
const MAX_SEQ = 64
const MESSAGES = [
  'sourceEventSeqs must be an array',
  'sourceEventSeqs member must be a non-negative safe integer',
  'sourceEventSeqs range must be a [start, end] pair',
  'sourceEventSeqs range start must be a non-negative safe integer',
  'sourceEventSeqs range end must be a non-negative safe integer',
  'sourceEventSeqs range exceeds its event seq',
  'sourceEventSeqs ranges must contain unique earlier seqs',
  'sourceEventSeqs ranges must be strictly increasing',
]
const LIMITS = ['float-lexeme', 'output-budget']
const V2_HEADER = { type: 'session', version: 2, id: 'source-event-seqs', createdAt: 0, isSeeded: false, delegationDepth: 0 }

type Outcome =
  | { outcome: 'absent' }
  | { outcome: 'decoded'; seqs: number[] }
  | { outcome: 'rejected'; message: string }

interface FieldCase {
  id: string
  seq: number
  /** Raw JSON text of the field, kept as text so number spellings survive. */
  field?: string
  ts: Outcome
  limit?: string
}

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

function sortedKeys(value: Record<string, unknown>): string {
  return Object.keys(value).sort().join()
}

function parseOutcome(value: unknown, id: string): Outcome {
  if (isObject(value)) {
    const keys = sortedKeys(value)
    if (value.outcome === 'absent' && keys === 'outcome') return { outcome: 'absent' }
    if (value.outcome === 'decoded' && keys === 'outcome,seqs' && Array.isArray(value.seqs)
      && value.seqs.every(seq => Number.isSafeInteger(seq) && seq >= 0)) {
      return { outcome: 'decoded', seqs: value.seqs as number[] }
    }
    if (value.outcome === 'rejected' && keys === 'message,outcome' && MESSAGES.includes(value.message as string)) {
      return { outcome: 'rejected', message: value.message as string }
    }
  }
  throw new Error(`${id}: invalid ts outcome ${JSON.stringify(value)}`)
}

function parseCase(value: unknown): FieldCase {
  if (!isObject(value) || typeof value.id !== 'string') throw new Error(`invalid case ${JSON.stringify(value)}`)
  const { id } = value
  const unknown = Object.keys(value).filter(key => !['id', 'seq', 'field', 'budget', 'ts', 'rust'].includes(key))
  if (unknown.length > 0) throw new Error(`${id}: unknown keys ${unknown.join()}`)
  if (!Number.isSafeInteger(value.seq) || (value.seq as number) < 0 || (value.seq as number) > MAX_SEQ) {
    throw new Error(`${id}: seq must be an integer from 0 to ${MAX_SEQ}`)
  }
  if (value.budget !== undefined && (!Number.isSafeInteger(value.budget) || (value.budget as number) < 0)) {
    throw new Error(`${id}: budget must be a non-negative integer`)
  }
  const ts = parseOutcome(value.ts, id)
  if (value.field !== undefined) {
    if (typeof value.field !== 'string') throw new Error(`${id}: field must be JSON text`)
    JSON.parse(value.field)
  }
  if ((value.field === undefined) !== (ts.outcome === 'absent')) {
    throw new Error(`${id}: field is omitted exactly when the outcome is absent`)
  }
  let limit: string | undefined
  if (Object.hasOwn(value, 'rust')) {
    const { rust } = value
    if (!isObject(rust) || sortedKeys(rust) !== 'limit,outcome' || rust.outcome !== 'native-subset'
      || !LIMITS.includes(rust.limit as string)) {
      throw new Error(`${id}: rust may only name a native-subset limit`)
    }
    limit = rust.limit as string
  }
  return {
    id,
    seq: value.seq as number,
    ...(value.field === undefined ? {} : { field: value.field }),
    ts,
    ...(limit === undefined ? {} : { limit }),
  }
}

function loadTable() {
  const table: unknown = JSON.parse(readFileSync(new URL('conformance/session/source-event-seqs-cases.json', REPO), 'utf8'))
  if (!isObject(table) || sortedKeys(table) !== 'cases,defaultBudget,fixtureReferences,oracle,schema,version'
    || table.schema !== SCHEMA || table.version !== 1 || table.oracle !== ORACLE
    || !Number.isSafeInteger(table.defaultBudget) || !Array.isArray(table.cases)) {
    throw new Error('source-event-seqs-cases.json does not match its version-1 schema')
  }
  const references = table.fixtureReferences
  if (!isObject(references) || sortedKeys(references) !== 'events,log' || references.log !== FIXTURE
    || !Array.isArray(references.events) || references.events.length === 0) {
    throw new Error('fixtureReferences must name the request-reconstruction log and its expected references')
  }
  const cases = table.cases.map(parseCase)
  if (new Set(cases.map(entry => entry.id)).size !== cases.length) throw new Error('case ids must be unique')
  return { cases, fixtureEvents: references.events as unknown[] }
}

/** Decodes one case row after `seq` priming rows; an unlisted error fails the case. */
function decodeField(entry: FieldCase): Outcome {
  const decoder = releasedV2SessionFormatCodec.createDecoder(V2_HEADER, 'strict')
  const collector = new SessionFormatEventCollector()
  for (let seq = 0; seq < entry.seq; seq += 1) {
    decoder.decodeRow({ type: 'prime', seq, time: 0, data: null }, collector)
  }
  const field = entry.field === undefined ? '' : `,"sourceEventSeqs":${entry.field}`
  const row: unknown = JSON.parse(`{"type":"case","seq":${entry.seq},"time":0,"data":null${field}}`)
  try {
    decoder.decodeRow(row, collector)
  } catch (error) {
    if (error instanceof SessionFormatError && MESSAGES.includes(error.message)) {
      return { outcome: 'rejected', message: error.message }
    }
    throw error
  }
  const event = collector.values.at(-1) as SessionFormatEvent
  expect(collector.values).toHaveLength(entry.seq + 1)
  expect(event.seq).toBe(entry.seq)
  if (!Object.hasOwn(event, 'sourceEventSeqs')) return { outcome: 'absent' }
  return { outcome: 'decoded', seqs: [...event.sourceEventSeqs as readonly number[]] }
}

const { cases, fixtureEvents } = loadTable()

describe('shared sourceEventSeqs field cases', () => {
  it('pin the table size and witness every message and native limit', () => {
    expect(cases).toHaveLength(CASE_COUNT)
    expect(new Set(cases.flatMap(entry => entry.ts.outcome === 'rejected' ? [entry.ts.message] : []))).toEqual(new Set(MESSAGES))
    expect(new Set(cases.flatMap(entry => entry.limit ?? []))).toEqual(new Set(LIMITS))
  })

  for (const entry of cases) {
    it(entry.id, () => {
      expect(decodeField(entry), entry.id).toEqual(entry.ts)
    })
  }

  it('match the references the V3 codec decodes from the request-reconstruction log', () => {
    const [headerLine, ...rows] = readFileSync(new URL(FIXTURE, REPO), 'utf8').split('\n')
    expect(rows.pop()).toBe('')
    const decoder = releasedV3SessionFormatCodec.createDecoder(JSON.parse(headerLine!), 'strict')
    const collector = new SessionFormatEventCollector()
    for (const row of rows) decoder.decodeRow(JSON.parse(row), collector)
    decoder.finish(collector)
    expect(collector.values).toHaveLength(rows.length)
    const references = collector.values
      .filter(event => event.sourceEventSeqs !== undefined)
      .map(event => ({ seq: event.seq, type: event.type, sourceEventSeqs: [...event.sourceEventSeqs as readonly number[]] }))
    expect(references).toEqual(fixtureEvents)
  })
})
