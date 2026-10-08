/**
 * Runs the shared fork cases in `conformance/session/fork-cases.json` through
 * `SessionStore.fork`. Each case edits one capture as text, and its source is
 * restored as `SessionStore.prepare` restores a stored log: the parsed rows,
 * with packed `sourceEventSeqs` ranges expanded, then `interruptedTurnClosers`,
 * passed to `Session.fromRestore` with `eventState: 'detached'` and no message
 * projections, so a case holds no `image/offload` row. The development Rust
 * `fork_seed` in `rust/crates/bake-session` checks the same table after
 * `restore_plain_log`. A `rust` override names a native limit or a boundary
 * a `u64` cannot spell; TypeScript still asserts its own outcome.
 *
 * The appended end seed's time is the clock's, so it is checked against the
 * clock read before and after the source is restored, and against the
 * source's own event, rather than against the table.
 */

import { createHash } from 'node:crypto'
import { readFileSync } from 'node:fs'
import { afterEach, describe, expect, it } from 'vitest'
import { Context } from '@deepseek-ai/cordis'
import SessionStore, {
  decodeSeqRanges, interruptedTurnClosers, SessionForkError, SessionId, SessionLogOffset,
} from 'bake-session'
import type { Session, SessionEvent, SessionHeader, SessionSeq } from 'bake-session'

const REPO = new URL('../../../../', import.meta.url)
const SCHEMA = 'bake/session-conformance/fork-cases'
const ORACLE = 'SessionStore.fork(source, boundary) in packages/core/session/src/index.ts over a source restored as SessionStore.prepare does: the decoded rows and interruptedTurnClosers passed to Session.fromRestore(..., "detached") with no message projections'
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
const CASE_COUNT = 48
const LIMITS = ['turn-diagnostic']
const CODES = ['INVALID_BOUNDARY', 'OPEN_TURN']

type Edit =
  | { truncate: number }
  | { header: string }
  | { append: string }
  | { row: number; text: string }
  | { row: number; find: string; replace: string }

type Outcome =
  | { outcome: 'forked'; stored: number; closers: number; endSeed: boolean }
  | { outcome: 'rejected'; class: 'SessionForkError'; code: string; message: string }
  | { outcome: 'rejected'; class: 'Error'; message: string }

type RustOverride = { outcome: 'native-subset'; limit: string } | { outcome: 'unrepresentable' }

interface ForkCase {
  id: string
  header: string
  rows: string[]
  boundary?: number
  ts: Outcome
  rust?: RustOverride
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

/** Apply text edits to one capture's header and rows; a find must match exactly once. */
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
    if ((keys === 'header' || keys === 'append') && isLine(Object.values(value)[0])) return value as Edit
    if (keys === 'row,text' && row(value) && isLine(value.text)) return value as Edit
    if (keys === 'find,replace,row' && row(value) && isLine(value.find) && value.find !== '' && isLine(value.replace)) {
      return value as Edit
    }
  }
  throw new Error(`${id}: invalid edit ${JSON.stringify(value)}`)
}

function parseOutcome(value: unknown, id: string): Outcome {
  if (isObject(value) && value.outcome === 'forked' && sortedKeys(value) === 'closers,endSeed,outcome,stored'
    && isCount(value.stored) && isCount(value.closers) && typeof value.endSeed === 'boolean') return value as Outcome
  if (isObject(value) && value.outcome === 'rejected' && typeof value.message === 'string') {
    if (value.class === 'SessionForkError' && sortedKeys(value) === 'class,code,message,outcome'
      && CODES.includes(value.code as string)) return value as Outcome
    if (value.class === 'Error' && sortedKeys(value) === 'class,message,outcome') return value as Outcome
  }
  throw new Error(`${id}: invalid outcome ${JSON.stringify(value)}`)
}

function parseRust(value: unknown, id: string): RustOverride {
  if (isObject(value) && value.outcome === 'native-subset' && sortedKeys(value) === 'limit,outcome'
    && LIMITS.includes(value.limit as string)) return value as RustOverride
  if (isObject(value) && value.outcome === 'unrepresentable' && sortedKeys(value) === 'outcome') return value as RustOverride
  throw new Error(`${id}: invalid rust override ${JSON.stringify(value)}`)
}

function loadTable(): ForkCase[] {
  const table: unknown = JSON.parse(readFileSync(new URL('conformance/session/fork-cases.json', REPO), 'utf8'))
  if (!isObject(table) || sortedKeys(table) !== 'cases,logs,oracle,provenance,schema,version'
    || table.schema !== SCHEMA || table.version !== 1 || table.oracle !== ORACLE
    || typeof table.provenance !== 'string' || !Array.isArray(table.cases)
    || JSON.stringify(table.logs) !== JSON.stringify(Object.fromEntries(Object.entries(LOGS).map(([name, { path }]) => [name, path])))) {
    throw new Error('fork-cases.json does not match its version-1 schema')
  }
  return table.cases.map((entry: unknown): ForkCase => {
    if (!isObject(entry) || typeof entry.id !== 'string' || typeof entry.log !== 'string' || !Array.isArray(entry.edits)) {
      throw new Error(`invalid case ${JSON.stringify(entry)}`)
    }
    const { id } = entry
    const unknown = Object.keys(entry).filter(key => !['id', 'log', 'edits', 'boundary', 'ts', 'rust', 'note'].includes(key))
    if (unknown.length > 0) throw new Error(`${id}: unknown keys ${unknown.join()}`)
    if ((entry.boundary !== undefined && typeof entry.boundary !== 'number')
      || (entry.note !== undefined && typeof entry.note !== 'string')) throw new Error(`${id}: invalid case fields`)
    const rust = entry.rust === undefined ? undefined : parseRust(entry.rust, id)
    return {
      id,
      ...caseLog(entry.log, entry.edits, id),
      ...(entry.boundary === undefined ? {} : { boundary: entry.boundary }),
      ts: parseOutcome(entry.ts, id),
      ...(rust === undefined ? {} : { rust }),
    }
  })
}

/** A stored row as `scanLog` decodes it, with packed source ranges expanded. */
function decodeRow(row: string): SessionEvent {
  const event = JSON.parse(row) as SessionEvent & { sourceEventSeqs?: unknown }
  if (event.sourceEventSeqs === undefined) return event
  return { ...event, sourceEventSeqs: decodeSeqRanges(event.sourceEventSeqs) } as SessionEvent
}

interface Source {
  session: Session
  events: SessionEvent[]
  closers: SessionEvent[]
  /** Clock readings around the source's construction, bounding its end-seed time. */
  before: number
  after: number
}

/** Restore a case's log as `SessionStore.prepare` does and enter it into the store. */
function restoreSource(sessions: SessionStore, entry: ForkCase): Source {
  const { type: _type, ...meta } = JSON.parse(entry.header) as SessionHeader & { type: string }
  const events = entry.rows.map(decodeRow)
  const closers = interruptedTurnClosers(events)
  const marker = events.findLast(event => event.type === 'session/end-seed'
    && (event.data as { inherited?: unknown }).inherited === true)
  const before = Date.now()
  const session = sessions.prepare(SessionId(meta.id), {
    seed: [...events, ...closers],
    meta,
    inheritedEventCount: SessionLogOffset(meta.isSeeded && marker !== undefined ? marker.seq : 0),
    eventState: 'detached',
  })
  const after = Date.now()
  sessions.enter(session)
  return { session, events, closers, before, after }
}

const cases = loadTable()
let ctx: Context | undefined

afterEach(async () => {
  await ctx?.fiber.dispose()
  ctx = undefined
})

describe('shared fork cases', () => {
  it('read the unchanged captures and pin the table', () => {
    for (const { path, sha256 } of Object.values(LOGS)) {
      expect(createHash('sha256').update(readFileSync(new URL(path, REPO))).digest('hex'), path).toBe(sha256)
    }
    expect(cases).toHaveLength(CASE_COUNT)
    expect(new Set(cases.map(entry => entry.id)).size).toBe(CASE_COUNT)
    const limits = new Set(cases.flatMap(entry => entry.rust?.outcome === 'native-subset' ? [entry.rust.limit] : []))
    expect(limits).toEqual(new Set(LIMITS))
    expect(cases.some(entry => entry.rust?.outcome === 'unrepresentable')).toBe(true)
    expect(cases.some(entry => entry.ts.outcome === 'forked' && entry.ts.endSeed)).toBe(true)
    const refusals = new Set(cases.flatMap(entry => entry.ts.outcome === 'rejected'
      ? [entry.ts.class === 'SessionForkError' ? entry.ts.code : entry.ts.class]
      : []))
    expect(refusals).toEqual(new Set([...CODES, 'Error']))
  })

  it('refuses malformed edits and outcomes', () => {
    for (const edit of [{ truncate: 99 }, { row: 99, text: '{}' }, { row: 0, find: '', replace: 'x' },
      { append: 'a\nb' }, { tail: '{' }]) {
      expect(() => parseEdit(edit, 16, 'malformed')).toThrow('invalid edit')
    }
    expect(() => parseOutcome({ outcome: 'rejected', class: 'SessionForkError', message: 'x' }, 'malformed'))
      .toThrow('invalid outcome')
    expect(() => parseOutcome({ outcome: 'forked', stored: 1, closers: 0 }, 'malformed')).toThrow('invalid outcome')
    expect(() => parseRust({ outcome: 'unrepresentable', limit: 'x' }, 'malformed')).toThrow('invalid rust override')
  })

  for (const entry of cases) {
    it(entry.id, async () => {
      ctx = new Context()
      await ctx.plugin(SessionStore)
      const source = restoreSource(ctx.sessions, entry)
      let child: Session
      try {
        child = ctx.sessions.fork(source.session, entry.boundary as SessionSeq | undefined)
      } catch (error) {
        if (!(error instanceof Error)) throw error
        const { ts } = entry
        if (ts.outcome !== 'rejected') throw error
        expect(error.constructor.name).toBe(ts.class)
        expect(error.message).toBe(ts.message)
        if (ts.class === 'SessionForkError') expect((error as SessionForkError).code).toBe(ts.code)
        return
      }
      const { ts } = entry
      if (ts.outcome !== 'forked') throw new Error(`${entry.id}: forked, expected ${ts.class}: ${ts.message}`)
      const count = ts.stored + ts.closers + (ts.endSeed ? 1 : 0)
      expect(child.inheritedEventCount).toBe(count)
      expect(child.header).toMatchObject({ parentSession: source.session.id, isSeeded: true })
      const inherited = child.snapshotEvents(SessionLogOffset(0), child.inheritedEventCount)
      expect(inherited).toStrictEqual(source.session.snapshotEvents(SessionLogOffset(0), SessionLogOffset(count)))
      if (ts.closers > 0 || ts.endSeed) expect(ts.stored).toBe(source.events.length)
      if (ts.endSeed) expect(ts.closers).toBe(source.closers.length)
      expect(inherited.slice(0, ts.stored + ts.closers))
        .toStrictEqual([...source.events.slice(0, ts.stored), ...source.closers.slice(0, ts.closers)])
      if (!ts.endSeed) return
      const endSeed = inherited.at(-1)
      expect(endSeed).toStrictEqual({ type: 'session/end-seed', seq: count - 1, time: endSeed?.time, data: {} })
      expect(endSeed?.time).toBeGreaterThanOrEqual(source.before)
      expect(endSeed?.time).toBeLessThanOrEqual(source.after)
    })
  }
})
