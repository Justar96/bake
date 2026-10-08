/**
 * Runs the shared goal cases in `conformance/session/goal-cases.json` through
 * `goalProjectionDefinition`, folding `init` and `applyGoalProjection` over
 * each case's `JSON.parse`d rows and then their `interruptedTurnClosers`, the
 * events a restored Session holds before its end seed. Session construction
 * (`Session.fromRestore`, without message projections) must admit the same
 * rows; the development Rust fold in `rust/crates/bake-session` checks that
 * the full restoration path does. A `rust` override names a native limit and
 * the seq Rust refuses; TypeScript still asserts its own outcome. Each case
 * appends rows to one runtime capture as text.
 */

import { createHash } from 'node:crypto'
import { readFileSync } from 'node:fs'
import { describe, expect, it } from 'vitest'
import { Session, SessionId, SessionLogOffset, interruptedTurnClosers } from 'bake-session'
import type { SessionEvent, SessionHeader } from 'bake-session'
import { goalProjectionDefinition } from '../src/index.ts'
import type { GoalProjectionState } from '../src/types.ts'

const REPO = new URL('../../../../', import.meta.url)
const SCHEMA = 'bake/session-conformance/goal-cases'
const ORACLE = 'goalProjectionDefinition.init and applyGoalProjection in packages/goal/goal/src/index.ts, folded over each case\'s parsed rows and their interruptedTurnClosers'
const LOGS: Record<string, { path: string; sha256: string }> = {
  'tool-call-turn': {
    path: 'conformance/runtime/request-reconstruction/tool-call-turn/session.jsonl',
    sha256: 'a7a8222990ef9f4c4f00a051c019de86d3156ca7f6c3c0d28af4be4cc3fbe657',
  },
}
/** Both harnesses pin the table size, so a dropped case fails. */
const CASE_COUNT = 112
const LIMITS = ['number', 'version-diagnostic']

type Edit =
  | { truncate: number }
  | { append: string }
  | { row: number; text: string }

interface GoalCase {
  id: string
  header: SessionHeader
  rows: SessionEvent[]
  ts: GoalProjectionState
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

/** Apply row edits to one capture. */
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
    } else {
      rows[edit.row] = edit.text
    }
  }
  return { header, rows }
}

function parseEdit(value: unknown, rows: number, id: string): Edit {
  if (isObject(value)) {
    const keys = sortedKeys(value)
    if (keys === 'truncate' && isCount(value.truncate) && value.truncate <= rows) return value as Edit
    if (keys === 'append' && isLine(value.append)) return value as Edit
    if (keys === 'row,text' && isCount(value.row) && value.row < rows && isLine(value.text)) return value as Edit
  }
  throw new Error(`${id}: invalid edit ${JSON.stringify(value)}`)
}

/** Shape-check an expected state; the fold's own values are compared strictly later. */
function parseState(value: unknown, id: string): GoalProjectionState {
  if (isObject(value) && sortedKeys(value) === 'current,failure,seenGoalIds'
    && (value.current === null || (isObject(value.current)
      && sortedKeys(value.current) === 'createdAt,goal,roundsStarted,updatedAt' && isObject(value.current.goal)))
    && Array.isArray(value.seenGoalIds) && value.seenGoalIds.every(goalId => typeof goalId === 'string')
    && (value.failure === null || typeof value.failure === 'string')) {
    return value as unknown as GoalProjectionState
  }
  throw new Error(`${id}: invalid state ${JSON.stringify(value)}`)
}

function loadTable(): GoalCase[] {
  const table: unknown = JSON.parse(readFileSync(new URL('conformance/session/goal-cases.json', REPO), 'utf8'))
  if (!isObject(table) || sortedKeys(table) !== 'cases,history,logs,oracle,schema,version' || table.schema !== SCHEMA
    || table.version !== 1 || table.oracle !== ORACLE || !Array.isArray(table.cases)
    || !Array.isArray(table.history) || !table.history.every(isLine)
    || JSON.stringify(table.logs) !== JSON.stringify(Object.fromEntries(Object.entries(LOGS).map(([name, { path }]) => [name, path])))) {
    throw new Error('goal-cases.json does not match its version-1 schema')
  }
  return table.cases.map((entry: unknown): GoalCase => {
    if (!isObject(entry) || typeof entry.id !== 'string' || typeof entry.log !== 'string' || !Array.isArray(entry.edits)) {
      throw new Error(`invalid case ${JSON.stringify(entry)}`)
    }
    const { id } = entry
    const unknown = Object.keys(entry).filter(key => !['id', 'log', 'edits', 'ts', 'rust', 'note'].includes(key))
    if (unknown.length > 0) throw new Error(`${id}: unknown keys ${unknown.join()}`)
    if (entry.note !== undefined && typeof entry.note !== 'string') throw new Error(`${id}: invalid note`)
    const ts = parseState(entry.ts, id)
    const { rust } = entry
    if (rust !== undefined && !(isObject(rust) && sortedKeys(rust) === 'limit,outcome,seq'
      && rust.outcome === 'native-subset' && LIMITS.includes(rust.limit as string) && isCount(rust.seq))) {
      throw new Error(`${id}: invalid rust override ${JSON.stringify(rust)}`)
    }
    const { header, rows } = caseRows(entry.log, entry.edits, id)
    const { type: _type, ...meta } = JSON.parse(header) as SessionHeader & { type: string }
    return {
      id,
      header: meta,
      rows: rows.map(row => JSON.parse(row) as SessionEvent),
      ts,
      ...(rust === undefined ? {} : { rust: rust as NonNullable<GoalCase['rust']> }),
    }
  })
}

const cases = loadTable()

describe('shared goal projection cases', () => {
  it('read the unchanged capture and pin the table', () => {
    for (const { path, sha256 } of Object.values(LOGS)) {
      expect(createHash('sha256').update(readFileSync(new URL(path, REPO))).digest('hex'), path).toBe(sha256)
    }
    expect(cases).toHaveLength(CASE_COUNT)
    expect(new Set(cases.map(entry => entry.id)).size).toBe(CASE_COUNT)
    expect(new Set(cases.flatMap(entry => entry.rust === undefined ? [] : [entry.rust.limit]))).toEqual(new Set(LIMITS))
  })

  it('refuses malformed edits and states', () => {
    for (const edit of [{ truncate: 99 }, { row: 99, text: '{}' }, { append: 'a\nb' }, { tail: 'x' },
      { row: 0, text: '{}', extra: 1 }, { row: 0, find: 'a', replace: 'b' }]) {
      expect(() => parseEdit(edit, 16, 'malformed')).toThrow('invalid edit')
    }
    expect(() => parseState({ current: null, seenGoalIds: [] }, 'malformed')).toThrow('invalid state')
  })

  for (const entry of cases) {
    it(entry.id, () => {
      const closers = interruptedTurnClosers(entry.rows)
      const events = [...entry.rows, ...closers]
      // Session construction admits the rows the fold reads.
      Session.fromRestore(SessionId(entry.header.id), events, entry.header, SessionLogOffset(0), 'detached')
      let state = goalProjectionDefinition.init()
      for (const event of events) state = goalProjectionDefinition.apply(state, event)
      expect(state).toStrictEqual(entry.ts)
    })
  }
})
