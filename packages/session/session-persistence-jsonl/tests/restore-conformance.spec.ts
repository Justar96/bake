/**
 * Runs the shared restore cases in `conformance/session/restore-cases.json`
 * through `restorePlainLog`, which composes the production read path for a
 * stored current-format plain log: `scanLog`, `validateStoredEvents`,
 * `interruptedTurnClosers`, then `Session.fromRestore` with the catalog's
 * message projections, as `readColdSessionLog` and `SessionStore.prepare`
 * do. The development Rust restoration in `rust/crates/bake-session` checks
 * the same table. Cases marked `production` also run through the JSONL
 * backend and `readColdSessionLog` in an owned temporary root, so the helper
 * cannot drift from the path it reproduces. A `rust` override names a native
 * limit, or the internal cause Rust reports for a rejection; TypeScript still
 * asserts its own outcome. Refusal classes and messages belong to this
 * helper; the file backend adds path context and wraps scan errors as
 * SessionPersistenceCorruptionError. Each case edits one capture as text.
 */

import { createHash } from 'node:crypto'
import { readFileSync } from 'node:fs'
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { dirname, join } from 'node:path'
import { afterAll, beforeAll, describe, expect, it } from 'vitest'
import { Context } from '@deepseek-ai/cordis'
import { Session, SessionId, SessionLogOffset, interruptedTurnClosers } from 'bake-session'
import type { SessionEvent } from 'bake-session'
import { currentSessionMessageProjections } from 'bake-session-format-catalog/message-projections'
import { validateStoredEvents } from 'bake-session-persistence'
import { readColdSessionLog } from 'bake-session-query'
import JsonlSessionPersistence from '../src/index.ts'
import { logPath, scanLog } from '../src/format.ts'

const REPO = new URL('../../../../', import.meta.url)
const SCHEMA = 'bake/session-conformance/restore-cases'
const ORACLE = 'restorePlainLog(log) in packages/session/session-persistence-jsonl/tests/restore-conformance.spec.ts: scanLog, validateStoredEvents, interruptedTurnClosers, then Session.fromRestore(..., "detached", currentSessionMessageProjections)'
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
const CASE_COUNT = 57
const CLASSES = ['Error', 'TypeError', 'SessionFormatUnsupportedError', 'SessionPersistenceCorruptionError']
const LIMITS = [
  'event-type', 'ignorable', 'number', 'depth', 'coordinate', 'config-member', 'tool-schema', 'context', 'repair',
]
/** The `assertMessageEventShape`, tool-update data, and marker checks adoption runs. */
const STORED_CHECKS = [
  'message-identity', 'message-role', 'message-source', 'message-content', 'model-source', 'tool-source',
  'tool-result-block', 'tool-call-id', 'tool-update-data', 'non-surface-marker',
]
/** Session construction's checks; restoration takes no lossless snapshot. */
const RESTORE_CHECKS = [
  ...STORED_CHECKS, 'settlement', 'header-provider-model', 'header-reasoning-effort', 'header-adapter-defaults',
  'header-reason', 'header-starts-series', 'replace-start', 'replace-end', 'replace-order', 'replace-sources',
  'tool-result-span', 'tool-result-target', 'tool-result-rest', 'system-head', 'tool-update-header',
  'tool-update-stale', 'tool-update-baseline', 'tool-update-change', 'tool-update-anchor',
]
const CAUSES = [
  'scan', 'unsupported/unknown-type', 'unsupported/fallback-header',
  ...STORED_CHECKS.map(check => `stored/${check}`), ...RESTORE_CHECKS.map(check => `restore/${check}`),
]
/** Every refusal layer a case must witness. */
const LAYERS = ['scan', 'unsupported/unknown-type', 'unsupported/fallback-header', 'stored', 'restore']
const RESTORED_KEYS = [
  'outcome', 'header', 'inheritedEventCount', 'committedBytes', 'storedEventCount', 'closers',
  'endSeedAppended', 'messages', 'requestHeader', 'toolHistory', 'requestContext',
]

type Json = null | boolean | number | string | Json[] | { [key: string]: Json }

type Edit =
  | { truncate: number }
  | { header: string }
  | { append: string }
  | { tail: string }
  | { row: number; text: string }
  | { row: number; find: string; replace: string }

interface Restored {
  outcome: 'restored'
  header: Json
  inheritedEventCount: number
  committedBytes: number
  storedEventCount: number
  closers: Json[]
  endSeedAppended: boolean
  messages: Json[]
  requestHeader: Json
  toolHistory: Json
  requestContext: Json
  /** Decoded `sourceEventSeqs` of chosen stored events. */
  events?: { seq: number; sourceEventSeqs: number[] }[]
}

type Outcome = Restored | { outcome: 'rejected'; class: string; message?: string }

type RustOverride = { outcome: 'native-subset'; limit: string } | { outcome: 'rejected'; cause: string }

interface RestoreCase {
  id: string
  log: Buffer
  /** The edited rows as `JSON.parse` reads them, by seq, for `$log` references. */
  rows: unknown[]
  ts: Outcome
  rust?: RustOverride
  production: boolean
}

/** What `restorePlainLog` returns; the rows are the stored events, not compared whole. */
interface RestoreResult {
  header: unknown
  inheritedEventCount: number
  committedBytes: number
  storedEventCount: number
  closers: SessionEvent[]
  endSeedAppended: boolean
  messages: unknown[]
  requestHeader: unknown
  toolHistory: unknown
  requestContext: unknown
  events: SessionEvent[]
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
 * Restore a plain current-format log the way production reads one:
 * `readColdSessionLog` appends the closers to what the JSONL backend's
 * `scanLog` and `validateStoredEvents` admit, and `SessionStore.prepare`
 * restores them with `Session.fromRestore`.
 * @param log - one complete plain log, possibly with a torn tail.
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
    events,
  }
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

function parseOutcome(value: unknown, id: string): Outcome {
  if (isObject(value) && value.outcome === 'rejected' && CLASSES.includes(value.class as string)) {
    // Only a `TypeError`, whose text is the engine's, may omit its message.
    if (sortedKeys(value) === 'class,message,outcome' && typeof value.message === 'string') return value as Outcome
    if (sortedKeys(value) === 'class,outcome' && value.class === 'TypeError') return value as Outcome
  }
  if (isObject(value) && value.outcome === 'restored'
    && Object.keys(value).every(key => RESTORED_KEYS.includes(key) || key === 'events')
    && RESTORED_KEYS.every(key => Object.hasOwn(value, key))
    && isCount(value.inheritedEventCount) && isCount(value.committedBytes) && isCount(value.storedEventCount)
    && Array.isArray(value.closers) && typeof value.endSeedAppended === 'boolean' && Array.isArray(value.messages)
    && (value.events === undefined || (Array.isArray(value.events) && value.events.every(event =>
      isObject(event) && sortedKeys(event) === 'seq,sourceEventSeqs' && isCount(event.seq)
      && Array.isArray(event.sourceEventSeqs) && event.sourceEventSeqs.every(isCount))))) {
    return value as unknown as Outcome
  }
  throw new Error(`${id}: invalid outcome ${JSON.stringify(value)}`)
}

function parseRust(value: unknown, ts: Outcome, id: string): RustOverride {
  if (isObject(value) && value.outcome === 'native-subset' && sortedKeys(value) === 'limit,outcome'
    && LIMITS.includes(value.limit as string)) return value as RustOverride
  if (isObject(value) && value.outcome === 'rejected' && sortedKeys(value) === 'cause,outcome'
    && CAUSES.includes(value.cause as string) && ts.outcome === 'rejected') return value as RustOverride
  throw new Error(`${id}: invalid rust override ${JSON.stringify(value)}`)
}

function loadTable(): RestoreCase[] {
  const table: unknown = JSON.parse(readFileSync(new URL('conformance/session/restore-cases.json', REPO), 'utf8'))
  if (!isObject(table) || sortedKeys(table) !== 'cases,logs,oracle,schema,version' || table.schema !== SCHEMA
    || table.version !== 1 || table.oracle !== ORACLE || !Array.isArray(table.cases)
    || JSON.stringify(table.logs) !== JSON.stringify(Object.fromEntries(Object.entries(LOGS).map(([name, { path }]) => [name, path])))) {
    throw new Error('restore-cases.json does not match its version-1 schema')
  }
  return table.cases.map((entry: unknown): RestoreCase => {
    if (!isObject(entry) || typeof entry.id !== 'string' || typeof entry.log !== 'string') {
      throw new Error(`invalid case ${JSON.stringify(entry)}`)
    }
    const { id } = entry
    const unknown = Object.keys(entry).filter(key => !['id', 'log', 'edits', 'ts', 'rust', 'production', 'note'].includes(key))
    if (unknown.length > 0) throw new Error(`${id}: unknown keys ${unknown.join()}`)
    if (!Array.isArray(entry.edits) || (entry.production !== undefined && entry.production !== true)
      || (entry.note !== undefined && typeof entry.note !== 'string')) throw new Error(`${id}: invalid case fields`)
    const edits: Edit[] = []
    for (const value of entry.edits) {
      const rows = caseLog(entry.log, edits, id).rows.length
      edits.push(parseEdit(value, rows, id))
    }
    const { log, rows } = caseLog(entry.log, edits, id)
    const ts = parseOutcome(entry.ts, id)
    if (ts.outcome === 'rejected' && entry.rust === undefined) throw new Error(`${id}: a rejection names its Rust cause`)
    if (entry.production === true && ts.outcome !== 'restored') throw new Error(`${id}: a production case restores`)
    const rust = entry.rust === undefined ? undefined : parseRust(entry.rust, ts, id)
    return {
      id,
      log,
      rows: rows.map((row) => { try { return JSON.parse(row) as unknown } catch { return undefined } }),
      ts,
      ...(rust === undefined ? {} : { rust }),
      production: entry.production === true,
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
function resolve(value: unknown, entry: RestoreCase, closers: unknown[]): unknown {
  if (Array.isArray(value)) return value.map(item => resolve(item, entry, closers))
  if (!isObject(value)) return value
  const keys = sortedKeys(value)
  if (keys === '$log' && typeof value.$log === 'string') return at(entry.rows, value.$log, entry.id)
  if (keys === '$closer' && typeof value.$closer === 'string') return at(closers, value.$closer, entry.id)
  if (keys.includes('$')) throw new Error(`${entry.id}: invalid reference ${JSON.stringify(value)}`)
  return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, resolve(item, entry, closers)]))
}

/** The independent expectation with references resolved, in `RestoreResult` form without its events. */
function expected(entry: RestoreCase & { ts: Restored }): Omit<RestoreResult, 'events'> {
  const { outcome: _outcome, events: _events, ...rest } = entry.ts
  const closers = resolve(rest.closers, entry, []) as unknown[]
  return { ...resolve(rest, entry, closers) as Omit<RestoreResult, 'events'>, closers: closers as SessionEvent[] }
}

const cases = loadTable()
let root: string

beforeAll(async () => {
  root = await mkdtemp(join(tmpdir(), 'bake-restore-conformance-'))
})

afterAll(async () => {
  await rm(root, { recursive: true, force: true })
})

/**
 * Read the log through the JSONL backend and `readColdSessionLog` in a fresh
 * root, then restore it as `SessionStore.prepare` does. The Context owns the
 * backend and is disposed before returning.
 */
async function restoreThroughBackend(entry: RestoreCase) {
  const id = SessionId((JSON.parse(entry.log.toString('utf8').split('\n')[0] as string) as { id: string }).id)
  const caseRoot = await mkdtemp(join(root, 'case-'))
  const ctx = new Context()
  try {
    await ctx.plugin(JsonlSessionPersistence, { root: caseRoot, compression: 'none' })
    const path = logPath(caseRoot, undefined, id, 'none')
    await mkdir(dirname(path), { recursive: true })
    await writeFile(path, entry.log)
    const cold = await readColdSessionLog(ctx.sessionPersistence, id)
    const session = Session.fromRestore(id, cold.events, cold.header, cold.inheritedEventCount, cold.eventState,
      currentSessionMessageProjections)
    return {
      header: cold.header,
      inheritedEventCount: cold.inheritedEventCount,
      events: cold.events,
      endSeedAppended: session.seq > cold.events.length,
      messages: session.deriveMessages(),
      requestHeader: session.requestHeader() ?? null,
      toolHistory: session.toolHistory(),
      requestContext: session.requestContext() ?? null,
    }
  } finally {
    await ctx.fiber.dispose()
  }
}

describe('shared restore cases', () => {
  it('read the unchanged captures and pin the table', () => {
    for (const { path, sha256 } of Object.values(LOGS)) {
      expect(createHash('sha256').update(readFileSync(new URL(path, REPO))).digest('hex'), path).toBe(sha256)
    }
    expect(cases).toHaveLength(CASE_COUNT)
    expect(new Set(cases.map(entry => entry.id)).size).toBe(CASE_COUNT)
    const limits = new Set(cases.flatMap(entry => entry.rust?.outcome === 'native-subset' ? [entry.rust.limit] : []))
    expect(limits).toEqual(new Set(LIMITS))
    const layers = new Set(cases.flatMap(entry =>
      entry.rust?.outcome === 'rejected' ? [entry.rust.cause.replace(/^(stored|restore)\/.*/, '$1')] : []))
    expect(layers).toEqual(new Set(LAYERS))
    expect(cases.filter(entry => entry.production).length).toBeGreaterThanOrEqual(6)
  })

  it('refuses malformed edits, outcomes, and references', () => {
    for (const edit of [{ truncate: 99 }, { row: 99, text: '{}' }, { row: 0, find: '', replace: 'x' },
      { append: 'a\nb' }, { row: 0, text: '{}', extra: 1 }]) {
      expect(() => parseEdit(edit, 16, 'malformed')).toThrow('invalid edit')
    }
    expect(() => parseOutcome({ outcome: 'rejected', class: 'Error' }, 'malformed')).toThrow('invalid outcome')
    expect(() => caseLog('tool-call-turn', [{ row: 8, find: '"id":"c1"', replace: '' }], 'twice')).toThrow('once')
    const entry = cases[0] as RestoreCase
    expect(() => resolve({ $log: '/99/data' }, entry, [])).toThrow('does not exist')
    expect(() => resolve({ $log: '/4/data', extra: 1 }, entry, [])).toThrow('invalid reference')
  })

  for (const entry of cases) {
    it(entry.id, async () => {
      let result: RestoreResult
      try {
        result = restorePlainLog(entry.log)
      } catch (error) {
        if (!(error instanceof Error)) throw error
        const { ts } = entry
        if (ts.outcome !== 'rejected') throw error
        expect(error.constructor.name).toBe(ts.class)
        if (ts.message !== undefined) expect(error.message).toBe(ts.message)
        return
      }
      const { ts } = entry
      if (ts.outcome !== 'restored') throw new Error(`${entry.id}: restored, expected ${ts.class}: ${ts.message}`)
      const { events, ...actual } = result
      const want = expected({ ...entry, ts })
      expect(actual).toStrictEqual(want)
      for (const { seq, sourceEventSeqs } of ts.events ?? []) {
        expect(events[seq]?.sourceEventSeqs, `event ${seq}`).toStrictEqual(sourceEventSeqs)
      }
      if (!entry.production) return
      // The backend's view, checked against the same independent expectation.
      const production = await restoreThroughBackend(entry)
      expect(production.header).toStrictEqual(want.header)
      expect(production.inheritedEventCount).toBe(want.inheritedEventCount)
      expect(production.events.slice(0, want.storedEventCount)).toStrictEqual(events)
      expect(production.events.slice(want.storedEventCount)).toStrictEqual(want.closers)
      expect({
        endSeedAppended: production.endSeedAppended,
        messages: production.messages,
        requestHeader: production.requestHeader,
        toolHistory: production.toolHistory,
        requestContext: production.requestContext,
      }).toStrictEqual({
        endSeedAppended: want.endSeedAppended,
        messages: want.messages,
        requestHeader: want.requestHeader,
        toolHistory: want.toolHistory,
        requestContext: want.requestContext,
      })
    })
  }
})
