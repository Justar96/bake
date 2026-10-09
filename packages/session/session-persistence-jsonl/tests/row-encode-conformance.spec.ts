/**
 * Runs the shared cases in `conformance/session/row-encode-cases.json`
 * through the real current-format writers: `JSON.stringify(toHeaderLine(header,
 * inheritedEventCount))` for a header and `eventLine(event)` for an event.
 * A log case joins its encoded lines, each followed by LF, and reads them back
 * with the real `scanLog`. The development Rust encoder in
 * `rust/crates/bake-session` checks the same table. `ts` is the hand-written
 * line, or the thrown class with its exact message; an engine `TypeError`
 * carries no message. A `rust` override names a native limit, and TypeScript
 * still asserts its own outcome. The spec reads only the table.
 */

import { readFileSync } from 'node:fs'
import { describe, expect, it } from 'vitest'
import { SessionLogOffset } from 'bake-session'
import type { SessionEvent, SessionHeader } from 'bake-session'
import { SessionFormatError, SessionFormatUnsupportedMigrationError } from 'bake-session-format'
import { eventLine, scanLog, toHeaderLine } from '../src/format.ts'

const REPO = new URL('../../../../', import.meta.url)
const SCHEMA = 'bake/session-conformance/row-encode-cases'
const ORACLE = 'JSON.stringify(toHeaderLine(header, inheritedEventCount)) and eventLine(event) from packages/session/session-persistence-jsonl/src/format.ts; log cases join the lines and scanLog reads them back'
/** Both harnesses pin the table size, so a dropped case fails. */
const CASE_COUNT = 101
const LIMITS = ['float-number', 'type-error', 'source-coercion', 'unreadable-row', 'codec']
/** Classes are matched exactly: the unsupported error subclasses SessionFormatError. */
const CLASSES = new Map<string, abstract new (...args: never[]) => Error>([
  ['Error', Error],
  ['TypeError', TypeError],
  ['SessionFormatError', SessionFormatError],
  ['SessionFormatUnsupportedMigrationError', SessionFormatUnsupportedMigrationError],
])

type Outcome =
  | { outcome: 'encoded'; line: string }
  | { outcome: 'thrown'; class: string; message?: string }

type RowCase =
  | { id: string; kind: 'header'; header: unknown; inheritedEventCount?: number; ts: Outcome; rust?: string }
  | { id: string; kind: 'event'; event: unknown; ts: Outcome; rust?: string }
  | {
    id: string
    kind: 'log'
    header: SessionHeader
    inheritedEventCount?: number
    events: SessionEvent[]
    ts: { outcome: 'scanned'; log: string; inheritedEventCount: number }
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

function parseOutcome(value: unknown, id: string): Outcome {
  if (isObject(value)) {
    const keys = sortedKeys(value)
    if (keys === 'line,outcome' && value.outcome === 'encoded' && typeof value.line === 'string') {
      return { outcome: 'encoded', line: value.line }
    }
    if (value.outcome === 'thrown' && typeof value.class === 'string' && CLASSES.has(value.class)) {
      if (keys === 'class,message,outcome' && typeof value.message === 'string') {
        return { outcome: 'thrown', class: value.class, message: value.message }
      }
      if (keys === 'class,outcome' && value.class === 'TypeError') return { outcome: 'thrown', class: 'TypeError' }
    }
  }
  throw new Error(`${id}: invalid ts outcome ${JSON.stringify(value)}`)
}

function parseRust(value: unknown, id: string): string | undefined {
  if (value === undefined) return undefined
  if (isObject(value) && sortedKeys(value) === 'limit,outcome' && value.outcome === 'native-subset'
    && LIMITS.includes(value.limit as string)) return value.limit as string
  throw new Error(`${id}: invalid rust override ${JSON.stringify(value)}`)
}

function loadTable(): RowCase[] {
  const table: unknown = JSON.parse(readFileSync(new URL('conformance/session/row-encode-cases.json', REPO), 'utf8'))
  if (!isObject(table) || sortedKeys(table) !== 'cases,history,oracle,schema,version' || table.schema !== SCHEMA
    || table.version !== 3 || table.oracle !== ORACLE || !Array.isArray(table.cases)
    || !Array.isArray(table.history) || !table.history.every(line => typeof line === 'string')) {
    throw new Error('row-encode-cases.json does not match its version-3 schema')
  }
  return table.cases.map((entry: unknown): RowCase => {
    if (!isObject(entry) || typeof entry.id !== 'string') throw new Error(`invalid case ${JSON.stringify(entry)}`)
    const { id } = entry
    if (entry.note !== undefined && typeof entry.note !== 'string') throw new Error(`${id}: invalid note`)
    if (entry.inheritedEventCount !== undefined && !Number.isInteger(entry.inheritedEventCount)) {
      throw new Error(`${id}: invalid inheritedEventCount`)
    }
    const count = entry.inheritedEventCount === undefined ? {} : { inheritedEventCount: entry.inheritedEventCount as number }
    const allowed = entry.kind === 'event'
      ? ['id', 'kind', 'event', 'ts', 'rust', 'note']
      : entry.kind === 'header'
        ? ['id', 'kind', 'header', 'inheritedEventCount', 'ts', 'rust', 'note']
        : ['id', 'kind', 'header', 'inheritedEventCount', 'events', 'ts', 'note']
    const unknown = Object.keys(entry).filter(key => !allowed.includes(key))
    if (unknown.length > 0) throw new Error(`${id}: unknown keys ${unknown.join()}`)
    if (entry.kind === 'log') {
      const ts = entry.ts
      if (!isObject(entry.header) || !Array.isArray(entry.events) || !isObject(ts)
        || sortedKeys(ts) !== 'inheritedEventCount,log,outcome' || ts.outcome !== 'scanned'
        || typeof ts.log !== 'string' || !isCount(ts.inheritedEventCount)) {
        throw new Error(`${id}: invalid log case`)
      }
      return {
        id,
        kind: 'log',
        header: entry.header as unknown as SessionHeader,
        ...count,
        events: entry.events as SessionEvent[],
        ts: ts as { outcome: 'scanned'; log: string; inheritedEventCount: number },
      }
    }
    const ts = parseOutcome(entry.ts, id)
    const rust = parseRust(entry.rust, id)
    if (ts.outcome === 'thrown' && ts.message === undefined && rust === undefined) {
      throw new Error(`${id}: a TypeError without a message needs a rust limit`)
    }
    const limit = rust === undefined ? {} : { rust }
    if (entry.kind === 'header' && Object.hasOwn(entry, 'header')) return { id, kind: 'header', header: entry.header, ...count, ts, ...limit }
    if (entry.kind === 'event' && Object.hasOwn(entry, 'event')) return { id, kind: 'event', event: entry.event, ts, ...limit }
    throw new Error(`${id}: invalid kind ${JSON.stringify(entry.kind)}`)
  })
}

function headerLine(header: unknown, inheritedEventCount: number | undefined): string {
  const count = inheritedEventCount === undefined ? undefined : inheritedEventCount as SessionLogOffset
  return JSON.stringify(toHeaderLine(header as SessionHeader, count))
}

/** Run one writer; an error of an unlisted class fails the case. */
function outcome(write: () => string): Outcome {
  try {
    return { outcome: 'encoded', line: write() }
  } catch (error) {
    const constructor = (error as object).constructor
    const name = [...CLASSES].find(([, value]) => value === constructor)?.[0]
    if (name === undefined) throw error
    return { outcome: 'thrown', class: name, message: (error as Error).message }
  }
}

const cases = loadTable()

describe('shared current-format row encoding cases', () => {
  it('pin the table and witness every limit, kind, and class', () => {
    expect(cases).toHaveLength(CASE_COUNT)
    expect(new Set(cases.map(entry => entry.id)).size).toBe(CASE_COUNT)
    expect(new Set(cases.flatMap(entry => entry.kind !== 'log' && entry.rust !== undefined ? [entry.rust] : [])))
      .toEqual(new Set(LIMITS))
    expect(new Set(cases.map(entry => entry.kind))).toEqual(new Set(['header', 'event', 'log']))
    expect(new Set(cases.flatMap(entry => entry.kind !== 'log' && entry.ts.outcome === 'thrown' ? [entry.ts.class] : [])))
      .toEqual(new Set(CLASSES.keys()))
  })

  it('refuses malformed outcomes and overrides', () => {
    expect(() => parseOutcome({ outcome: 'thrown', class: 'RangeError', message: 'x' }, 'malformed')).toThrow('invalid ts outcome')
    expect(() => parseOutcome({ outcome: 'thrown', class: 'SessionFormatError' }, 'malformed')).toThrow('invalid ts outcome')
    expect(() => parseRust({ outcome: 'native-subset', limit: 'other' }, 'malformed')).toThrow('invalid rust override')
  })

  for (const entry of cases) {
    it(entry.id, () => {
      if (entry.kind === 'log') {
        const lines = [headerLine(entry.header, entry.inheritedEventCount), ...entry.events.map(event => eventLine(event))]
        const log = lines.map(line => `${line}\n`).join('')
        expect(log).toBe(entry.ts.log)
        const scan = scanLog(Buffer.from(log, 'utf8'))
        expect(scan.committedBytes).toBe(Buffer.byteLength(log))
        expect(scan.inheritedEventCount).toBe(entry.ts.inheritedEventCount)
        expect(scan.meta).toStrictEqual({ ...entry.header, delegationDepth: entry.header.delegationDepth ?? 0 })
        expect(scan.events).toStrictEqual(entry.events)
        return
      }
      const actual = entry.kind === 'header'
        ? outcome(() => headerLine(entry.header, entry.inheritedEventCount))
        : outcome(() => eventLine(entry.event as SessionEvent))
      if (entry.ts.outcome === 'thrown' && entry.ts.message === undefined) {
        expect(actual.outcome === 'thrown' ? actual.class : actual).toBe(entry.ts.class)
      } else {
        expect(actual).toStrictEqual(entry.ts)
      }
    })
  }
})
