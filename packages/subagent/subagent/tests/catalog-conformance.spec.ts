/**
 * Runs the shared cases in `conformance/session/subagent-catalog-cases.json`
 * through `subagentCatalogProjectionDefinition`. Each fold starts from
 * `init` with the restored inherited cut and applies each case's
 * `JSON.parse`d rows, then their `interruptedTurnClosers`: the events a
 * restored Session holds before its end seed. Session construction
 * (`Session.fromRestore`) must admit the same rows and cut. The development
 * Rust port in `rust/crates/bake-session` checks that the full restoration
 * path reaches the same catalog.
 *
 * A `catalog` outcome is the wire view after every event. A `rejected`
 * outcome is the `ZodError` that `apply` throws at its first invalid own
 * catalog event, named by that event's seq; Zod's issue text is not part of
 * the contract. `createdAt` keeps the sign of -0, so the table spells it
 * `-0.0` and the comparison uses `Object.is`. Each case edits one runtime
 * capture as text.
 */

import { createHash } from 'node:crypto'
import { readFileSync } from 'node:fs'
import { describe, expect, it } from 'vitest'
import { z } from 'zod'
import { Session, SessionId, SessionLogOffset, interruptedTurnClosers } from 'bake-session'
import type { SessionEvent, SessionHeader } from 'bake-session'
import { subagentCatalogProjectionDefinition } from '../src/catalog.ts'

const REPO = new URL('../../../../', import.meta.url)
const TABLE = 'conformance/session/subagent-catalog-cases.json'
const SCHEMA = 'bake/session-conformance/subagent-catalog-cases'
const ORACLE = 'subagentCatalogProjectionDefinition in packages/subagent/subagent/src/catalog.ts, folded from init over each case\'s parsed rows and their interruptedTurnClosers with the restored inherited cut'
const LOGS: Record<string, { path: string; sha256: string }> = {
  'tool-call-turn': {
    path: 'conformance/runtime/request-reconstruction/tool-call-turn/session.jsonl',
    sha256: 'a7a8222990ef9f4c4f00a051c019de86d3156ca7f6c3c0d28af4be4cc3fbe657',
  },
}
/** Both harnesses pin the table size, so a dropped case fails. */
const CASE_COUNT = 82
/** Outcome mix and coverage the table must keep. */
const REJECTED_COUNT = 48
const NEGATIVE_ZERO_ENTRIES = 4
const INHERITED_CUTS = [0, 17, 19]
/** The TypeScript list's chunk capacity; the table must cross it. */
const CHUNK_CAPACITY = 64

type Edit =
  | { truncate: number }
  | { header: string }
  | { append: string }

interface Entry {
  id: string
  createdAt: number
  mode: 'one-shot' | 'continuable'
  label?: string
}

type Expected =
  | { outcome: 'catalog'; inheritedEventCount: number; entries: Entry[] }
  | { outcome: 'rejected'; inheritedEventCount: number; seq: number; class: 'ZodError' }

interface CatalogCase {
  id: string
  header: SessionHeader
  rows: SessionEvent[]
  ts: Expected
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

function isLine(value: unknown): value is string {
  return typeof value === 'string' && !value.includes('\n')
}

/** Apply edits to one capture's header and rows. */
function caseLog(name: string, edits: unknown[], id: string): { header: string; rows: string[] } {
  const source = LOGS[name]
  if (source === undefined) throw new Error(`${id}: unknown log ${name}`)
  const lines = readFileSync(new URL(source.path, REPO), 'utf8').slice(0, -1).split('\n')
  let header = lines[0] as string
  let rows = lines.slice(1)
  for (const value of edits) {
    const edit = parseEdit(value, rows.length, id)
    if ('truncate' in edit) {
      rows = rows.slice(0, edit.truncate)
    } else if ('header' in edit) {
      header = edit.header
    } else {
      rows.push(edit.append)
    }
  }
  return { header, rows }
}

function parseEdit(value: unknown, rows: number, id: string): Edit {
  if (isObject(value)) {
    const keys = sortedKeys(value)
    if (keys === 'truncate' && isCount(value.truncate) && value.truncate <= rows) return value as Edit
    if ((keys === 'header' || keys === 'append') && isLine(Object.values(value)[0])) return value as Edit
  }
  throw new Error(`${id}: invalid edit ${JSON.stringify(value)}`)
}

/** An expected entry: `createdAt` is a nonnegative safe integer, -0 included. */
function isEntry(value: unknown): boolean {
  if (!isObject(value) || typeof value.id !== 'string') return false
  if (!(Number.isSafeInteger(value.createdAt) && (value.createdAt as number) >= 0)) return false
  const keys = sortedKeys(value)
  if (value.mode === 'continuable') return keys === 'createdAt,id,label,mode' && typeof value.label === 'string'
  return value.mode === 'one-shot'
    && (keys === 'createdAt,id,mode' || (keys === 'createdAt,id,label,mode' && typeof value.label === 'string'))
}

function parseExpected(value: unknown, id: string): Expected {
  if (isObject(value) && isCount(value.inheritedEventCount)) {
    const keys = sortedKeys(value)
    if (value.outcome === 'catalog' && keys === 'entries,inheritedEventCount,outcome'
      && Array.isArray(value.entries) && value.entries.every(isEntry)) {
      return value as unknown as Expected
    }
    if (value.outcome === 'rejected' && keys === 'class,inheritedEventCount,outcome,seq'
      && value.class === 'ZodError' && isCount(value.seq)) {
      return value as unknown as Expected
    }
  }
  throw new Error(`${id}: invalid ts outcome ${JSON.stringify(value)}`)
}

function loadTable(): CatalogCase[] {
  const table: unknown = JSON.parse(readFileSync(new URL(TABLE, REPO), 'utf8'))
  if (!isObject(table) || sortedKeys(table) !== 'cases,history,logs,oracle,schema,version' || table.schema !== SCHEMA
    || table.version !== 1 || table.oracle !== ORACLE || !Array.isArray(table.cases)
    || !Array.isArray(table.history) || !table.history.every(isLine)
    || JSON.stringify(table.logs) !== JSON.stringify(Object.fromEntries(Object.entries(LOGS).map(([name, { path }]) => [name, path])))) {
    throw new Error('subagent-catalog-cases.json does not match its version-1 schema')
  }
  return table.cases.map(parseCase)
}

function parseCase(entry: unknown): CatalogCase {
  if (!isObject(entry) || typeof entry.id !== 'string' || typeof entry.log !== 'string' || !Array.isArray(entry.edits)) {
    throw new Error(`invalid case ${JSON.stringify(entry)}`)
  }
  const { id } = entry
  const unknown = Object.keys(entry).filter(key => !['id', 'log', 'edits', 'ts', 'note'].includes(key))
  if (unknown.length > 0) throw new Error(`${id}: unknown keys ${unknown.join()}`)
  if (entry.note !== undefined && typeof entry.note !== 'string') throw new Error(`${id}: invalid note`)
  const ts = parseExpected(entry.ts, id)
  const { header, rows } = caseLog(entry.log, entry.edits, id)
  const { type: _type, ...meta } = JSON.parse(header) as SessionHeader & { type: string }
  return { id, header: meta, rows: rows.map(row => JSON.parse(row) as SessionEvent), ts }
}

/**
 * Fold the production definition from `init`. Only a `ZodError` thrown by
 * `apply` is an outcome, named by the event being applied; anything else
 * fails the case.
 */
function fold(header: SessionHeader, inherited: number, events: readonly SessionEvent[]): Expected {
  const inheritedEventCount = SessionLogOffset(inherited)
  let state = subagentCatalogProjectionDefinition.init(header, inheritedEventCount)
  for (const event of events) {
    try {
      state = subagentCatalogProjectionDefinition.apply(state, event)
    } catch (error: unknown) {
      if (!(error instanceof z.ZodError)) throw error
      return { outcome: 'rejected', inheritedEventCount: inherited, seq: event.seq, class: 'ZodError' }
    }
  }
  const entries = subagentCatalogProjectionDefinition.wire.view(state)
  // The published view must satisfy the definition's own wire schema.
  subagentCatalogProjectionDefinition.wire.viewSchema.parse(entries)
  return { outcome: 'catalog', inheritedEventCount: inherited, entries }
}

/** `toStrictEqual` plus the sign of each `createdAt`, which must survive. */
function expectOutcome(actual: Expected, expected: Expected): void {
  expect(actual).toStrictEqual(expected)
  if (actual.outcome === 'catalog' && expected.outcome === 'catalog') {
    expect(actual.entries.map(entry => Object.is(entry.createdAt, -0)))
      .toEqual(expected.entries.map(entry => Object.is(entry.createdAt, -0)))
  }
}

const cases = loadTable()

describe('shared subagent catalog projection cases', () => {
  it('read the unchanged capture and pin the table', () => {
    for (const { path, sha256 } of Object.values(LOGS)) {
      expect(createHash('sha256').update(readFileSync(new URL(path, REPO))).digest('hex'), path).toBe(sha256)
    }
    expect(cases).toHaveLength(CASE_COUNT)
    expect(new Set(cases.map(entry => entry.id)).size).toBe(CASE_COUNT)
    const rejected = cases.filter(entry => entry.ts.outcome === 'rejected')
    expect(rejected).toHaveLength(REJECTED_COUNT)
    const entries = cases.flatMap(entry => entry.ts.outcome === 'catalog' ? entry.ts.entries : [])
    expect(entries.filter(entry => Object.is(entry.createdAt, -0))).toHaveLength(NEGATIVE_ZERO_ENTRIES)
    expect(new Set(entries.map(entry => entry.mode))).toEqual(new Set(['one-shot', 'continuable']))
    expect(entries.some(entry => entry.mode === 'one-shot' && entry.label === undefined)).toBe(true)
    expect(Math.max(...cases.map(entry => entry.ts.outcome === 'catalog' ? entry.ts.entries.length : 0)))
      .toBeGreaterThan(CHUNK_CAPACITY * 2)
    expect([...new Set(cases.map(entry => entry.ts.inheritedEventCount))].sort((a, b) => a - b))
      .toEqual(INHERITED_CUTS)
    expect(cases.filter(entry => entry.header.isSeeded).length).toBeGreaterThan(0)
    // Some rejection and some catalog follow an inherited cut.
    expect(rejected.some(entry => entry.ts.inheritedEventCount > 0)).toBe(true)
    expect(cases.some(entry => entry.ts.outcome === 'catalog' && entry.ts.inheritedEventCount > 0
      && entry.ts.entries.length > 0)).toBe(true)
    // Some cases reach their outcome through interrupted-turn closers.
    expect(cases.filter(entry => interruptedTurnClosers(entry.rows).length > 0).length).toBeGreaterThan(3)
  })

  it('refuses malformed edits and outcomes', () => {
    for (const edit of [{ truncate: 99 }, { truncate: -1 }, { append: 'a\nb' }, { header: 1 }, { tail: 'x' }]) {
      expect(() => parseEdit(edit, 16, 'malformed')).toThrow('invalid edit')
    }
    const entry = { id: 'c', createdAt: 1, mode: 'one-shot' }
    for (const outcome of [
      { outcome: 'catalog', entries: [] },
      { outcome: 'catalog', inheritedEventCount: -0, entries: [] },
      { outcome: 'catalog', inheritedEventCount: 0, entries: [{ ...entry, createdAt: 1.5 }] },
      { outcome: 'catalog', inheritedEventCount: 0, entries: [{ ...entry, createdAt: -1 }] },
      { outcome: 'catalog', inheritedEventCount: 0, entries: [{ ...entry, createdAt: 2 ** 53 }] },
      { outcome: 'catalog', inheritedEventCount: 0, entries: [{ ...entry, mode: 'continuable' }] },
      { outcome: 'catalog', inheritedEventCount: 0, entries: [{ ...entry, label: null }] },
      { outcome: 'catalog', inheritedEventCount: 0, entries: [{ ...entry, extra: 1 }] },
      { outcome: 'catalog', inheritedEventCount: 0, entries: [], seq: 1 },
      { outcome: 'rejected', inheritedEventCount: 0, seq: 1 },
      { outcome: 'rejected', inheritedEventCount: 0, seq: 1, class: 'TypeError' },
      { outcome: 'rejected', inheritedEventCount: 0, seq: 1.5, class: 'ZodError' },
      { outcome: 'other', inheritedEventCount: 0 },
    ]) {
      expect(() => parseExpected(outcome, 'malformed')).toThrow('invalid ts outcome')
    }
    const negative = { outcome: 'catalog', inheritedEventCount: 0, entries: [{ ...entry, createdAt: -0 }] }
    expect(parseExpected(negative, 'negative')).toBe(negative)
    // The sign check fails a fold that loses -0.
    expect(() => expectOutcome(
      { outcome: 'catalog', inheritedEventCount: 0, entries: [{ id: 'c', createdAt: 0, mode: 'one-shot' }] },
      negative as Expected,
    )).toThrow()
    const ts = { outcome: 'catalog', inheritedEventCount: 0, entries: [] }
    expect(() => parseCase({ id: 'override', log: 'tool-call-turn', edits: [], ts, rust: {} }))
      .toThrow('override: unknown keys rust')
  })

  it('keeps a non-Zod failure out of the outcomes', () => {
    const header = cases[0]?.header as SessionHeader
    // A TypeError from reading the payload is not a schema refusal.
    const hostile = { type: 'subagent/catalog', seq: 0, time: 0, get data(): never { throw new TypeError('boom') } }
    expect(() => fold(header, 0, [hostile as unknown as SessionEvent])).toThrow('boom')
  })

  for (const entry of cases) {
    it(entry.id, () => {
      const closers = interruptedTurnClosers(entry.rows)
      const events = [...entry.rows, ...closers]
      const marker = entry.rows.findLast(event => event.type === 'session/end-seed'
        && (event.data as { inherited?: unknown }).inherited === true)
      const inherited = entry.header.isSeeded && marker !== undefined ? marker.seq : 0
      // Session construction admits the rows and cut the fold reads.
      const session = Session.fromRestore(SessionId(entry.header.id), events, entry.header, SessionLogOffset(inherited), 'detached')
      expect(session.inheritedEventCount).toBe(entry.ts.inheritedEventCount)
      expectOutcome(fold(entry.header, inherited, events), entry.ts)
    })
  }
})
