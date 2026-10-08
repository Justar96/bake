/**
 * Runs the shared cases in `conformance/session/boundary-cases.json` through
 * `turnBoundaryProjectionDefinition` from `bake-agent-loop` or
 * `titleProjectionDefinition`, folding `init` and `apply` over each case's
 * `JSON.parse`d rows and then their `interruptedTurnClosers`, the events a
 * restored Session holds before its end seed. Session construction
 * (`Session.fromRestore`, without message projections) must admit the same
 * rows; the development Rust folds in `rust/crates/bake-session` check that
 * the full restoration path does. In `ts`, an absent `lastTurn` or `title`
 * is JavaScript's `undefined`, and `threw` names the error the fold throws.
 * A `rust` override names a native limit and the seq Rust refuses;
 * TypeScript still asserts its own outcome. Each case edits one runtime
 * capture as text.
 */

import { createHash } from 'node:crypto'
import { readFileSync } from 'node:fs'
import { describe, expect, it } from 'vitest'
import type { TurnBoundaryProjection } from 'bake-agent'
import { turnBoundaryProjectionDefinition } from 'bake-agent-loop'
import { Session, SessionId, SessionLogOffset, interruptedTurnClosers } from 'bake-session'
import type { SessionEvent, SessionHeader } from 'bake-session'
import { titleProjectionDefinition } from '../src/index.ts'

const REPO = new URL('../../../../', import.meta.url)
const SCHEMA = 'bake/session-conformance/boundary-cases'
const ORACLE = 'turnBoundaryProjectionDefinition in packages/core/agent-loop/src/index.ts and titleProjectionDefinition in packages/session/session-title/src/index.ts, init and apply folded over each case\'s parsed rows and their interruptedTurnClosers'
const LOGS: Record<string, { path: string; sha256: string }> = {
  'tool-call-turn': {
    path: 'conformance/runtime/request-reconstruction/tool-call-turn/session.jsonl',
    sha256: 'a7a8222990ef9f4c4f00a051c019de86d3156ca7f6c3c0d28af4be4cc3fbe657',
  },
  'dynamic-tools': {
    path: 'conformance/runtime/request-reconstruction/dynamic-tools/session.jsonl',
    sha256: '43852e686ea6ef5f599065a7ead57f82d27f8b20e9e936e81b0b0596636e61e2',
  },
}
/** Both harnesses pin the table size, so a dropped case fails. */
const CASE_COUNT = 52
const LIMITS = ['undefined-member', 'null-data', 'number']
const BOUNDARY_KEYS = 'lastStepBoundary,lastStepStartSeq,openTurnStartSeq'

type Edit =
  | { truncate: number }
  | { header: string }
  | { append: string }

type Fold = 'turnBoundary' | 'title'

/** A fold's value, or the error class it throws. */
type Outcome = { value: unknown } | { threw: 'TypeError' }

interface BoundaryCase {
  id: string
  fold: Fold
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

/**
 * Read an expected outcome, restoring the `undefined` an absent member
 * stands for. The second result names the limit Rust must report, if any.
 */
function parseOutcome(fold: Fold, value: unknown, id: string): [Outcome, string | undefined] {
  if (fold === 'turnBoundary' && isObject(value)) {
    const keys = sortedKeys(value)
    if (keys === BOUNDARY_KEYS) return [{ value: { ...value, lastTurn: undefined } }, 'undefined-member']
    if (keys === 'lastStepBoundary,lastStepStartSeq,lastTurn,openTurnStartSeq') return [{ value }, undefined]
  }
  if (fold === 'title' && isObject(value)) {
    const keys = sortedKeys(value)
    if (keys === 'title') return [{ value: value.title }, undefined]
    if (keys === '') return [{ value: undefined }, 'undefined-member']
    if (keys === 'threw' && value.threw === 'TypeError') return [{ threw: 'TypeError' }, 'null-data']
  }
  throw new Error(`${id}: invalid ${fold} outcome ${JSON.stringify(value)}`)
}

function loadTable(): BoundaryCase[] {
  const table: unknown = JSON.parse(readFileSync(new URL('conformance/session/boundary-cases.json', REPO), 'utf8'))
  if (!isObject(table) || sortedKeys(table) !== 'cases,history,logs,oracle,schema,version' || table.schema !== SCHEMA
    || table.version !== 1 || table.oracle !== ORACLE || !Array.isArray(table.cases)
    || !Array.isArray(table.history) || !table.history.every(isLine)
    || JSON.stringify(table.logs) !== JSON.stringify(Object.fromEntries(Object.entries(LOGS).map(([name, { path }]) => [name, path])))) {
    throw new Error('boundary-cases.json does not match its version-1 schema')
  }
  return table.cases.map((entry: unknown): BoundaryCase => {
    if (!isObject(entry) || typeof entry.id !== 'string' || typeof entry.log !== 'string' || !Array.isArray(entry.edits)
      || (entry.fold !== 'turnBoundary' && entry.fold !== 'title')) {
      throw new Error(`invalid case ${JSON.stringify(entry)}`)
    }
    const { id, fold } = entry
    const unknown = Object.keys(entry).filter(key => !['id', 'fold', 'log', 'edits', 'ts', 'rust', 'note'].includes(key))
    if (unknown.length > 0) throw new Error(`${id}: unknown keys ${unknown.join()}`)
    if (entry.note !== undefined && typeof entry.note !== 'string') throw new Error(`${id}: invalid note`)
    const [ts, required] = parseOutcome(fold, entry.ts, id)
    const { rust } = entry
    if (rust !== undefined && !(isObject(rust) && sortedKeys(rust) === 'limit,outcome,seq'
      && rust.outcome === 'native-subset' && LIMITS.includes(rust.limit as string) && isCount(rust.seq))) {
      throw new Error(`${id}: invalid rust override ${JSON.stringify(rust)}`)
    }
    if (required !== undefined && (rust === undefined || rust.limit !== required)) {
      throw new Error(`${id}: an undefined or thrown outcome needs the ${required} limit`)
    }
    const { header, rows } = caseLog(entry.log, entry.edits, id)
    const { type: _type, ...meta } = JSON.parse(header) as SessionHeader & { type: string }
    return {
      id,
      fold,
      header: meta,
      rows: rows.map(row => JSON.parse(row) as SessionEvent),
      ts,
      ...(rust === undefined ? {} : { rust: rust as NonNullable<BoundaryCase['rust']> }),
    }
  })
}

/** Fold one definition from `init` over the events. */
function fold(kind: Fold, events: readonly SessionEvent[]): unknown {
  if (kind === 'turnBoundary') {
    let state: TurnBoundaryProjection = turnBoundaryProjectionDefinition.init()
    for (const event of events) state = turnBoundaryProjectionDefinition.apply(state, event)
    return state
  }
  let state: string | null = titleProjectionDefinition.init()
  for (const event of events) state = titleProjectionDefinition.apply(state, event)
  return titleProjectionDefinition.wire.view(state)
}

const cases = loadTable()

describe('shared turn-boundary and title projection cases', () => {
  it('read the unchanged captures and pin the table', () => {
    for (const { path, sha256 } of Object.values(LOGS)) {
      expect(createHash('sha256').update(readFileSync(new URL(path, REPO))).digest('hex'), path).toBe(sha256)
    }
    expect(cases).toHaveLength(CASE_COUNT)
    expect(new Set(cases.map(entry => entry.id)).size).toBe(CASE_COUNT)
    expect(new Set(cases.flatMap(entry => entry.rust === undefined ? [] : [entry.rust.limit]))).toEqual(new Set(LIMITS))
    expect(new Set(cases.map(entry => entry.fold))).toEqual(new Set(['turnBoundary', 'title']))
  })

  it('refuses malformed edits and outcomes', () => {
    for (const edit of [{ truncate: 99 }, { append: 'a\nb' }, { header: 1 }, { tail: 'x' }, { row: 0, text: '{}' }]) {
      expect(() => parseEdit(edit, 16, 'malformed')).toThrow('invalid edit')
    }
    expect(() => parseOutcome('turnBoundary', { lastTurn: 1 }, 'malformed')).toThrow('invalid turnBoundary outcome')
    expect(() => parseOutcome('title', { threw: 'Error' }, 'malformed')).toThrow('invalid title outcome')
  })

  for (const entry of cases) {
    it(entry.id, () => {
      const closers = interruptedTurnClosers(entry.rows)
      const events = [...entry.rows, ...closers]
      const marker = entry.rows.findLast(event => event.type === 'session/end-seed'
        && (event.data as { inherited?: unknown }).inherited === true)
      const inherited = entry.header.isSeeded && marker !== undefined ? marker.seq : 0
      // Session construction admits the rows the folds read.
      Session.fromRestore(SessionId(entry.header.id), events, entry.header, SessionLogOffset(inherited), 'detached')
      if ('threw' in entry.ts) {
        expect(() => fold(entry.fold, events)).toThrow(TypeError)
      } else {
        expect(fold(entry.fold, events)).toStrictEqual(entry.ts.value)
      }
    })
  }
})
