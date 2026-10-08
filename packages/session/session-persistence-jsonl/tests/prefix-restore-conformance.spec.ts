/**
 * Runs the shared cases in `conformance/session/prefix-restore-cases.json`
 * through `restorePlainLog`, the composition of the production read path
 * that the plain restore cases also use: `scanLog`, `validateStoredEvents`,
 * `interruptedTurnClosers`, then `Session.fromRestore` with the catalog's
 * message projections. The table holds one case per row prefix of each
 * runtime capture, from the header alone to the whole log, so every point a
 * writer could stop at between two committed rows restores to a stated
 * state. The spec also tears each following row after 1 byte, half its
 * length, and all but its last byte, and requires that log to restore
 * exactly as the prefix does, with the prefix's committed bytes. The
 * development Rust restoration in `rust/crates/bake-session` checks the
 * same table. Restored messages are also compared as `JSON.stringify` text,
 * because `toStrictEqual` ignores member order. The spec reads only the
 * table and the captures.
 */

import { createHash } from 'node:crypto'
import { readFileSync } from 'node:fs'
import { describe, expect, it } from 'vitest'
import { Session, SessionId, SessionLogOffset, interruptedTurnClosers } from 'bake-session'
import { currentSessionMessageProjections } from 'bake-session-format-catalog/message-projections'
import { validateStoredEvents } from 'bake-session-persistence'
import { scanLog } from '../src/format.ts'

const REPO = new URL('../../../../', import.meta.url)
const SCHEMA = 'bake/session-conformance/prefix-restore-cases'
const ORACLE = 'restorePlainLog(prefix) in packages/session/session-persistence-jsonl/tests/prefix-restore-conformance.spec.ts: scanLog, validateStoredEvents, interruptedTurnClosers, then Session.fromRestore(..., "detached", currentSessionMessageProjections)'
/** Each capture, its SHA-256, and its committed row count after the header. */
const LOGS: Record<string, { path: string; sha256: string; rows: number }> = {
  'tool-call-turn': {
    path: 'conformance/runtime/request-reconstruction/tool-call-turn/session.jsonl',
    sha256: 'a7a8222990ef9f4c4f00a051c019de86d3156ca7f6c3c0d28af4be4cc3fbe657',
    rows: 16,
  },
  'dynamic-tools': {
    path: 'conformance/runtime/request-reconstruction/dynamic-tools/session.jsonl',
    sha256: '43852e686ea6ef5f599065a7ead57f82d27f8b20e9e936e81b0b0596636e61e2',
    rows: 38,
  },
  'retry-attempt': {
    path: 'conformance/runtime/request-reconstruction/retry-attempt/session.jsonl',
    sha256: 'cd79f036ffd20337af3393ab1dcfb57f62aa7434129a6ec3ddea9398c9a7a2db',
    rows: 14,
  },
}
/** Both harnesses pin the table size, so a dropped case fails. */
const CASE_COUNT = 71
/** Torn variants per prefix with a following row: after 1 byte, half, and all but the last byte. */
const TORN_COUNT = 3 * (16 + 38 + 14)
const EXPECTED_KEYS = [
  'closers', 'endSeedAppended', 'messages', 'requestContext', 'requestHeader', 'storedEventCount', 'toolHistory',
]

type Json = null | boolean | number | string | Json[] | { [key: string]: Json }

interface Expected {
  storedEventCount: number
  closers: Json[]
  endSeedAppended: boolean
  messages: Json[]
  requestHeader: Json
  toolHistory: Json
  requestContext: Json
}

interface Capture {
  header: string
  rows: string[]
  expectedHeader: Json
  inheritedEventCount: number
}

interface PrefixCase {
  id: string
  log: string
  /** How many committed rows follow the header. */
  rows: number
  ts: Expected
}

/** What `restorePlainLog` returns. */
interface RestoreResult {
  header: unknown
  inheritedEventCount: number
  committedBytes: number
  storedEventCount: number
  closers: unknown[]
  endSeedAppended: boolean
  messages: unknown[]
  requestHeader: unknown
  toolHistory: unknown
  requestContext: unknown
}

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

function sortedKeys(value: object): string {
  return Object.keys(value).sort().join()
}

function isCount(value: unknown): value is number {
  return Number.isSafeInteger(value) && (value as number) >= 0 && !Object.is(value, -0)
}

/**
 * Restore a plain current-format log the way production reads one, as the
 * plain restore cases' helper of the same name does.
 * @param log - one plain log, possibly with a torn tail.
 * @returns the restored state and its four projections.
 */
function restorePlainLog(log: Buffer): RestoreResult {
  const { meta, inheritedEventCount, events, committedBytes } = scanLog(log)
  validateStoredEvents(meta, events)
  const closers = interruptedTurnClosers(events)
  const session = Session.fromRestore(SessionId(meta.id), [...events, ...closers], meta,
    SessionLogOffset(inheritedEventCount), 'detached', currentSessionMessageProjections)
  return {
    header: meta,
    inheritedEventCount,
    committedBytes,
    storedEventCount: events.length,
    closers,
    endSeedAppended: session.seq > events.length + closers.length,
    messages: session.deriveMessages(),
    requestHeader: session.requestHeader() ?? null,
    toolHistory: session.toolHistory(),
    requestContext: session.requestContext() ?? null,
  }
}

function readCapture(name: string, entry: unknown): Capture {
  const source = LOGS[name]
  if (source === undefined || !isObject(entry) || sortedKeys(entry) !== 'header,inheritedEventCount,path'
    || entry.path !== source.path || !isObject(entry.header) || !isCount(entry.inheritedEventCount)) {
    throw new Error(`prefix-restore-cases.json: invalid log ${name}`)
  }
  const bytes = readFileSync(new URL(source.path, REPO))
  if (createHash('sha256').update(bytes).digest('hex') !== source.sha256) throw new Error(`${source.path} changed`)
  const text = bytes.toString('utf8')
  if (!text.endsWith('\n')) throw new Error(`${source.path} lacks its final LF`)
  const [header, ...rows] = text.slice(0, -1).split('\n')
  if (header === undefined || rows.length !== source.rows) throw new Error(`${source.path} changed its row count`)
  return { header, rows, expectedHeader: entry.header as Json, inheritedEventCount: entry.inheritedEventCount }
}

function parseExpected(value: unknown, rows: number, id: string): Expected {
  if (isObject(value) && sortedKeys(value) === EXPECTED_KEYS.join() && value.storedEventCount === rows
    && Array.isArray(value.closers) && typeof value.endSeedAppended === 'boolean' && Array.isArray(value.messages)) {
    return value as unknown as Expected
  }
  throw new Error(`${id}: invalid expectation ${JSON.stringify(value)}`)
}

function loadTable(): { captures: Map<string, Capture>; cases: PrefixCase[] } {
  const table: unknown = JSON.parse(readFileSync(new URL('conformance/session/prefix-restore-cases.json', REPO), 'utf8'))
  if (!isObject(table) || sortedKeys(table) !== 'cases,history,logs,oracle,schema,version' || table.schema !== SCHEMA
    || table.version !== 1 || table.oracle !== ORACLE || !Array.isArray(table.cases)
    || !Array.isArray(table.history) || !table.history.every(line => typeof line === 'string')
    || !isObject(table.logs) || sortedKeys(table.logs) !== Object.keys(LOGS).sort().join()) {
    throw new Error('prefix-restore-cases.json does not match its version-1 schema')
  }
  const logs = table.logs
  const captures = new Map(Object.keys(LOGS).map(name => [name, readCapture(name, logs[name])]))
  const cases = table.cases.map((entry: unknown): PrefixCase => {
    if (!isObject(entry) || typeof entry.id !== 'string') throw new Error(`invalid case ${JSON.stringify(entry)}`)
    const { id } = entry
    if (sortedKeys(entry) !== 'id,log,rows,ts') throw new Error(`${id}: case keys must be id, log, rows, ts`)
    const capture = typeof entry.log === 'string' ? captures.get(entry.log) : undefined
    if (capture === undefined || !isCount(entry.rows) || entry.rows > capture.rows.length
      || id !== `${entry.log as string}/${entry.rows}`) throw new Error(`${id}: invalid log or rows`)
    return { id, log: entry.log as string, rows: entry.rows, ts: parseExpected(entry.ts, entry.rows, id) }
  })
  return { captures, cases }
}

/** The header and the first `rows` committed rows, each with its LF. */
function prefix(capture: Capture, rows: number): string {
  return [capture.header, ...capture.rows.slice(0, rows)].map(line => `${line}\n`).join('')
}

/** Resolve a JSON pointer without `~` escapes, refusing a missing member. */
function at(root: unknown, pointer: string, id: string): unknown {
  if (!pointer.startsWith('/') || pointer.includes('~')) throw new Error(`${id}: unsupported pointer ${pointer}`)
  let node = root
  for (const key of pointer.slice(1).split('/')) {
    if (typeof node !== 'object' || node === null || !Object.hasOwn(node, key)) throw new Error(`${id}: ${pointer} does not exist`)
    node = (node as Record<string, unknown>)[key]
  }
  return node
}

/** Replace each `{ "$log": pointer }` and `{ "$closer": pointer }` with the value it names. */
function resolve(value: unknown, id: string, rows: unknown[], closers: unknown[]): unknown {
  if (Array.isArray(value)) return value.map(item => resolve(item, id, rows, closers))
  if (!isObject(value)) return value
  const keys = sortedKeys(value)
  if (keys === '$log' && typeof value.$log === 'string') return at(rows, value.$log, id)
  if (keys === '$closer' && typeof value.$closer === 'string') return at(closers, value.$closer, id)
  if (keys.includes('$')) throw new Error(`${id}: invalid reference ${JSON.stringify(value)}`)
  return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, resolve(item, id, rows, closers)]))
}

/** The independent expectation for one prefix, with references resolved against that prefix's rows. */
function expected(entry: PrefixCase, capture: Capture): RestoreResult {
  const rows = capture.rows.slice(0, entry.rows).map(row => JSON.parse(row) as unknown)
  const closers = resolve(entry.ts.closers, entry.id, rows, []) as unknown[]
  const rest = resolve(entry.ts, entry.id, rows, closers) as Expected
  return {
    header: capture.expectedHeader,
    inheritedEventCount: capture.inheritedEventCount,
    committedBytes: Buffer.byteLength(prefix(capture, entry.rows)),
    ...rest,
    closers,
  }
}

/** The byte lengths a following row is torn at. */
function tornLengths(row: string): number[] {
  const length = Buffer.byteLength(row)
  return [1, Math.floor(length / 2), length - 1]
}

const { captures, cases } = loadTable()

describe('shared prefix restore cases', () => {
  it('cover every row prefix of the unchanged captures once', () => {
    expect(cases).toHaveLength(CASE_COUNT)
    expect(new Set(cases.map(entry => entry.id)).size).toBe(CASE_COUNT)
    for (const [name, capture] of captures) {
      const swept = cases.filter(entry => entry.log === name).map(entry => entry.rows)
      expect(swept, name).toEqual(Array.from({ length: capture.rows.length + 1 }, (_, rows) => rows))
    }
    const torn = cases.flatMap((entry) => {
      const next = (captures.get(entry.log) as Capture).rows[entry.rows]
      return next === undefined ? [] : tornLengths(next)
    })
    expect(torn).toHaveLength(TORN_COUNT)
  })

  it('refuses malformed expectations and references', () => {
    const entry = cases[0] as PrefixCase
    expect(() => parseExpected({ ...entry.ts, storedEventCount: 1 }, 0, 'malformed')).toThrow('invalid expectation')
    expect(() => parseExpected({ ...entry.ts, extra: 1 }, 0, 'malformed')).toThrow('invalid expectation')
    expect(() => resolve({ $log: '/0/time' }, 'malformed', [], [])).toThrow('does not exist')
    expect(() => resolve({ $log: '/0/time', extra: 1 }, 'malformed', [{ time: 1 }], [])).toThrow('invalid reference')
  })

  for (const entry of cases) {
    it(entry.id, () => {
      const capture = captures.get(entry.log) as Capture
      const want = expected(entry, capture)
      const text = prefix(capture, entry.rows)
      const actual = restorePlainLog(Buffer.from(text))
      expect(actual).toStrictEqual(want)
      expect(JSON.stringify(actual.messages)).toBe(JSON.stringify(want.messages))
      const next = capture.rows[entry.rows]
      if (next === undefined) return
      // A writer stopped inside the next row: its partial bytes are no record.
      for (const length of tornLengths(next)) {
        const torn = Buffer.concat([Buffer.from(text), Buffer.from(next).subarray(0, length)])
        const restored = restorePlainLog(torn)
        expect(restored, `torn after ${length} bytes`).toStrictEqual(want)
        expect(JSON.stringify(restored.messages), `torn after ${length} bytes: member order`).toBe(JSON.stringify(want.messages))
      }
    })
  }
})
