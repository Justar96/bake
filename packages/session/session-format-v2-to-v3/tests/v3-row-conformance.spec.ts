/**
 * Runs the shared V3 row cases in `conformance/session/v3-row-cases.json`
 * through one strict V3 codec `decodeRow` call after `expectedSeq` contiguous
 * priming rows. The development Rust decoder in `rust/crates/bake-session`
 * checks the same table. Agreement covers that codec call: raw-row admission,
 * the released v2 envelope, and the per-event checks. It is not restoration,
 * which also requires an installed vocabulary and checks relationships between
 * events. A `rust` override names a native-subset limit or a class-only
 * refusal; TypeScript still asserts its own outcome. The vocabulary lists are
 * checked against the real exports and, name by name, against the codec.
 */

import { createHash } from 'node:crypto'
import { readFileSync } from 'node:fs'
import { SessionFormatError, SessionFormatEventCollector, SessionFormatUnsupportedMigrationError } from 'bake-session-format'
import type { SessionFormatEvent } from 'bake-session-format'
import { RELEASED_V2_EVENT_DISPOSITIONS } from 'bake-session-format-v1-to-v2'
import { describe, expect, it } from 'vitest'
import { releasedV3SessionFormatCodec } from '../src/index.ts'
import { SURFACE_TYPES } from '../src/payload.ts'

const REPO = new URL('../../../../', import.meta.url)
const SCHEMA = 'bake/session-format-conformance/v3-row-cases'
const ORACLE = "releasedV3SessionFormatCodec.createDecoder(header, 'strict').decodeRow(row) from packages/session/session-format-v2-to-v3/src/codec.ts, after priming rows 0 through expectedSeq - 1"
const FIXTURE_SHA256 = 'a7a8222990ef9f4c4f00a051c019de86d3156ca7f6c3c0d28af4be4cc3fbe657'
/** Both harnesses pin the table size, so a dropped case fails. */
const CASE_COUNT = 104
/** Bounds the priming rows each case decodes. */
const MAX_EXPECTED_SEQ = 16
const LIMITS = [
  'obsolete-seq-diagnostic',
  'system-turn-float-lexeme',
  'system-step-float-lexeme',
  'start-seq-float-lexeme',
  'end-seq-float-lexeme',
  'system-payload',
  'envelope/negative-zero-seq',
  'envelope/seq-diagnostic',
  'envelope/source-output-budget',
]
const CLASSES = ['SessionFormatError', 'SessionFormatUnsupportedMigrationError'] as const
/** Every message the table may expect, with the subject, seq, key, and value as wildcards. */
const MESSAGE_PATTERNS: Array<[RegExp, (typeof CLASSES)[number]]> = [
  [/^request\/header data must be an object$/, 'SessionFormatError'],
  [/^request header must be an object$/, 'SessionFormatError'],
  [/^format v3 request\/header rejects retired header\.system$/, 'SessionFormatUnsupportedMigrationError'],
  [/^system\/message data must be an object$/, 'SessionFormatError'],
  [/^system\/message data lacks required field \w+$/, 'SessionFormatError'],
  [/^system\/message data has unexpected field .+$/, 'SessionFormatError'],
  [/^(turn|step) must be a non-negative safe integer$/, 'SessionFormatError'],
  [/^(turn|step) must be positive$/, 'SessionFormatError'],
  [/^system message must be an object$/, 'SessionFormatError'],
  [/^system message lacks required field \w+$/, 'SessionFormatError'],
  [/^system message has unexpected field .+$/, 'SessionFormatError'],
  [/^system message requires an id and system role$/, 'SessionFormatError'],
  [/^system source must be an object$/, 'SessionFormatError'],
  [/^system message requires plugin source$/, 'SessionFormatError'],
  // The frozen payload validator, reached only through a system row.
  [/^user\/message 0 (content|source)/, 'SessionFormatError'],
  [/^format v3 contains unknown event type "tool\/code-dispatch(-start)?" at seq .*$/, 'SessionFormatUnsupportedMigrationError'],
  [/^released v2 row \d+ /, 'SessionFormatError'],
  [/^sourceEventSeqs /, 'SessionFormatError'],
  [/^format v3 .+ at seq \d+ has unexpected field (sourceEventSeqs|surfaceOp)$/, 'SessionFormatError'],
  [/^format v3 .+ at seq \d+ seq must be a non-negative safe integer$/, 'SessionFormatError'],
  [/^format v3 .+ at seq \d+ requires a surfaceOp marker$/, 'SessionFormatError'],
  [/^format v3 .+ at seq \d+ surfaceOp must be an object$/, 'SessionFormatError'],
  [/^format v3 .+ at seq \d+ requires exact replace fields op\/startSeq\/endSeq$/, 'SessionFormatError'],
  [/^format v3 .+ at seq \d+ surfaceOp (startSeq|endSeq) must be a non-negative safe integer$/, 'SessionFormatError'],
  [/^format v3 .+ at seq \d+ replacement endpoints must reference earlier events$/, 'SessionFormatError'],
  [/^format v3 assistant\/message at seq \d+ embeds its stream and cannot carry sourceEventSeqs$/, 'SessionFormatError'],
  [/^format v3 .+ at seq \d+ sourceEventSeqs must be a non-empty array$/, 'SessionFormatError'],
  [/^format v3 request\/header at seq \d+ empty optional header fields must be omitted$/, 'SessionFormatError'],
  [/^format v3 tool\/result at seq \d+ data must be an object$/, 'SessionFormatError'],
  [/^format v3 tool\/result at seq \d+ message must be an object$/, 'SessionFormatError'],
  [/^format v3 tool\/result at seq \d+ carries error metadata for a non-error tool result$/, 'SessionFormatError'],
]
const VOCABULARY_LISTS = ['surfaceTypes', 'dispositionTypes', 'nativeTypes', 'obsoleteTypes', 'objectPrototypeNames', 'opaqueTypes'] as const
const ENVELOPE_KEYS = ['type', 'seq', 'time', 'ignorable', 'sourceEventSeqs', 'surfaceOp']
const PRIME = 'bake/conformance-prime'
const HEADER = { type: 'session', version: 3, id: 'v3-row', createdAt: 0, isSeeded: false, delegationDepth: 0 }
/** Valid system data, for the vocabulary witness row of `system/message`. */
const SYSTEM_DATA = {
  turn: 1,
  step: 1,
  message: { id: 's', role: 'system', source: { kind: 'plugin', plugin: 'p' }, content: [] },
}

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
  | { outcome: 'rejected'; class: (typeof CLASSES)[number]; message: string }
  | { outcome: 'rejected'; class: 'TypeError' }

type RustOverride =
  | { outcome: 'native-subset'; limit: string }
  | { outcome: 'rejected-class'; class: 'SessionFormatError'; unexpectedKeys?: string[] }

interface RowCase {
  id: string
  expectedSeq: number
  /** Raw JSON text of the row, kept as text so number spellings survive. */
  row: string
  budget?: number
  ts: Outcome
  rust?: RustOverride
}

interface Mutant {
  id: string
  seq: number
  pointer: string
  value: unknown
  ts: Outcome
  rust?: RustOverride
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
    || typeof value.ignorable !== 'boolean'
    || value.sourceEventSeqs !== undefined && !(Array.isArray(value.sourceEventSeqs) && value.sourceEventSeqs.every(isCount))) {
    throw new Error(`${context}: invalid envelope ${JSON.stringify(value)}`)
  }
  return value as unknown as Envelope
}

function parseOutcome(value: unknown, context: string): Outcome {
  if (isObject(value)) {
    const keys = sortedKeys(value)
    if (value.outcome === 'decoded' && keys === 'envelope,outcome') {
      return { outcome: 'decoded', envelope: parseEnvelope(value.envelope, context) }
    }
    if (value.outcome === 'rejected' && keys === 'class,outcome' && value.class === 'TypeError') {
      return { outcome: 'rejected', class: 'TypeError' }
    }
    const errorClass = CLASSES.find(name => name === value.class)
    const message = value.message
    if (value.outcome === 'rejected' && keys === 'class,message,outcome' && errorClass !== undefined && typeof message === 'string'
      && MESSAGE_PATTERNS.some(([pattern, owner]) => pattern.test(message) && owner === errorClass)) {
      return { outcome: 'rejected', class: errorClass, message }
    }
  }
  throw new Error(`${context}: invalid outcome ${JSON.stringify(value)}`)
}

/** A Rust override must be consistent with the TypeScript outcome it replaces. */
function parseRust(value: unknown, context: string, ts: Outcome, eventType: unknown): RustOverride {
  if (isObject(value)) {
    const keys = sortedKeys(value)
    if (value.outcome === 'native-subset' && keys === 'limit,outcome' && LIMITS.includes(value.limit as string)
      && (value.limit !== 'system-payload' || eventType === 'system/message')) {
      return { outcome: 'native-subset', limit: value.limit as string }
    }
    const message = ts.outcome === 'rejected' && ts.class === 'SessionFormatError' ? ts.message : undefined
    if (value.outcome === 'rejected-class' && value.class === 'SessionFormatError' && message !== undefined) {
      const unexpected = value.unexpectedKeys
      if (keys === 'class,outcome,unexpectedKeys' && Array.isArray(unexpected) && unexpected.length >= 2
        && unexpected.every(key => typeof key === 'string') && unexpected.some(key => message.endsWith(` has unexpected field ${key}`))) {
        return { outcome: 'rejected-class', class: 'SessionFormatError', unexpectedKeys: unexpected as string[] }
      }
      if (keys === 'class,outcome' && /^released v2 row (\d+) has seq gap \(expected \1, got /.test(message)) {
        return { outcome: 'rejected-class', class: 'SessionFormatError' }
      }
    }
  }
  throw new Error(`${context}: rust override ${JSON.stringify(value)} does not match its TypeScript outcome`)
}

function parseCase(value: unknown): RowCase {
  if (!isObject(value) || typeof value.id !== 'string') throw new Error(`invalid case ${JSON.stringify(value)}`)
  const { id } = value
  const unknown = Object.keys(value).filter(key => !['id', 'expectedSeq', 'row', 'budget', 'ts', 'rust'].includes(key))
  if (unknown.length > 0) throw new Error(`${id}: unknown keys ${unknown.join()}`)
  if (!isCount(value.expectedSeq) || value.expectedSeq > MAX_EXPECTED_SEQ) {
    throw new Error(`${id}: expectedSeq must be an integer from 0 to ${MAX_EXPECTED_SEQ}`)
  }
  if (value.budget !== undefined && !isCount(value.budget)) throw new Error(`${id}: budget must be a non-negative integer`)
  if (typeof value.row !== 'string') throw new Error(`${id}: row must be JSON text`)
  const row: unknown = JSON.parse(value.row)
  const ts = parseOutcome(value.ts, `${id} ts`)
  if (ts.outcome === 'decoded' && ts.envelope.seq !== value.expectedSeq) throw new Error(`${id}: a decoded seq is the expected seq`)
  const rust = Object.hasOwn(value, 'rust') ? parseRust(value.rust, id, ts, isObject(row) ? row.type : undefined) : undefined
  if (ts.outcome === 'rejected' && ts.class === 'TypeError' && rust?.outcome !== 'native-subset') {
    throw new Error(`${id}: Rust cannot claim a TypeError`)
  }
  return {
    id,
    expectedSeq: value.expectedSeq,
    row: value.row,
    ...(value.budget === undefined ? {} : { budget: value.budget }),
    ts,
    ...(rust === undefined ? {} : { rust }),
  }
}

function stringList(value: unknown, context: string): string[] {
  if (!Array.isArray(value) || !value.every(name => typeof name === 'string')) throw new Error(`${context} must be a string array`)
  return value as string[]
}

function loadTable() {
  const table: unknown = JSON.parse(readFileSync(new URL('conformance/session/v3-row-cases.json', REPO), 'utf8'))
  if (!isObject(table) || sortedKeys(table) !== 'cases,defaultBudget,fixture,oracle,schema,version,vocabulary'
    || table.schema !== SCHEMA || table.version !== 1 || table.oracle !== ORACLE
    || !isCount(table.defaultBudget) || !Array.isArray(table.cases)) {
    throw new Error('v3-row-cases.json does not match its version-1 schema')
  }
  const vocabulary = table.vocabulary
  if (!isObject(vocabulary) || sortedKeys(vocabulary) !== [...VOCABULARY_LISTS].sort().join()) {
    throw new Error(`vocabulary must hold exactly ${VOCABULARY_LISTS.join()}`)
  }
  const lists = Object.fromEntries(VOCABULARY_LISTS.map(list => [list, stringList(vocabulary[list], list)])) as
    Record<(typeof VOCABULARY_LISTS)[number], string[]>
  const fixture = table.fixture
  if (!isObject(fixture) || sortedKeys(fixture) !== 'events,log,mutants,types' || typeof fixture.log !== 'string'
    || !isCount(fixture.events) || !isCount(fixture.types) || !Array.isArray(fixture.mutants)) {
    throw new Error('fixture must name the request-reconstruction log, its counts, and its mutants')
  }
  const mutants = fixture.mutants.map((value): Mutant => {
    if (!isObject(value) || typeof value.id !== 'string' || !isCount(value.seq) || typeof value.pointer !== 'string'
      || !value.pointer.startsWith('/') || !Object.hasOwn(value, 'value')
      || !Object.keys(value).every(key => ['id', 'seq', 'pointer', 'value', 'ts', 'rust'].includes(key))) {
      throw new Error(`invalid mutant ${JSON.stringify(value)}`)
    }
    const ts = parseOutcome(value.ts, `${value.id} ts`)
    const rust = Object.hasOwn(value, 'rust') ? parseRust(value.rust, value.id, ts, 'system/message') : undefined
    return { id: value.id, seq: value.seq, pointer: value.pointer, value: value.value, ts, ...(rust === undefined ? {} : { rust }) }
  })
  const cases = table.cases.map(parseCase)
  if (new Set(cases.map(entry => entry.id)).size !== cases.length) throw new Error('case ids must be unique')
  return {
    cases,
    lists,
    fixture: { log: fixture.log, events: fixture.events, types: fixture.types, mutants },
  }
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

/** Decodes `row` after `primes`; an error of an unlisted class fails the case. */
function decodeAfter(header: unknown, primes: readonly unknown[], row: unknown): Outcome {
  const decoder = releasedV3SessionFormatCodec.createDecoder(header, 'strict')
  const collector = new SessionFormatEventCollector()
  for (const prime of primes) decoder.decodeRow(prime, collector)
  try {
    decoder.decodeRow(row, collector)
  } catch (error) {
    // Classes are matched exactly: the unsupported error subclasses SessionFormatError.
    const constructor = (error as object).constructor
    if (constructor === SessionFormatUnsupportedMigrationError || constructor === SessionFormatError) {
      return { outcome: 'rejected', class: constructor.name as (typeof CLASSES)[number], message: (error as Error).message }
    }
    if (constructor === TypeError) return { outcome: 'rejected', class: 'TypeError' }
    throw error
  }
  expect(collector.values).toHaveLength(primes.length + 1)
  const event = collector.values.at(-1) as SessionFormatEvent
  // The codec retains the row's payload instead of projecting it.
  expect(isObject(row) && Object.is(event.data, row.data)).toBe(true)
  expect(sortedKeys(event)).toBe(sortedKeys(row as Record<string, unknown>))
  return { outcome: 'decoded', envelope: envelopeOf(event) }
}

function primes(count: number): unknown[] {
  return Array.from({ length: count }, (_, seq) => ({ type: PRIME, seq, time: 0, data: {} }))
}

function setPointer(root: unknown, pointer: string, value: unknown): void {
  const path = pointer.slice(1).split('/')
  const last = path.pop() as string
  const parent = path.reduce<unknown>((node, key) => (node as Record<string, unknown>)[key], root)
  if (!isObject(parent)) throw new Error(`${pointer} does not name an object member`)
  parent[last] = value
}

const { cases, lists, fixture } = loadTable()

describe('shared V3 row cases', () => {
  it('pin the table size and witness every message, class, limit, and class-only refusal', () => {
    expect(cases).toHaveLength(CASE_COUNT)
    for (const [pattern] of MESSAGE_PATTERNS) {
      expect(cases.some(entry => entry.ts.outcome === 'rejected' && entry.ts.class !== 'TypeError'
        && pattern.test(entry.ts.message)), String(pattern)).toBe(true)
    }
    expect(new Set(cases.map(entry => entry.ts.outcome === 'rejected' ? entry.ts.class : 'decoded')))
      .toEqual(new Set(['decoded', 'TypeError', ...CLASSES]))
    expect(new Set(cases.flatMap(entry => entry.rust?.outcome === 'native-subset' ? [entry.rust.limit] : []))).toEqual(new Set(LIMITS))
    const classOnly = cases.flatMap(entry => entry.rust?.outcome === 'rejected-class' ? [entry.rust] : [])
    expect(classOnly.some(rust => rust.unexpectedKeys !== undefined)).toBe(true)
    expect(classOnly.some(rust => rust.unexpectedKeys === undefined)).toBe(true)
    // serde_json reads this seq as -0, which JavaScript renders as "0"; Rust must not claim either text.
    expect(cases.some(entry => entry.id === 'obsolete-underflow-seq' && entry.ts.outcome === 'rejected'
      && entry.ts.class !== 'TypeError' && entry.ts.message.endsWith(' at seq -5e-324')
      && entry.rust?.outcome === 'native-subset' && entry.rust.limit === 'obsolete-seq-diagnostic')).toBe(true)
    // The closed system subset must not overclaim: frozen acceptance and rejection both stay limits.
    expect(cases.some(entry => entry.rust?.outcome === 'native-subset' && entry.rust.limit === 'system-payload'
      && entry.ts.outcome === 'decoded')).toBe(true)
    expect(cases.some(entry => entry.rust?.outcome === 'native-subset' && entry.rust.limit === 'system-payload'
      && entry.ts.outcome === 'rejected')).toBe(true)
  })

  it('list the real codec vocabulary', () => {
    expect(lists.dispositionTypes).toEqual(Object.keys(RELEASED_V2_EVENT_DISPOSITIONS))
    expect(lists.surfaceTypes).toEqual([...SURFACE_TYPES])
    // Engines order Object.prototype's names differently.
    expect(new Set(lists.objectPrototypeNames)).toEqual(new Set(Object.getOwnPropertyNames(Object.prototype)))
    expect(lists.objectPrototypeNames).toHaveLength(Object.getOwnPropertyNames(Object.prototype).length)
    expect(lists.obsoleteTypes.every(name => lists.dispositionTypes.includes(name))).toBe(true)
    const named = [...lists.surfaceTypes, ...lists.dispositionTypes, ...lists.objectPrototypeNames]
    for (const name of [...lists.nativeTypes, ...lists.opaqueTypes]) expect(named, name).not.toContain(name)
    for (const name of lists.opaqueTypes) expect(lists.nativeTypes, name).not.toContain(name)
  })

  it('classify every listed type as the codec does', () => {
    const names = new Set(VOCABULARY_LISTS.flatMap(list => lists[list]))
    for (const name of names) {
      const row = { type: name, seq: 0, time: 0, data: name === 'system/message' ? SYSTEM_DATA : { header: {} }, ignorable: true, surfaceOp: 1 }
      const subject = `format v3 ${name} at seq 0`
      const expected: Outcome = lists.surfaceTypes.includes(name)
        ? { outcome: 'rejected', class: 'SessionFormatError', message: `${subject} surfaceOp must be an object` }
        : lists.obsoleteTypes.includes(name) || lists.opaqueTypes.includes(name)
          ? { outcome: 'decoded', envelope: { type: name, seq: 0, time: 0, ignorable: true, surfaceOp: 1 } }
          : { outcome: 'rejected', class: 'SessionFormatError', message: `${subject} has unexpected field surfaceOp` }
      expect(decodeAfter(HEADER, [], row), name).toEqual(expected)
    }
  })

  for (const entry of cases) {
    it(entry.id, () => {
      expect(decodeAfter(HEADER, primes(entry.expectedSeq), JSON.parse(entry.row)), entry.id).toEqual(entry.ts)
    })
  }

  it('decode every row of the unchanged request-reconstruction log, and its mutants', () => {
    const bytes = readFileSync(new URL(fixture.log, REPO))
    expect(createHash('sha256').update(bytes).digest('hex')).toBe(FIXTURE_SHA256)
    const [headerLine, ...lines] = bytes.toString('utf8').split('\n')
    expect(lines.pop()).toBe('')
    const header: unknown = JSON.parse(headerLine!)
    const rows = lines.map(line => JSON.parse(line) as Record<string, unknown>)
    const decoder = releasedV3SessionFormatCodec.createDecoder(header, 'strict')
    const collector = new SessionFormatEventCollector()
    for (const row of rows) decoder.decodeRow(row, collector)
    expect(decoder.finish(collector)).toBe(0)
    expect(collector.values).toHaveLength(fixture.events)
    collector.values.forEach((event, index) => expect(Object.is(event.data, rows[index]!.data)).toBe(true))
    expect(new Set(collector.values.map(event => event.type)).size).toBe(fixture.types)
    for (const mutant of fixture.mutants) {
      const row = structuredClone(rows[mutant.seq])
      setPointer(row, mutant.pointer, mutant.value)
      expect(decodeAfter(header, rows.slice(0, mutant.seq), row), mutant.id).toEqual(mutant.ts)
    }
  })
})
