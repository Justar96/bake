/**
 * Runs the shared cases in
 * `conformance/runtime/restored-request-derivation-cases.json` through
 * `deriveRestoredRequests`, a spec-local composition of production pieces:
 * the restored events of a Session as the production read path hands them to
 * `SessionStore.prepare`, then, for each Assistant settlement at or after the
 * inherited cut, the prefix before it restored with `Session.fromRestore`,
 * `'detached'`, and the catalog's message projections, and assembled with
 * `foldRequestHeader` exactly as `replayRequests` in `runtime-fixture.ts`
 * assembles a request. `replayRequests` itself refuses a seeded log and
 * constructs its Session without projections, so it cannot serve here.
 * `Session.fromRestore` refuses an inherited count beyond its seed, so a cut
 * inside the inherited prefix is outside the domain and yields no request.
 * A plain case edits a runtime capture as text and restores it with
 * `scanLog`, `validateStoredEvents`, and `interruptedTurnClosers`; a migrated
 * case writes a released v0, v1, or v2 file in an owned temporary root and
 * reads it through the JSONL backend and `readColdSessionLog`. The
 * development Rust `replay_restored_requests` in `rust/crates/bake-session`
 * checks the same table. A `rust` override names a native limit, or the
 * refusal Rust reports with the same message; TypeScript still asserts its
 * own outcome. Requests are compared as values, and their messages also as
 * `JSON.stringify` text, because `toStrictEqual` ignores member order.
 */

import { createHash } from 'node:crypto'
import { readFileSync } from 'node:fs'
import { mkdir, mkdtemp, readdir, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { afterAll, beforeAll, describe, expect, it } from 'vitest'
import { Context } from '@deepseek-ai/cordis'
import { Session, SessionId, SessionLogOffset, foldRequestHeader, interruptedTurnClosers } from 'bake-session'
import type { SessionEvent, SessionHeader } from 'bake-session'
import { currentSessionMessageProjections } from 'bake-session-format-catalog/message-projections'
import { validateStoredEvents } from 'bake-session-persistence'
import JsonlSessionPersistence from 'bake-session-persistence-jsonl'
import { generationLogFilename, scanLog, sessionDir } from 'bake-session-persistence-jsonl/src/format.ts'
import { readColdSessionLog } from 'bake-session-query'
import { snapshotJsonValue } from 'bake-util-values'

const REPO = new URL('../../../../', import.meta.url)
const SCHEMA = 'bake/runtime-conformance/restored-request-derivation-cases'
const ORACLE = 'deriveRestoredRequests in packages/core/agent-loop/tests/restored-request-derivation-conformance.spec.ts: the restored events of restorePlainLog (scanLog, validateStoredEvents, interruptedTurnClosers, Session.fromRestore) or of readColdSessionLog for a migrated file, then for each settlement at or after the inherited cut, Session.fromRestore(prefix, ..., "detached", currentSessionMessageProjections) and foldRequestHeader(prefix) assembled as replayRequests assembles them'
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
const CASE_COUNT = 37
/** Rust's native limits; each must be witnessed. */
const LIMITS = ['coordinate', 'repeated-coordinate', 'restore/number']
/** The refusals Rust reports with the helper's message; each must be witnessed. */
const CAUSES = ['no-later-settlement', 'no-request-header']

type Json = null | boolean | number | string | Json[] | { [key: string]: Json }

type Edit =
  | { truncate: number }
  | { header: string }
  | { append: string }
  | { tail: string }
  | { row: number; text: string }
  | { row: number; find: string; replace: string }

type Outcome =
  | { outcome: 'requests'; inheritedEventCount: number; requests: Json[] }
  | { outcome: 'rejected'; message: string }

type RustOverride = { outcome: 'native-subset'; limit: string } | { outcome: 'rejected'; cause: string }

interface Migrated {
  version: 0 | 1 | 2
  header: string
  rows: string[]
}

interface DerivationCase {
  id: string
  /** A plain log's bytes, or a released file to read through the backend. */
  input: { plain: Buffer } | { migrated: Migrated }
  /** The case's rows as `JSON.parse` reads them, by index, for `$log` references. */
  rows: unknown[]
  ts: Outcome
  rust?: RustOverride
}

/** What the oracle observes for one restored Session. */
type Observed =
  | { outcome: 'requests'; inheritedEventCount: number; requests: unknown[] }
  | { outcome: 'rejected'; message: string }

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

function sha256(bytes: Buffer): string {
  return createHash('sha256').update(bytes).digest('hex')
}

/**
 * Rebuild each own dispatch of a restored Session, as `replayRequests` cuts
 * and assembles requests, from the events `SessionStore.prepare` restores.
 * @param events - the stored events followed by their closers.
 * @param header - the scanned or migrated header.
 * @param inheritedEventCount - the inherited cut.
 * @returns one request per settlement at or after the cut, in the helper's order.
 * @throws with the helper's message when a step's first settlement is missing
 * or precedes it, or an own cut has no header.
 */
function deriveRestoredRequests(events: readonly SessionEvent[], header: SessionHeader, inheritedEventCount: number): unknown[] {
  events.forEach((event, index) => {
    if (event.seq !== index) throw new Error(`events[${index}] has seq ${event.seq}`)
  })
  return events.filter(event => event.type === 'step/start').flatMap((start) => {
    const settlements = events.filter(event =>
      (event.type === 'assistant/message' || event.type === 'assistant/attempt')
      && event.data.turn === start.data.turn
      && event.data.step === start.data.step)
    if (settlements[0] === undefined || settlements[0].seq < start.seq) {
      throw new Error(`step ${start.data.turn}.${start.data.step} has no later Assistant settlement`)
    }
    // `Session.fromRestore` cannot restore a prefix shorter than the inherited cut.
    return settlements.filter(settlement => settlement.seq >= inheritedEventCount).map((settlement) => {
      const prefix = events.slice(0, settlement.seq)
      const session = Session.fromRestore(SessionId(header.id), prefix, header,
        SessionLogOffset(inheritedEventCount), 'detached', currentSessionMessageProjections)
      const requestHeader = foldRequestHeader(prefix)
      if (requestHeader === undefined) throw new Error(`step ${start.data.turn}.${start.data.step} has no request header`)
      const request = snapshotJsonValue<unknown>({
        ...requestHeader.config,
        messages: session.deriveMessages(),
        toolHistory: session.toolHistory(),
        ...requestHeader.tools !== undefined ? { tools: requestHeader.tools } : {},
        sessionId: header.id,
      })
      if (request === undefined) throw new Error('replayed request is not lossless JSON')
      return request
    })
  })
}

/** Derive requests, turning only a derivation `Error` into a rejection. */
function derive(events: readonly SessionEvent[], header: SessionHeader, inheritedEventCount: number): Observed {
  try {
    return { outcome: 'requests', inheritedEventCount, requests: deriveRestoredRequests(events, header, inheritedEventCount) }
  } catch (error) {
    if (!(error instanceof Error) || error.constructor !== Error) throw error
    return { outcome: 'rejected', message: error.message }
  }
}

/** Restore a plain log as the production read path does, which every case must pass, then derive. */
function derivePlain(log: Buffer): Observed {
  const { meta, inheritedEventCount, events } = scanLog(log)
  validateStoredEvents(meta, events)
  const restored = [...events, ...interruptedTurnClosers(events)]
  Session.fromRestore(SessionId(meta.id), restored, meta, SessionLogOffset(inheritedEventCount), 'detached',
    currentSessionMessageProjections)
  return derive(restored, meta, inheritedEventCount)
}

let root: string

/**
 * Write a released file in a fresh root, read it through the backend, and
 * derive. The Context owns the backend and is disposed before returning; the
 * source file must be the only entry of its directory afterwards, unchanged.
 */
async function deriveMigrated(entry: Migrated): Promise<Observed> {
  const header = JSON.parse(entry.header) as { id: string; cwd?: string }
  const id = SessionId(header.id)
  const caseRoot = await mkdtemp(join(root, 'case-'))
  const dir = sessionDir(caseRoot, header.cwd, id)
  const name = generationLogFilename(entry.version, 'none')
  const source = join(dir, name)
  const bytes = Buffer.from([entry.header, ...entry.rows].map(line => `${line}\n`).join(''))
  await mkdir(dir, { recursive: true })
  await writeFile(source, bytes)
  const ctx = new Context()
  try {
    await ctx.plugin(JsonlSessionPersistence, { root: caseRoot, compression: 'none' })
    const cold = await readColdSessionLog(ctx.sessionPersistence, id)
    Session.fromRestore(id, cold.events, cold.header, cold.inheritedEventCount, cold.eventState,
      currentSessionMessageProjections)
    return derive(cold.events, cold.header, cold.inheritedEventCount)
  } finally {
    await ctx.fiber.dispose()
    expect(await readdir(dir)).toStrictEqual([name])
    expect((await readFile(source)).equals(bytes)).toBe(true)
  }
}

/** Apply text edits to one capture's header, rows, and tail; a find must match exactly once. */
function caseLog(name: string, edits: Edit[], id: string): { log: Buffer; rows: string[] } {
  const source = LOGS[name]
  if (source === undefined) throw new Error(`${id}: unknown log ${name}`)
  const bytes = readFileSync(new URL(source.path, REPO))
  if (sha256(bytes) !== source.sha256) throw new Error(`${source.path} changed`)
  const lines = bytes.toString('utf8').slice(0, -1).split('\n')
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

function parseEdit(value: unknown, id: string): Edit {
  if (isObject(value)) {
    const keys = sortedKeys(value)
    if (keys === 'truncate' && isCount(value.truncate)) return value as Edit
    if ((keys === 'header' || keys === 'append' || keys === 'tail') && isLine(Object.values(value)[0])) return value as Edit
    if (keys === 'row,text' && isCount(value.row) && isLine(value.text)) return value as Edit
    if (keys === 'find,replace,row' && isCount(value.row) && isLine(value.find) && value.find !== '' && isLine(value.replace)) {
      return value as Edit
    }
  }
  throw new Error(`${id}: invalid edit ${JSON.stringify(value)}`)
}

function parseOutcome(value: unknown, id: string): Outcome {
  if (isObject(value) && value.outcome === 'requests' && sortedKeys(value) === 'inheritedEventCount,outcome,requests'
    && isCount(value.inheritedEventCount) && Array.isArray(value.requests)) {
    return value as unknown as Outcome
  }
  if (isObject(value) && value.outcome === 'rejected' && sortedKeys(value) === 'message,outcome'
    && typeof value.message === 'string') {
    return value as Outcome
  }
  throw new Error(`${id}: invalid outcome ${JSON.stringify(value)}`)
}

/** Rust may name a limit for any case, but a refusal only where TypeScript rejects. */
function parseRust(value: unknown, ts: Outcome, id: string): RustOverride {
  if (isObject(value) && value.outcome === 'native-subset' && sortedKeys(value) === 'limit,outcome'
    && LIMITS.includes(value.limit as string)) return value as RustOverride
  if (isObject(value) && value.outcome === 'rejected' && sortedKeys(value) === 'cause,outcome'
    && CAUSES.includes(value.cause as string) && ts.outcome === 'rejected') return value as RustOverride
  throw new Error(`${id}: invalid rust override ${JSON.stringify(value)}`)
}

function parseMigrated(value: unknown, id: string): Migrated {
  if (isObject(value) && sortedKeys(value) === 'header,rows,version'
    && (value.version === 0 || value.version === 1 || value.version === 2)
    && isLine(value.header) && Array.isArray(value.rows) && value.rows.every(isLine)) {
    return value as unknown as Migrated
  }
  throw new Error(`${id}: invalid migrated input`)
}

function loadTable(): DerivationCase[] {
  const table: unknown = JSON.parse(readFileSync(new URL('conformance/runtime/restored-request-derivation-cases.json', REPO), 'utf8'))
  if (!isObject(table) || sortedKeys(table) !== 'cases,history,logs,oracle,schema,version' || table.schema !== SCHEMA
    || table.version !== 3 || table.oracle !== ORACLE || !Array.isArray(table.cases)
    || !Array.isArray(table.history) || !table.history.every(isLine)
    || !isObject(table.logs) || sortedKeys(table.logs) !== Object.keys(LOGS).sort().join()
    || Object.entries(LOGS).some(([name, { path }]) => (table.logs as Record<string, unknown>)[name] !== path)) {
    throw new Error('restored-request-derivation-cases.json does not match its version-3 schema')
  }
  return table.cases.map((entry: unknown): DerivationCase => {
    if (!isObject(entry) || typeof entry.id !== 'string') throw new Error(`invalid case ${JSON.stringify(entry)}`)
    const { id } = entry
    const unknown = Object.keys(entry).filter(key => !['id', 'log', 'edits', 'migrated', 'ts', 'rust', 'note'].includes(key))
    if (unknown.length > 0) throw new Error(`${id}: unknown keys ${unknown.join()}`)
    if (entry.note !== undefined && typeof entry.note !== 'string') throw new Error(`${id}: invalid note`)
    let input: DerivationCase['input']
    let rows: string[]
    if (typeof entry.log === 'string' && Array.isArray(entry.edits) && entry.migrated === undefined) {
      const built = caseLog(entry.log, entry.edits.map(edit => parseEdit(edit, id)), id)
      input = { plain: built.log }
      rows = built.rows
    } else if (entry.log === undefined && entry.edits === undefined) {
      const migrated = parseMigrated(entry.migrated, id)
      input = { migrated }
      rows = migrated.rows
    } else {
      throw new Error(`${id}: a case is a capture with edits or a migrated file`)
    }
    const ts = parseOutcome(entry.ts, id)
    const rust = entry.rust === undefined ? undefined : parseRust(entry.rust, ts, id)
    if (ts.outcome === 'rejected' && rust === undefined) throw new Error(`${id}: a rejection needs a Rust cause or limit`)
    return { id, input, rows: rows.map(line => JSON.parse(line) as unknown), ts, ...(rust === undefined ? {} : { rust }) }
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

/** Replace each `{ "$log": pointer }` with the value it names in the case's rows. */
function resolve(value: unknown, id: string, rows: unknown[]): unknown {
  if (Array.isArray(value)) return value.map(item => resolve(item, id, rows))
  if (!isObject(value)) return value
  const keys = sortedKeys(value)
  if (keys === '$log' && typeof value.$log === 'string') return at(rows, value.$log, id)
  if (keys.includes('$')) throw new Error(`${id}: invalid reference ${JSON.stringify(value)}`)
  return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, resolve(item, id, rows)]))
}

function messagesText(requests: unknown[]): string[] {
  return requests.map(request => JSON.stringify((request as { messages: unknown }).messages))
}

const cases = loadTable()

beforeAll(async () => {
  root = await mkdtemp(join(tmpdir(), 'bake-restored-request-derivation-'))
})

afterAll(async () => {
  await rm(root, { recursive: true, force: true })
})

describe('shared restored request-derivation cases', () => {
  it('pins the table and witnesses every limit and refusal', () => {
    expect(cases).toHaveLength(CASE_COUNT)
    expect(new Set(cases.map(entry => entry.id)).size).toBe(CASE_COUNT)
    expect(new Set(cases.flatMap(entry => entry.rust?.outcome === 'native-subset' ? [entry.rust.limit] : [])))
      .toEqual(new Set(LIMITS))
    expect(new Set(cases.flatMap(entry => entry.rust?.outcome === 'rejected' ? [entry.rust.cause] : [])))
      .toEqual(new Set(CAUSES))
    // A limit claims nothing, so it must also cover input TypeScript derives.
    for (const outcome of ['requests', 'rejected']) {
      expect(cases.some(entry => entry.rust?.outcome === 'native-subset' && entry.ts.outcome === outcome), outcome).toBe(true)
    }
    expect(cases.some(entry => 'migrated' in entry.input)).toBe(true)
  })

  it('refuses malformed references', () => {
    expect(() => resolve({ $log: '/0/time' }, 'malformed', [])).toThrow('does not exist')
    expect(() => resolve({ $log: '/0/time', extra: 1 }, 'malformed', [{ time: 1 }])).toThrow('invalid reference')
  })

  for (const entry of cases) {
    it(entry.id, async () => {
      const actual = 'plain' in entry.input ? derivePlain(entry.input.plain) : await deriveMigrated(entry.input.migrated)
      const want = entry.ts.outcome === 'requests'
        ? { ...entry.ts, requests: resolve(entry.ts.requests, entry.id, entry.rows) as unknown[] }
        : entry.ts
      expect(actual).toStrictEqual(want)
      if (actual.outcome === 'requests' && want.outcome === 'requests') {
        expect(messagesText(actual.requests)).toStrictEqual(messagesText(want.requests))
      }
    })
  }
})
