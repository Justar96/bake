/**
 * Runs the shared token-usage cases in `conformance/session/usage-cases.json`
 * through `tokenUsageProjectionDefinition`, folding `init` and `apply` over
 * each case's `JSON.parse`d rows and then their `interruptedTurnClosers`, the
 * events a restored Session holds before its end seed. Session construction
 * (`Session.fromRestore`, without message projections) must admit the same
 * rows; the development Rust fold in `rust/crates/bake-session` checks that
 * the full restoration path does. A `rust` override names a native limit and
 * the seq Rust refuses; TypeScript still asserts its own outcome. Each case
 * edits one runtime capture as text.
 */

import { createHash } from 'node:crypto'
import { readFileSync } from 'node:fs'
import { describe, expect, it } from 'vitest'
import { Session, SessionId, SessionLogOffset, interruptedTurnClosers } from 'bake-session'
import type { SessionEvent, SessionHeader } from 'bake-session'
import { tokenUsageProjectionDefinition } from '../src/usage-projection.ts'

const REPO = new URL('../../../../', import.meta.url)
const SCHEMA = 'bake/session-conformance/usage-cases'
const ORACLE = 'tokenUsageProjectionDefinition.init/apply in packages/llm/token-meter/src/usage-projection.ts, folded over each case\'s parsed rows and their interruptedTurnClosers'
const LOGS: Record<string, { path: string; sha256: string }> = {
  'tool-call-turn': {
    path: 'conformance/runtime/request-reconstruction/tool-call-turn/session.jsonl',
    sha256: 'a7a8222990ef9f4c4f00a051c019de86d3156ca7f6c3c0d28af4be4cc3fbe657',
  },
  'dynamic-tools': {
    path: 'conformance/runtime/request-reconstruction/dynamic-tools/session.jsonl',
    sha256: '43852e686ea6ef5f599065a7ead57f82d27f8b20e9e936e81b0b0596636e61e2',
  },
  'retry-attempt': {
    path: 'conformance/runtime/request-reconstruction/retry-attempt/session.jsonl',
    sha256: 'cd79f036ffd20337af3393ab1dcfb57f62aa7434129a6ec3ddea9398c9a7a2db',
  },
}
/** Both harnesses pin the table size, so a dropped case fails. */
const CASE_COUNT = 34
const LIMITS = ['number', 'usage', 'stream', 'retry']

type State = Parameters<typeof tokenUsageProjectionDefinition.apply>[0]

type Edit =
  | { truncate: number }
  | { append: string }
  | { row: number; text: string }
  | { row: number; find: string; replace: string }

type Outcome =
  | { outcome: 'folded'; totals: State['totals']; last: State['last'] }
  | { outcome: 'rejected'; class: 'TypeError'; seq: number }

interface UsageCase {
  id: string
  header: SessionHeader
  rows: SessionEvent[]
  ts: Outcome
  rust?: { outcome: 'native-subset'; limit: string; seq: number }
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

/** Apply row edits to one capture; a find must match exactly once. */
function caseRows(name: string, edits: unknown[], id: string): { header: string; rows: string[] } {
  const source = LOGS[name]
  if (source === undefined) throw new Error(`${id}: unknown log ${name}`)
  const lines = readFileSync(new URL(source.path, REPO), 'utf8').slice(0, -1).split('\n')
  const header = lines[0] as string
  let rows = lines.slice(1)
  for (const value of edits) {
    const edit = parseEdit(value, rows.length, id)
    if ('truncate' in edit) {
      rows = rows.slice(0, edit.truncate)
    } else if ('append' in edit) {
      rows.push(edit.append)
    } else if ('find' in edit) {
      const row = rows[edit.row] as string
      if (row.split(edit.find).length !== 2) throw new Error(`${id}: find must match row ${edit.row} once`)
      rows[edit.row] = row.replace(edit.find, () => edit.replace)
    } else {
      rows[edit.row] = edit.text
    }
  }
  return { header, rows }
}

function parseEdit(value: unknown, rows: number, id: string): Edit {
  const row = (entry: Record<string, unknown>) => isCount(entry.row) && entry.row < rows
  if (isObject(value)) {
    const keys = sortedKeys(value)
    if (keys === 'truncate' && isCount(value.truncate) && value.truncate <= rows) return value as Edit
    if (keys === 'append' && isLine(value.append)) return value as Edit
    if (keys === 'row,text' && row(value) && isLine(value.text)) return value as Edit
    if (keys === 'find,replace,row' && row(value) && isLine(value.find) && value.find !== '' && isLine(value.replace)) {
      return value as Edit
    }
  }
  throw new Error(`${id}: invalid edit ${JSON.stringify(value)}`)
}

function isBuckets(value: unknown): boolean {
  return isObject(value) && sortedKeys(value) === 'cacheReadTokens,cacheWriteTokens,outputTokens,uncachedInputTokens'
    && Object.values(value).every(Number.isInteger)
}

function parseOutcome(value: unknown, id: string): Outcome {
  if (isObject(value) && value.outcome === 'rejected' && sortedKeys(value) === 'class,outcome,seq'
    && value.class === 'TypeError' && isCount(value.seq)) return value as Outcome
  if (isObject(value) && value.outcome === 'folded' && sortedKeys(value) === 'last,outcome,totals'
    && isBuckets(value.totals)
    && (value.last === null || (isObject(value.last) && sortedKeys(value.last) === 'buckets,step,turn'
      && isCount(value.last.turn) && isCount(value.last.step) && isBuckets(value.last.buckets)))) {
    return value as Outcome
  }
  throw new Error(`${id}: invalid outcome ${JSON.stringify(value)}`)
}

function loadTable(): UsageCase[] {
  const table: unknown = JSON.parse(readFileSync(new URL('conformance/session/usage-cases.json', REPO), 'utf8'))
  if (!isObject(table) || sortedKeys(table) !== 'cases,history,logs,oracle,schema,version' || table.schema !== SCHEMA
    || table.version !== 1 || table.oracle !== ORACLE || !Array.isArray(table.cases)
    || !Array.isArray(table.history) || !table.history.every(isLine)
    || JSON.stringify(table.logs) !== JSON.stringify(Object.fromEntries(Object.entries(LOGS).map(([name, { path }]) => [name, path])))) {
    throw new Error('usage-cases.json does not match its version-1 schema')
  }
  return table.cases.map((entry: unknown): UsageCase => {
    if (!isObject(entry) || typeof entry.id !== 'string' || typeof entry.log !== 'string' || !Array.isArray(entry.edits)) {
      throw new Error(`invalid case ${JSON.stringify(entry)}`)
    }
    const { id } = entry
    const unknown = Object.keys(entry).filter(key => !['id', 'log', 'edits', 'ts', 'rust', 'note'].includes(key))
    if (unknown.length > 0) throw new Error(`${id}: unknown keys ${unknown.join()}`)
    if (entry.note !== undefined && typeof entry.note !== 'string') throw new Error(`${id}: invalid note`)
    const ts = parseOutcome(entry.ts, id)
    const { rust } = entry
    if (rust !== undefined && !(isObject(rust) && sortedKeys(rust) === 'limit,outcome,seq'
      && rust.outcome === 'native-subset' && LIMITS.includes(rust.limit as string) && isCount(rust.seq)
      && (ts.outcome !== 'rejected' || ts.seq === rust.seq))) {
      throw new Error(`${id}: invalid rust override ${JSON.stringify(rust)}`)
    }
    if (ts.outcome === 'rejected' && rust === undefined) throw new Error(`${id}: a rejection names its Rust limit`)
    const { header, rows } = caseRows(entry.log, entry.edits, id)
    const { type: _type, ...meta } = JSON.parse(header) as SessionHeader & { type: string }
    return {
      id,
      header: meta,
      rows: rows.map(row => JSON.parse(row) as SessionEvent),
      ts,
      ...(rust === undefined ? {} : { rust: rust as NonNullable<UsageCase['rust']> }),
    }
  })
}

/** Fold the definition over the events, reporting the seq of an event that throws. */
function fold(events: readonly SessionEvent[]): Outcome | { outcome: 'threw'; error: unknown; seq: number } {
  let state: State = tokenUsageProjectionDefinition.init()
  for (const event of events) {
    try {
      state = tokenUsageProjectionDefinition.apply(state, event)
    } catch (error) {
      return { outcome: 'threw', error, seq: event.seq }
    }
  }
  return { outcome: 'folded', ...state }
}

const cases = loadTable()

describe('shared token-usage cases', () => {
  it('read the unchanged captures and pin the table', () => {
    for (const { path, sha256 } of Object.values(LOGS)) {
      expect(createHash('sha256').update(readFileSync(new URL(path, REPO))).digest('hex'), path).toBe(sha256)
    }
    expect(cases).toHaveLength(CASE_COUNT)
    expect(new Set(cases.map(entry => entry.id)).size).toBe(CASE_COUNT)
    expect(new Set(cases.flatMap(entry => entry.rust === undefined ? [] : [entry.rust.limit]))).toEqual(new Set(LIMITS))
  })

  it('refuses malformed edits and outcomes', () => {
    for (const edit of [{ truncate: 99 }, { row: 99, text: '{}' }, { row: 0, find: '', replace: 'x' },
      { append: 'a\nb' }, { tail: 'x' }, { row: 0, text: '{}', extra: 1 }]) {
      expect(() => parseEdit(edit, 16, 'malformed')).toThrow('invalid edit')
    }
    expect(() => parseOutcome({ outcome: 'rejected', class: 'TypeError' }, 'malformed')).toThrow('invalid outcome')
    expect(() => caseRows('tool-call-turn', [{ row: 8, find: '"id":"c1"', replace: '' }], 'twice')).toThrow('once')
  })

  for (const entry of cases) {
    it(entry.id, () => {
      const closers = interruptedTurnClosers(entry.rows)
      const events = [...entry.rows, ...closers]
      // Session construction admits the rows the fold reads.
      Session.fromRestore(SessionId(entry.header.id), events, entry.header, SessionLogOffset(0), 'detached')
      const actual = fold(events)
      if (actual.outcome === 'threw') {
        if (entry.ts.outcome !== 'rejected') throw actual.error
        expect(actual.error).toBeInstanceOf(TypeError)
        expect(actual.seq).toBe(entry.ts.seq)
        return
      }
      expect(actual).toStrictEqual(entry.ts)
    })
  }
})
