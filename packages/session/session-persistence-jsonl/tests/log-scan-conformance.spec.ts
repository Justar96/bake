/**
 * Runs the shared log scan cases in `conformance/session/log-scan-cases.json`
 * through the real `scanLog`. The development Rust scan in
 * `rust/crates/bake-session` checks the same table, so every TypeScript
 * outcome here is the oracle for its Rust expectation. A `rust` override
 * names a native limit or a class-only rejection; TypeScript still asserts
 * its own outcome. The spec reads only the table and the one fixture log it
 * names.
 */

import { createHash } from 'node:crypto'
import { readFileSync } from 'node:fs'
import { describe, expect, it } from 'vitest'
import { scanLog } from '../src/format.ts'

const REPO = new URL('../../../../', import.meta.url)
const SCHEMA = 'bake/session-format-conformance/log-scan-cases'
const ORACLE = 'scanLog(log) from packages/session/session-persistence-jsonl/src/format.ts'
/** The only log a case may read whole. */
const FIXTURE = 'conformance/runtime/request-reconstruction/tool-call-turn/session.jsonl'
const FIXTURE_SHA256 = 'a7a8222990ef9f4c4f00a051c019de86d3156ca7f6c3c0d28af4be4cc3fbe657'
/** Both harnesses pin the table size, so a dropped case fails. */
const CASE_COUNT = 76
const CLASSES = ['Error', 'SessionFormatError', 'SessionFormatUnsupportedError']
const LIMITS = ['invalid-utf8', 'json-parser', 'number-lexeme', 'codec']

type Json = null | boolean | number | string | Json[] | { [key: string]: Json }

type Outcome =
  | {
    outcome: 'scanned'
    header: Record<string, Json>
    events: number
    inheritedEventCount: number
    committedBytes: number
    /** Expanded `sourceEventSeqs` by event index, where a row spells a range. */
    sources?: Record<string, number[]>
    /** Exact float64 bits of the event value at each pointer. */
    numbers?: { event: number; pointer: string; bits: string }[]
  }
  | { outcome: 'thrown'; class: string; message: string }

interface ScanCase {
  id: string
  log: Buffer
  ts: Outcome
  rust?: { outcome: 'native-subset'; limit: string } | { outcome: 'thrown-class' }
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

/** The case's bytes: `lines` each followed by LF, then `tail`; or hex; or the fixture. */
function caseLog(entry: Record<string, unknown>, id: string): Buffer {
  const sources = ['lines', 'bytesHex', 'fixture'].filter(key => Object.hasOwn(entry, key))
  if (sources.length !== 1 || (Object.hasOwn(entry, 'tail') && sources[0] !== 'lines')) {
    throw new Error(`${id}: exactly one log source; tail requires lines`)
  }
  if (Object.hasOwn(entry, 'fixture')) {
    if (entry.fixture !== FIXTURE || Object.hasOwn(entry, 'lines') || Object.hasOwn(entry, 'bytesHex')) {
      throw new Error(`${id}: a fixture case names only ${FIXTURE}`)
    }
    return readFileSync(new URL(FIXTURE, REPO))
  }
  if (Object.hasOwn(entry, 'bytesHex')) {
    if (typeof entry.bytesHex !== 'string' || !/^(?:[0-9a-f]{2})*$/.test(entry.bytesHex)
      || Object.hasOwn(entry, 'lines')) throw new Error(`${id}: invalid bytesHex`)
    return Buffer.from(entry.bytesHex, 'hex')
  }
  const { lines, tail = '' } = entry
  if (!Array.isArray(lines) || typeof tail !== 'string'
    || ![...lines, tail].every(line => typeof line === 'string' && !line.includes('\n'))) {
    throw new Error(`${id}: lines and tail must be strings without LF`)
  }
  return Buffer.from(lines.map(line => `${line as string}\n`).join('') + tail)
}

function parseOutcome(value: unknown, id: string): Outcome {
  if (isObject(value) && value.outcome === 'thrown' && sortedKeys(value) === 'class,message,outcome'
    && CLASSES.includes(value.class as string) && typeof value.message === 'string') {
    return value as Outcome
  }
  if (isObject(value) && value.outcome === 'scanned' && isObject(value.header) && isCount(value.events)
    && isCount(value.inheritedEventCount) && isCount(value.committedBytes)
    && Object.keys(value).every(key =>
      ['outcome', 'header', 'events', 'inheritedEventCount', 'committedBytes', 'sources', 'numbers'].includes(key))) {
    if (value.sources !== undefined && (!isObject(value.sources) || !Object.entries(value.sources).every(([key, sources]) =>
      /^(?:0|[1-9][0-9]*)$/.test(key) && Number(key) < (value.events as number)
      && Array.isArray(sources) && sources.every(isCount)))) {
      throw new Error(`${id}: invalid source expectations`)
    }
    if (value.numbers !== undefined && (!Array.isArray(value.numbers) || !value.numbers.every(number =>
      isObject(number) && sortedKeys(number) === 'bits,event,pointer' && isCount(number.event)
      && (number.event as number) < (value.events as number) && typeof number.pointer === 'string'
      && number.pointer.startsWith('/') && !number.pointer.includes('~')
      && typeof number.bits === 'string' && /^[0-9a-f]{16}$/.test(number.bits)))) {
      throw new Error(`${id}: invalid number expectations`)
    }
    return value as Outcome
  }
  throw new Error(`${id}: invalid outcome ${JSON.stringify(value)}`)
}

function parseRust(value: unknown, ts: Outcome, id: string): ScanCase['rust'] {
  if (isObject(value) && value.outcome === 'native-subset' && sortedKeys(value) === 'limit,outcome'
    && LIMITS.includes(value.limit as string)) return { outcome: 'native-subset', limit: value.limit as string }
  if (isObject(value) && value.outcome === 'thrown-class' && sortedKeys(value) === 'outcome' && ts.outcome === 'thrown') {
    return { outcome: 'thrown-class' }
  }
  throw new Error(`${id}: invalid rust override ${JSON.stringify(value)}`)
}

function loadTable(): ScanCase[] {
  const table: unknown = JSON.parse(readFileSync(new URL('conformance/session/log-scan-cases.json', REPO), 'utf8'))
  if (!isObject(table) || sortedKeys(table) !== 'cases,history,oracle,schema,version' || table.schema !== SCHEMA
    || table.version !== 2 || table.oracle !== ORACLE || !Array.isArray(table.cases)
    || !Array.isArray(table.history) || !table.history.every(line => typeof line === 'string')) {
    throw new Error('log-scan-cases.json does not match its version-2 schema')
  }
  return table.cases.map((entry: unknown): ScanCase => {
    if (!isObject(entry) || typeof entry.id !== 'string') throw new Error(`invalid case ${JSON.stringify(entry)}`)
    const { id } = entry
    const unknown = Object.keys(entry).filter(key => !['id', 'lines', 'tail', 'bytesHex', 'fixture', 'ts', 'rust'].includes(key))
    if (unknown.length > 0) throw new Error(`${id}: unknown keys ${unknown.join()}`)
    const ts = parseOutcome(entry.ts, id)
    const rust = Object.hasOwn(entry, 'rust') ? parseRust(entry.rust, ts, id) : undefined
    return { id, log: caseLog(entry, id), ts, ...(rust === undefined ? {} : { rust }) }
  })
}

/** Resolve a JSON pointer without `~` escapes. */
function at(value: unknown, pointer: string): unknown {
  return pointer.slice(1).split('/').reduce<unknown>((node, key) => (node as Record<string, unknown>)[key], value)
}

function float64Bits(value: number): string {
  const view = new DataView(new ArrayBuffer(8))
  view.setFloat64(0, value)
  return view.getBigUint64(0).toString(16).padStart(16, '0')
}

/** The expected events: the leading records, as `JSON.parse` reads them, with source ranges expanded. */
function expectedEvents(log: Buffer, ts: Extract<Outcome, { outcome: 'scanned' }>): unknown[] {
  const records = log.toString('utf8').split('\n').slice(1, ts.events + 1)
  return records.map((record, index) => {
    const row = JSON.parse(record) as Record<string, unknown>
    const sources = ts.sources?.[String(index)]
    return sources === undefined ? row : { ...row, sourceEventSeqs: sources }
  })
}

const cases = loadTable()

describe('shared log scan cases', () => {
  it('rejects ambiguous log sources and ignored expectations', () => {
    for (const entry of [
      { fixture: FIXTURE, tail: '{' },
      { bytesHex: '7b', tail: '}' },
      { fixture: FIXTURE, lines: [] },
    ]) expect(() => caseLog(entry, 'ambiguous')).toThrow('exactly one log source')
    const scanned = { outcome: 'scanned', header: {}, events: 1, inheritedEventCount: 0, committedBytes: 1 }
    for (const extra of [{ sources: [] }, { sources: { 1: [0] } }, { numbers: {} },
      { numbers: [{ event: 1, pointer: '/data', bits: '0000000000000000' }] }]) {
      expect(() => parseOutcome({ ...scanned, ...extra }, 'malformed')).toThrow('expectations')
    }
  })

  it('read the unchanged fixture and pin the table', () => {
    expect(createHash('sha256').update(readFileSync(new URL(FIXTURE, REPO))).digest('hex')).toBe(FIXTURE_SHA256)
    expect(cases).toHaveLength(CASE_COUNT)
    expect(new Set(cases.map(entry => entry.id)).size).toBe(CASE_COUNT)
    expect(new Set(cases.flatMap(entry => entry.ts.outcome === 'thrown' ? [entry.ts.class] : []))).toEqual(new Set(CLASSES))
    // A limit claims nothing, so each one covers input the scanner accepts and input it throws for.
    for (const limit of LIMITS) {
      const outcomes = cases.flatMap(entry =>
        entry.rust?.outcome === 'native-subset' && entry.rust.limit === limit ? [entry.ts.outcome] : [])
      expect(new Set(outcomes), limit).toEqual(new Set(['scanned', 'thrown']))
    }
  })

  for (const entry of cases) {
    it(entry.id, () => {
      let scan: ReturnType<typeof scanLog>
      try {
        scan = scanLog(entry.log)
      } catch (error) {
        if (!(error instanceof Error)) throw error
        expect({ outcome: 'thrown', class: error.constructor.name, message: error.message }).toEqual(entry.ts)
        return
      }
      const { ts } = entry
      if (ts.outcome !== 'scanned') throw new Error(`${entry.id}: scanned, expected ${ts.class}: ${ts.message}`)
      expect({ ...scan.meta }).toEqual(ts.header)
      expect(scan.inheritedEventCount).toBe(ts.inheritedEventCount)
      expect(scan.committedBytes).toBe(ts.committedBytes)
      // Object.is equality: -0 stays distinct from 0.
      expect(scan.events).toEqual(expectedEvents(entry.log, ts))
      for (const { event, pointer, bits } of ts.numbers ?? []) {
        const value = at(scan.events[event], pointer)
        expect(typeof value).toBe('number')
        expect(float64Bits(value as number)).toBe(bits)
      }
    })
  }
})
