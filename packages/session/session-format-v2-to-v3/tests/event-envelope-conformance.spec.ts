/**
 * Runs the shared row-envelope cases in
 * `conformance/session/event-envelope-cases.json` through one strict released
 * v2 `decodeRow` call after `expectedSeq` contiguous priming rows. The
 * development Rust decoder in `rust/crates/bake-session` checks the same
 * table. Agreement covers that call only: envelope fields, source references,
 * the seq gap, and end-seed data. It is not V3 row admission, which runs a
 * structural check before the call and known-event envelope and payload
 * checks after it. A case's optional `v3` outcome records what the real V3
 * codec does with the same rows, so divergences stay visible. A `rust`
 * override names a native-subset limit or a class-only refusal; TypeScript
 * still asserts its own outcome. The fixture check decodes the unchanged
 * request-reconstruction log through the real V3 codec.
 */

import { createHash } from 'node:crypto'
import { readFileSync } from 'node:fs'
import { SessionFormatError, SessionFormatEventCollector } from 'bake-session-format'
import type { SessionFormatCodec, SessionFormatEvent } from 'bake-session-format'
import { describe, expect, it } from 'vitest'
import { releasedV2SessionFormatCodec, releasedV3SessionFormatCodec } from '../src/index.ts'

const REPO = new URL('../../../../', import.meta.url)
const SCHEMA = 'bake/session-format-conformance/event-envelope-cases'
const ORACLE = "releasedV2SessionFormatCodec.createDecoder(header, 'strict').decodeRow(row) from packages/session/session-format-v1-to-v2/src/codec.ts, after priming rows 0 through expectedSeq - 1"
const FIXTURE = 'conformance/runtime/request-reconstruction/tool-call-turn/session.jsonl'
const FIXTURE_SHA256 = 'a7a8222990ef9f4c4f00a051c019de86d3156ca7f6c3c0d28af4be4cc3fbe657'
/** Both harnesses pin the table size, so a dropped case fails. */
const CASE_COUNT = 92
/** Bounds the priming rows each case decodes. */
const MAX_EXPECTED_SEQ = 16
const LIMITS = [
  'time-float-lexeme',
  'seq-float-lexeme',
  'negative-zero-seq',
  'seq-diagnostic',
  'source-float-lexeme',
  'source-output-budget',
]
/** Every released v2 row message, with the row index, seq, key, and value as wildcards. */
const MESSAGE_PATTERNS = [
  /^released v2 row \d+ must be an object$/,
  /^released v2 row \d+ lacks required field (type|seq|time|data)$/,
  /^released v2 row \d+ has unexpected field .+$/,
  /^released v2 row \d+ type must be a string$/,
  /^released v2 row \d+ time must be a safe integer$/,
  /^released v2 row \d+ ignorable must be true when present$/,
  /^released v2 row \d+ seq must be a non-negative safe integer$/,
  /^sourceEventSeqs /,
  /^released v2 row \d+ has seq gap \(expected \d+, got .*\)$/,
  /^session\/end-seed \d+ data must be an object$/,
]
const ENVELOPE_KEYS = ['type', 'seq', 'time', 'ignorable', 'sourceEventSeqs', 'surfaceOp']
const PRIME = 'bake/conformance-prime'
const HEADER = { type: 'session', id: 'event-envelope', createdAt: 0, isSeeded: false, delegationDepth: 0 }

interface Envelope {
  type: string
  seq: number
  time: number
  ignorable: boolean
  sourceEventSeqs?: number[]
  surfaceOp?: unknown
}

type Outcome =
  | { outcome: 'decoded'; envelope: Envelope }
  | { outcome: 'rejected'; message: string }
  | { outcome: 'thrown'; error: 'TypeError' }

type RustOverride =
  | { outcome: 'native-subset'; limit: string }
  | { outcome: 'rejected-class'; class: 'seq-gap' }
  | { outcome: 'rejected-class'; class: 'unexpected-fields'; keys: string[] }

interface EnvelopeCase {
  id: string
  expectedSeq: number
  /** Raw JSON text of the row, kept as text so number spellings survive. */
  row: string
  ts: Outcome
  rust?: RustOverride
  v3?: Outcome
}

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

function sortedKeys(value: Record<string, unknown>): string {
  return Object.keys(value).sort().join()
}

function isCount(value: unknown): value is number {
  return Number.isSafeInteger(value) && (value as number) >= 0
}

function parseEnvelope(value: unknown, context: string): Envelope {
  if (!isObject(value) || !['type', 'seq', 'time', 'ignorable'].every(key => Object.hasOwn(value, key))
    || !Object.keys(value).every(key => ENVELOPE_KEYS.includes(key))
    || typeof value.type !== 'string' || !isCount(value.seq) || !Number.isSafeInteger(value.time)
    || Object.is(value.time, -0) || typeof value.ignorable !== 'boolean'
    || value.sourceEventSeqs !== undefined && !(Array.isArray(value.sourceEventSeqs) && value.sourceEventSeqs.every(isCount))) {
    throw new Error(`${context}: invalid envelope ${JSON.stringify(value)}`)
  }
  return value as unknown as Envelope
}

/** V3 messages are not constrained to the released v2 vocabulary. */
function parseOutcome(value: unknown, context: string, v2Messages = true): Outcome {
  if (isObject(value)) {
    const keys = sortedKeys(value)
    if (value.outcome === 'decoded' && keys === 'envelope,outcome') {
      return { outcome: 'decoded', envelope: parseEnvelope(value.envelope, context) }
    }
    if (value.outcome === 'rejected' && keys === 'message,outcome' && typeof value.message === 'string'
      && (!v2Messages || MESSAGE_PATTERNS.some(pattern => pattern.test(value.message as string)))) {
      return { outcome: 'rejected', message: value.message }
    }
    if (value.outcome === 'thrown' && keys === 'error,outcome' && value.error === 'TypeError') {
      return { outcome: 'thrown', error: 'TypeError' }
    }
  }
  throw new Error(`${context}: invalid outcome ${JSON.stringify(value)}`)
}

/** A Rust override must be consistent with the TypeScript outcome it replaces. */
function parseRust(value: unknown, entry: Omit<EnvelopeCase, 'rust' | 'v3'>): RustOverride {
  const { id, expectedSeq, ts } = entry
  if (isObject(value)) {
    const keys = sortedKeys(value)
    if (value.outcome === 'native-subset' && keys === 'limit,outcome' && LIMITS.includes(value.limit as string)) {
      return { outcome: 'native-subset', limit: value.limit as string }
    }
    const message = ts.outcome === 'rejected' ? ts.message : undefined
    const row = `released v2 row ${expectedSeq}`
    if (value.outcome === 'rejected-class' && value.class === 'seq-gap' && keys === 'class,outcome'
      && message?.startsWith(`${row} has seq gap (expected ${expectedSeq}, got `) === true) {
      return { outcome: 'rejected-class', class: 'seq-gap' }
    }
    if (value.outcome === 'rejected-class' && value.class === 'unexpected-fields' && keys === 'class,keys,outcome'
      && Array.isArray(value.keys) && value.keys.length >= 2 && value.keys.every(key => typeof key === 'string')
      && value.keys.some(key => message === `${row} has unexpected field ${key}`)) {
      return { outcome: 'rejected-class', class: 'unexpected-fields', keys: value.keys as string[] }
    }
  }
  throw new Error(`${id}: rust override ${JSON.stringify(value)} does not match its TypeScript outcome`)
}

function parseCase(value: unknown): EnvelopeCase {
  if (!isObject(value) || typeof value.id !== 'string') throw new Error(`invalid case ${JSON.stringify(value)}`)
  const { id } = value
  const unknown = Object.keys(value).filter(key => !['id', 'expectedSeq', 'row', 'budget', 'ts', 'rust', 'v3'].includes(key))
  if (unknown.length > 0) throw new Error(`${id}: unknown keys ${unknown.join()}`)
  if (!isCount(value.expectedSeq) || value.expectedSeq > MAX_EXPECTED_SEQ) {
    throw new Error(`${id}: expectedSeq must be an integer from 0 to ${MAX_EXPECTED_SEQ}`)
  }
  if (value.budget !== undefined && !isCount(value.budget)) throw new Error(`${id}: budget must be a non-negative integer`)
  if (typeof value.row !== 'string') throw new Error(`${id}: row must be JSON text`)
  JSON.parse(value.row)
  const ts = parseOutcome(value.ts, `${id} ts`)
  if (ts.outcome === 'decoded' && !Object.is(ts.envelope.seq, value.expectedSeq) && !Object.is(ts.envelope.seq, -0)) {
    throw new Error(`${id}: a decoded seq is the expected seq`)
  }
  const base = { id, expectedSeq: value.expectedSeq, row: value.row, ts }
  const rust = Object.hasOwn(value, 'rust') ? parseRust(value.rust, base) : undefined
  if (ts.outcome === 'thrown' && rust?.outcome !== 'native-subset') throw new Error(`${id}: Rust cannot claim a thrown TypeError`)
  const v3 = Object.hasOwn(value, 'v3') ? parseOutcome(value.v3, `${id} v3`, false) : undefined
  return { ...base, ...(rust === undefined ? {} : { rust }), ...(v3 === undefined ? {} : { v3 }) }
}

function loadTable() {
  const table: unknown = JSON.parse(readFileSync(new URL('conformance/session/event-envelope-cases.json', REPO), 'utf8'))
  if (!isObject(table) || sortedKeys(table) !== 'cases,defaultBudget,fixtureEnvelopes,oracle,schema,version'
    || table.schema !== SCHEMA || table.version !== 1 || table.oracle !== ORACLE
    || !isCount(table.defaultBudget) || !Array.isArray(table.cases)) {
    throw new Error('event-envelope-cases.json does not match its version-1 schema')
  }
  const fixture = table.fixtureEnvelopes
  if (!isObject(fixture) || sortedKeys(fixture) !== 'events,log' || fixture.log !== FIXTURE
    || !Array.isArray(fixture.events) || fixture.events.length === 0) {
    throw new Error('fixtureEnvelopes must name the request-reconstruction log and its expected envelopes')
  }
  const cases = table.cases.map(parseCase)
  if (new Set(cases.map(entry => entry.id)).size !== cases.length) throw new Error('case ids must be unique')
  const fixtureEvents = fixture.events.map((event, index) => parseEnvelope(event, `fixture event ${index}`))
  return { cases, fixtureEvents }
}

/** The envelope metadata of a decoded event, keeping an omitted field apart from JSON `null`. */
function envelopeOf(event: SessionFormatEvent): Envelope {
  return {
    type: event.type,
    seq: event.seq,
    time: event.time,
    ignorable: event.ignorable === true,
    ...(Object.hasOwn(event, 'sourceEventSeqs') ? { sourceEventSeqs: [...event.sourceEventSeqs as readonly number[]] } : {}),
    ...(Object.hasOwn(event, 'surfaceOp') ? { surfaceOp: event['surfaceOp'] } : {}),
  }
}

/** Decodes one case row after `expectedSeq` priming rows; an unlisted error fails the case. */
function decodeCase(codec: SessionFormatCodec, version: number, entry: EnvelopeCase): Outcome {
  const decoder = codec.createDecoder({ ...HEADER, version }, 'strict')
  const collector = new SessionFormatEventCollector()
  for (let seq = 0; seq < entry.expectedSeq; seq += 1) {
    decoder.decodeRow({ type: PRIME, seq, time: 0, data: {} }, collector)
  }
  const row: unknown = JSON.parse(entry.row)
  try {
    decoder.decodeRow(row, collector)
  } catch (error) {
    if (error instanceof SessionFormatError) return { outcome: 'rejected', message: error.message }
    // The gap message converts an object or array seq outside the decoder's error handling.
    if (error instanceof TypeError) return { outcome: 'thrown', error: 'TypeError' }
    throw error
  }
  expect(collector.values).toHaveLength(entry.expectedSeq + 1)
  const event = collector.values.at(-1) as SessionFormatEvent
  // The decoder retains the row's payload instead of projecting it.
  expect(isObject(row) && Object.is(event.data, row.data)).toBe(true)
  expect(sortedKeys(event)).toBe(sortedKeys(row as Record<string, unknown>))
  return { outcome: 'decoded', envelope: envelopeOf(event) }
}

const { cases, fixtureEvents } = loadTable()

describe('shared row-envelope cases', () => {
  it('pin the table size and witness every message, limit, class, and V3 divergence', () => {
    expect(cases).toHaveLength(CASE_COUNT)
    for (const pattern of MESSAGE_PATTERNS) {
      expect(cases.some(entry => entry.ts.outcome === 'rejected' && pattern.test(entry.ts.message)), String(pattern)).toBe(true)
    }
    expect(new Set(cases.flatMap(entry => entry.rust?.outcome === 'native-subset' ? [entry.rust.limit] : []))).toEqual(new Set(LIMITS))
    expect(new Set(cases.flatMap(entry => entry.rust?.outcome === 'rejected-class' ? [entry.rust.class] : [])))
      .toEqual(new Set(['seq-gap', 'unexpected-fields']))
    expect(cases.some(entry => entry.ts.outcome === 'thrown')).toBe(true)
    expect(cases.some(entry => entry.v3 !== undefined && entry.ts.outcome === 'decoded' && entry.v3.outcome === 'rejected')).toBe(true)
    expect(cases.some(entry => entry.v3 !== undefined && entry.ts.outcome === 'rejected' && entry.v3.outcome === 'rejected'
      && entry.v3.message !== entry.ts.message)).toBe(true)
    expect(cases.some(entry => entry.id === 'unknown-type-opaque-payload'
      && entry.ts.outcome === 'decoded' && entry.v3?.outcome === 'decoded')).toBe(true)
  })

  for (const entry of cases) {
    it(entry.id, () => {
      expect(decodeCase(releasedV2SessionFormatCodec, 2, entry), entry.id).toEqual(entry.ts)
      if (entry.v3 !== undefined) expect(decodeCase(releasedV3SessionFormatCodec, 3, entry), `${entry.id} v3`).toEqual(entry.v3)
    })
  }

  it('match the envelopes the V3 codec decodes from the unchanged request-reconstruction log', () => {
    const bytes = readFileSync(new URL(FIXTURE, REPO))
    expect(createHash('sha256').update(bytes).digest('hex')).toBe(FIXTURE_SHA256)
    const [headerLine, ...rows] = bytes.toString('utf8').split('\n')
    expect(rows.pop()).toBe('')
    const decoder = releasedV3SessionFormatCodec.createDecoder(JSON.parse(headerLine!), 'strict')
    const collector = new SessionFormatEventCollector()
    const parsed = rows.map(row => JSON.parse(row) as Record<string, unknown>)
    for (const row of parsed) decoder.decodeRow(row, collector)
    decoder.finish(collector)
    expect(collector.values).toHaveLength(parsed.length)
    collector.values.forEach((event, index) => expect(Object.is(event.data, parsed[index]!.data)).toBe(true))
    expect(collector.values.map(envelopeOf)).toEqual(fixtureEvents)
    expect(new Set(fixtureEvents.map(event => event.type)).size).toBe(12)
  })
})
