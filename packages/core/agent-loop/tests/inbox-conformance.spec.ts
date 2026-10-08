/**
 * Runs the shared inbox cases in `conformance/session/inbox-cases.json`
 * through `inboxProjectionDefinition.apply`, from `init()`, and
 * `foldConsumedWork`, over the events `restorePlainLog` restores: the stored
 * events, the closers, and any appended end seed, as the restored Session
 * holds them. The development Rust folds in `rust/crates/bake-session` check
 * the same table. `restorePlainLog` and `caseLog` are local copies of the
 * restore conformance spec's helpers, except that this package does not
 * depend on the format catalog's message projections, so the Session is
 * restored without them; they change derived messages only, and no case logs
 * an `image/offload`. A `rust` override names a native limit and its seq;
 * TypeScript still asserts its own outcome.
 */

import { createHash } from 'node:crypto'
import { readFileSync } from 'node:fs'
import { describe, expect, it } from 'vitest'
import { foldConsumedWork } from 'bake-agent'
import type { InboxState } from 'bake-agent'
import { Session, SessionId, SessionLogOffset, interruptedTurnClosers } from 'bake-session'
import type { SessionEvent } from 'bake-session'
import { validateStoredEvents } from 'bake-session-persistence'
import { scanLog } from 'bake-session-persistence-jsonl/src/format.ts'
import { inboxProjectionDefinition } from '../src/inbox.ts'

const REPO = new URL('../../../../', import.meta.url)
const SCHEMA = 'bake/session-conformance/inbox-cases'
const ORACLE = 'inboxProjectionDefinition.apply from init() and foldConsumedWork over the events restorePlainLog(log) restores in packages/core/agent-loop/tests/inbox-conformance.spec.ts'
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
const CASE_COUNT = 63
const LIMITS = {
  inbox: ['target', 'count', 'inserted', 'message-id'],
  consumedWork: ['data', 'turn', 'inserted', 'reason'],
} as const

type Fold = keyof typeof LIMITS

type Edit =
  | { truncate: number }
  | { header: string }
  | { append: string }
  | { tail: string }
  | { row: number; text: string }
  | { row: number; find: string; replace: string }

type InboxOutcome =
  | { outcome: 'pending'; 'next-turn': unknown[]; 'next-step': unknown[] }
  | { outcome: 'rejected'; message: string }

type ConsumedWorkOutcome =
  | { outcome: 'folded'; end?: unknown; droppedUnrun: boolean }
  | { outcome: 'rejected'; class: 'TypeError' }

interface RustLimit { outcome: 'native-subset'; limit: string; seq: number }

interface InboxCase {
  id: string
  log: Buffer
  /** The edited rows as `JSON.parse` reads them, by seq, for `$log` references. */
  rows: unknown[]
  ts: { inbox: InboxOutcome; consumedWork: ConsumedWorkOutcome }
  rust: Partial<Record<Fold, RustLimit>>
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

/**
 * Restore a plain current-format log as `readColdSessionLog` and
 * `SessionStore.prepare` do, without message projections.
 * @param log - one complete plain log.
 * @returns the restored Session's events and the closers among them.
 */
function restorePlainLog(log: Buffer): { events: readonly SessionEvent[]; closers: SessionEvent[] } {
  const { meta, inheritedEventCount, events } = scanLog(log)
  validateStoredEvents(meta, events)
  const closers = interruptedTurnClosers(events)
  const session = Session.fromRestore(SessionId(meta.id), [...events, ...closers], meta,
    SessionLogOffset(inheritedEventCount), 'detached')
  const restored = session.snapshotEvents()
  expect(restored.slice(0, events.length + closers.length)).toStrictEqual([...events, ...closers])
  expect(restored.slice(events.length + closers.length).every(event => event.type === 'session/end-seed')).toBe(true)
  return { events: restored, closers }
}

/** Apply text edits to one capture's header, rows, and tail; a find must match exactly once. */
function caseLog(name: string, edits: Edit[], id: string): { log: Buffer; rows: string[] } {
  const source = LOGS[name]
  if (source === undefined) throw new Error(`${id}: unknown log ${name}`)
  const text = readFileSync(new URL(source.path, REPO), 'utf8')
  const lines = text.slice(0, -1).split('\n')
  let header = lines[0] as string
  let rows = lines.slice(1)
  let tail = ''
  for (const edit of edits) {
    if ('truncate' in edit) {
      rows = rows.slice(0, edit.truncate)
    } else if ('header' in edit) {
      header = edit.header
    } else if ('append' in edit) {
      rows.push(edit.append)
    } else if ('tail' in edit) {
      tail = edit.tail
    } else if ('find' in edit) {
      const row = rows[edit.row] as string
      if (row.split(edit.find).length !== 2) throw new Error(`${id}: find must match row ${edit.row} once`)
      rows[edit.row] = row.replace(edit.find, () => edit.replace)
    } else {
      rows[edit.row] = edit.text
    }
  }
  return { log: Buffer.from([header, ...rows].map(line => `${line}\n`).join('') + tail), rows }
}

function parseEdit(value: unknown, rows: number, id: string): Edit {
  const row = (entry: Record<string, unknown>) => isCount(entry.row) && entry.row < rows
  if (isObject(value)) {
    const keys = sortedKeys(value)
    if (keys === 'truncate' && isCount(value.truncate) && value.truncate <= rows) return value as Edit
    if ((keys === 'header' || keys === 'append' || keys === 'tail') && isLine(Object.values(value)[0])) return value as Edit
    if (keys === 'row,text' && row(value) && isLine(value.text)) return value as Edit
    if (keys === 'find,replace,row' && row(value) && isLine(value.find) && value.find !== '' && isLine(value.replace)) {
      return value as Edit
    }
  }
  throw new Error(`${id}: invalid edit ${JSON.stringify(value)}`)
}

function parseOutcomes(value: unknown, id: string): InboxCase['ts'] {
  if (isObject(value) && sortedKeys(value) === 'consumedWork,inbox') {
    const { inbox, consumedWork } = value
    const inboxValid = isObject(inbox) && (
      (sortedKeys(inbox) === 'next-step,next-turn,outcome' && inbox.outcome === 'pending'
        && Array.isArray(inbox['next-turn']) && Array.isArray(inbox['next-step']))
      || (sortedKeys(inbox) === 'message,outcome' && inbox.outcome === 'rejected' && typeof inbox.message === 'string'))
    const workValid = isObject(consumedWork) && (
      (['droppedUnrun,outcome', 'droppedUnrun,end,outcome'].includes(sortedKeys(consumedWork))
        && consumedWork.outcome === 'folded' && typeof consumedWork.droppedUnrun === 'boolean')
      || (sortedKeys(consumedWork) === 'class,outcome' && consumedWork.outcome === 'rejected'
        && consumedWork.class === 'TypeError'))
    if (inboxValid && workValid) return value as InboxCase['ts']
  }
  throw new Error(`${id}: invalid outcome ${JSON.stringify(value)}`)
}

function parseRust(value: unknown, id: string): InboxCase['rust'] {
  if (value === undefined) return {}
  if (isObject(value) && Object.keys(value).length > 0 && Object.entries(value).every(([fold, limit]) =>
    Object.hasOwn(LIMITS, fold) && isObject(limit) && sortedKeys(limit) === 'limit,outcome,seq'
    && limit.outcome === 'native-subset' && (LIMITS[fold as Fold] as readonly string[]).includes(limit.limit as string)
    && isCount(limit.seq))) {
    return value as InboxCase['rust']
  }
  throw new Error(`${id}: invalid rust override ${JSON.stringify(value)}`)
}

function loadTable(): InboxCase[] {
  const table: unknown = JSON.parse(readFileSync(new URL('conformance/session/inbox-cases.json', REPO), 'utf8'))
  if (!isObject(table) || sortedKeys(table) !== 'cases,logs,oracle,schema,version' || table.schema !== SCHEMA
    || table.version !== 1 || table.oracle !== ORACLE || !Array.isArray(table.cases)
    || JSON.stringify(table.logs) !== JSON.stringify(Object.fromEntries(Object.entries(LOGS).map(([name, { path }]) => [name, path])))) {
    throw new Error('inbox-cases.json does not match its version-1 schema')
  }
  return table.cases.map((entry: unknown): InboxCase => {
    if (!isObject(entry) || typeof entry.id !== 'string' || typeof entry.log !== 'string') {
      throw new Error(`invalid case ${JSON.stringify(entry)}`)
    }
    const { id } = entry
    const unknown = Object.keys(entry).filter(key => !['id', 'log', 'edits', 'ts', 'rust', 'note'].includes(key))
    if (unknown.length > 0) throw new Error(`${id}: unknown keys ${unknown.join()}`)
    if (!Array.isArray(entry.edits) || (entry.note !== undefined && typeof entry.note !== 'string')) {
      throw new Error(`${id}: invalid case fields`)
    }
    const edits: Edit[] = []
    for (const value of entry.edits) {
      const rows = caseLog(entry.log, edits, id).rows.length
      edits.push(parseEdit(value, rows, id))
    }
    const { log, rows } = caseLog(entry.log, edits, id)
    const ts = parseOutcomes(entry.ts, id)
    if (ts.consumedWork.outcome === 'rejected' && entry.rust?.consumedWork === undefined) {
      throw new Error(`${id}: a TypeError names its Rust limit`)
    }
    return {
      id,
      log,
      rows: rows.map((row) => { try { return JSON.parse(row) as unknown } catch { return undefined } }),
      ts,
      rust: parseRust(entry.rust, id),
    }
  })
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
function resolve(value: unknown, entry: InboxCase, closers: unknown[]): unknown {
  if (Array.isArray(value)) return value.map(item => resolve(item, entry, closers))
  if (!isObject(value)) return value
  const keys = sortedKeys(value)
  if (keys === '$log' && typeof value.$log === 'string') return at(entry.rows, value.$log, entry.id)
  if (keys === '$closer' && typeof value.$closer === 'string') return at(closers, value.$closer, entry.id)
  if (keys.includes('$')) throw new Error(`${entry.id}: invalid reference ${JSON.stringify(value)}`)
  return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, resolve(item, entry, closers)]))
}

/** The inbox fold's outcome, as the projection registry would fold it from `init()`. */
function foldInbox(events: readonly SessionEvent[]): InboxOutcome {
  let state: InboxState = inboxProjectionDefinition.init()
  try {
    for (const event of events) state = inboxProjectionDefinition.apply(state, event)
  } catch (error) {
    if (!(error instanceof Error) || error.constructor !== Error) throw error
    return { outcome: 'rejected', message: error.message }
  }
  return { outcome: 'pending', 'next-turn': [...state['next-turn']], 'next-step': [...state['next-step']] }
}

/** `foldConsumedWork`'s outcome; only a `TypeError` counts as its rejection. */
function foldWork(events: readonly SessionEvent[]): ConsumedWorkOutcome {
  try {
    return { outcome: 'folded', ...foldConsumedWork(events) }
  } catch (error) {
    if (!(error instanceof TypeError)) throw error
    return { outcome: 'rejected', class: 'TypeError' }
  }
}

const cases = loadTable()

describe('shared inbox cases', () => {
  it('read the unchanged captures and pin the table', () => {
    for (const { path, sha256 } of Object.values(LOGS)) {
      expect(createHash('sha256').update(readFileSync(new URL(path, REPO))).digest('hex'), path).toBe(sha256)
    }
    expect(cases).toHaveLength(CASE_COUNT)
    expect(new Set(cases.map(entry => entry.id)).size).toBe(CASE_COUNT)
    for (const fold of ['inbox', 'consumedWork'] as const) {
      const limits = new Set(cases.flatMap(entry => entry.rust[fold] === undefined ? [] : [entry.rust[fold].limit]))
      expect(limits).toEqual(new Set(LIMITS[fold]))
    }
    expect(cases.some(entry => entry.ts.inbox.outcome === 'rejected')).toBe(true)
  })

  it('refuses malformed edits, outcomes, and overrides', () => {
    expect(() => parseEdit({ truncate: 99 }, 16, 'malformed')).toThrow('invalid edit')
    expect(() => parseOutcomes({ inbox: { outcome: 'pending' }, consumedWork: { outcome: 'folded', droppedUnrun: false } },
      'malformed')).toThrow('invalid outcome')
    expect(() => parseRust({ inbox: { outcome: 'native-subset', limit: 'data', seq: 1 } }, 'malformed'))
      .toThrow('invalid rust override')
    expect(() => resolve({ $log: '/99/data' }, cases[0] as InboxCase, [])).toThrow('does not exist')
  })

  for (const entry of cases) {
    it(entry.id, () => {
      expect(entry.log.includes('image/offload')).toBe(false)
      const { events, closers } = restorePlainLog(entry.log)
      expect(foldInbox(events)).toStrictEqual(resolve(entry.ts.inbox, entry, closers))
      expect(foldWork(events)).toStrictEqual(resolve(entry.ts.consumedWork, entry, closers))
    })
  }
})
