/**
 * Runs the shared cases in `conformance/session/subagent-cases.json` through
 * `subagentIdentityProjectionDefinition` and
 * `subagentTimingProjectionDefinition`. Each fold starts from `init` and
 * applies each case's `JSON.parse`d rows, then their
 * `interruptedTurnClosers`: the events a restored Session holds before its
 * end seed. Session construction (`Session.fromRestore`, without message
 * projections) must admit the same rows. The development Rust folds in
 * `rust/crates/bake-session` check that the full restoration path does.
 *
 * `ts.timing` is the whole fold state and `ts.view` its wire view. An absent
 * `active` or `pendingTurnStart` means the state has no such member.
 * `ts.identity` is the identity wire view. Event times are safe integers,
 * but `settledMs` is JavaScript number arithmetic, so a total may leave the
 * safe range and round; both harnesses must reach the same value. Each case
 * edits one runtime capture as text.
 */

import { createHash } from 'node:crypto'
import { readFileSync } from 'node:fs'
import { describe, expect, it } from 'vitest'
import { Session, SessionId, SessionLogOffset, interruptedTurnClosers } from 'bake-session'
import type { SessionEvent, SessionHeader } from 'bake-session'
import {
  subagentIdentityProjectionDefinition,
  subagentTimingProjectionDefinition,
} from '../src/projection.ts'
import type { TimingState } from '../src/projection.ts'
// projection-types.ts declares the catalog view, whose state map entry is in
// catalog.ts; the conformance typecheck needs both declarations.
import type {} from '../src/catalog.ts'

const REPO = new URL('../../../../', import.meta.url)
const SCHEMA = 'bake/session-conformance/subagent-cases'
const ORACLE = 'subagentIdentityProjectionDefinition and subagentTimingProjectionDefinition in packages/subagent/subagent/src/projection.ts, folded from init over each case\'s parsed rows and their interruptedTurnClosers'
const LOGS: Record<string, { path: string; sha256: string }> = {
  'tool-call-turn': {
    path: 'conformance/runtime/request-reconstruction/tool-call-turn/session.jsonl',
    sha256: 'a7a8222990ef9f4c4f00a051c019de86d3156ca7f6c3c0d28af4be4cc3fbe657',
  },
}
/** Both harnesses pin the table size, so a dropped case fails. */
const CASE_COUNT = 84
/** The cases whose expected total is above `Number.MAX_SAFE_INTEGER`. */
const UNSAFE_TOTALS = [
  'length-over-max-safe',
  'promoted-pending-full-span',
  'total-over-max-safe-rounds',
  'overflow-in-closer',
]

type Edit =
  | { truncate: number }
  | { header: string }
  | { append: string }

interface Interval { since: number; through: number }

interface Expected {
  identity: unknown
  timing: TimingState
  view: { settledMs: number; active?: Interval }
}

interface SubagentCase {
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

function isTime(value: unknown): value is number {
  return Number.isSafeInteger(value) && !Object.is(value, -0)
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

function isInterval(value: unknown): value is Interval {
  return isObject(value) && sortedKeys(value) === 'since,through' && isTime(value.since) && isTime(value.through)
}

function isIdentity(value: unknown): boolean {
  if (value === null) return true
  if (!isObject(value) || !isCount(value.seq)) return false
  const keys = sortedKeys(value)
  if (value.mode === 'continuable') return keys === 'label,mode,seq' && typeof value.label === 'string'
  return value.mode === 'one-shot'
    && (keys === 'mode,seq' || (keys === 'label,mode,seq' && typeof value.label === 'string'))
}

/**
 * Read an expected outcome. `settledMs` is any nonnegative finite integer,
 * since a JavaScript total can leave the safe range; times stay safe.
 */
function parseExpected(value: unknown, id: string): Expected {
  const invalid = new Error(`${id}: invalid ts outcome ${JSON.stringify(value)}`)
  if (!isObject(value) || sortedKeys(value) !== 'identity,timing,view' || !isIdentity(value.identity)) throw invalid
  const { timing, view } = value
  if (!isObject(timing) || !isObject(view)) throw invalid
  const optional = ['active', 'pendingTurnStart'].filter(key => Object.hasOwn(timing, key))
  if (sortedKeys(timing) !== ['descriptorSeen', 'settledMs', ...optional].sort().join()
    || typeof timing.descriptorSeen !== 'boolean'
    || !(Number.isInteger(timing.settledMs) && (timing.settledMs as number) >= 0 && !Object.is(timing.settledMs, -0))
    || (Object.hasOwn(timing, 'active') && !isInterval(timing.active))
    || (Object.hasOwn(timing, 'pendingTurnStart') && !isTime(timing.pendingTurnStart))) {
    throw invalid
  }
  // The view is the state without its internal members.
  const derived = { settledMs: timing.settledMs, ...Object.hasOwn(timing, 'active') ? { active: timing.active } : {} }
  if (JSON.stringify(Object.entries(view).sort()) !== JSON.stringify(Object.entries(derived).sort())) throw invalid
  return value as unknown as Expected
}

function loadTable(): SubagentCase[] {
  const table: unknown = JSON.parse(readFileSync(new URL('conformance/session/subagent-cases.json', REPO), 'utf8'))
  if (!isObject(table) || sortedKeys(table) !== 'cases,history,logs,oracle,schema,version' || table.schema !== SCHEMA
    || table.version !== 1 || table.oracle !== ORACLE || !Array.isArray(table.cases)
    || !Array.isArray(table.history) || !table.history.every(isLine)
    || JSON.stringify(table.logs) !== JSON.stringify(Object.fromEntries(Object.entries(LOGS).map(([name, { path }]) => [name, path])))) {
    throw new Error('subagent-cases.json does not match its version-1 schema')
  }
  return table.cases.map(parseCase)
}

function parseCase(entry: unknown): SubagentCase {
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
  return {
    id,
    header: meta,
    rows: rows.map(row => JSON.parse(row) as SessionEvent),
    ts,
  }
}

/** Fold both definitions from `init` over the events. */
function fold(events: readonly SessionEvent[]): { identity: unknown; timing: TimingState; view: unknown } {
  let identity = subagentIdentityProjectionDefinition.init()
  let timing: TimingState = subagentTimingProjectionDefinition.init()
  for (const event of events) {
    identity = subagentIdentityProjectionDefinition.apply(identity, event)
    timing = subagentTimingProjectionDefinition.apply(timing, event)
  }
  return {
    identity: subagentIdentityProjectionDefinition.wire.view(identity),
    timing,
    view: subagentTimingProjectionDefinition.wire.view(timing),
  }
}

const cases = loadTable()

describe('shared subagent identity and timing projection cases', () => {
  it('read the unchanged capture and pin the table', () => {
    for (const { path, sha256 } of Object.values(LOGS)) {
      expect(createHash('sha256').update(readFileSync(new URL(path, REPO))).digest('hex'), path).toBe(sha256)
    }
    expect(cases).toHaveLength(CASE_COUNT)
    expect(new Set(cases.map(entry => entry.id)).size).toBe(CASE_COUNT)
    expect(cases.filter(entry => !Number.isSafeInteger(entry.ts.timing.settledMs)).map(entry => entry.id))
      .toEqual(UNSAFE_TOTALS)
  })

  it('refuses malformed edits and outcomes', () => {
    for (const edit of [{ truncate: 99 }, { truncate: -1 }, { append: 'a\nb' }, { header: 1 }, { tail: 'x' }]) {
      expect(() => parseEdit(edit, 16, 'malformed')).toThrow('invalid edit')
    }
    const view = { settledMs: 0 }
    const timing = { settledMs: 0, descriptorSeen: false }
    for (const outcome of [
      { identity: undefined, timing, view },
      { identity: { mode: 'continuable', seq: 1 }, timing, view },
      { identity: { mode: 'one-shot', label: 1, seq: 1 }, timing, view },
      { identity: { mode: 'one-shot', seq: -1 }, timing, view },
      { identity: null, timing: { settledMs: 0 }, view },
      { identity: null, timing: { ...timing, pendingTurnStart: 1.5 }, view },
      { identity: null, timing: { ...timing, active: { since: 1 } }, view },
      { identity: null, timing: { ...timing, extra: 1 }, view },
      { identity: null, timing: { ...timing, active: { since: 1, through: 2 } }, view },
      { identity: null, timing, view: { settledMs: 1 } },
      { identity: null, timing, view, extra: 1 },
    ]) {
      expect(() => parseExpected(outcome, 'malformed')).toThrow('invalid ts outcome')
    }
    for (const settledMs of [-1, 0.5, Infinity, -0]) {
      expect(() => parseExpected({ identity: null, timing: { ...timing, settledMs }, view: { settledMs } }, 'malformed'))
        .toThrow('invalid ts outcome')
    }
    const unsafe = { identity: null, timing: { ...timing, settledMs: 2 ** 60 }, view: { settledMs: 2 ** 60 } }
    expect(parseExpected(unsafe, 'unsafe')).toBe(unsafe)
    const rust = { outcome: 'native-subset', limit: 'timing-arithmetic', seq: 1 }
    expect(() => parseCase({ id: 'override', log: 'tool-call-turn', edits: [], ts: unsafe, rust }))
      .toThrow('override: unknown keys rust')
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
      expect(fold(events)).toStrictEqual(entry.ts)
    })
  }
})
