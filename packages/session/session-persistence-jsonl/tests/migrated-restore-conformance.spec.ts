/**
 * Runs the shared cases in `conformance/session/migrated-restore-cases.json`
 * through the production read path of a historical Session file. Each case
 * writes its released v0, v1, or v2 header and rows as `session.v<N>.jsonl`
 * in its Session directory of a fresh temporary root, reads it with
 * `readColdSessionLog` through the JSONL backend with `compression: 'none'`,
 * and restores the result with `Session.fromRestore` and the current message
 * projections, as `SessionStore.prepare` does. The read migrates the file in
 * memory, so the Session directory must still hold only the unchanged source
 * file afterwards. The development Rust `restore_migrated` in
 * `rust/crates/bake-session` checks the same table over the migration's
 * output. A `rust` override names a native limit or a Rust refusal;
 * TypeScript still asserts its own outcome. A rejection without one is the
 * catalog's final-check refusal, which Rust claims with the same message.
 * A rejection's message spells the source path as `{src}`. Restored messages
 * are also compared as `JSON.stringify` text, because `toStrictEqual`
 * ignores member order. The spec reads only the table.
 */

import { readFileSync } from 'node:fs'
import { mkdir, mkdtemp, readdir, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { afterAll, beforeAll, describe, expect, it } from 'vitest'
import { Context } from '@deepseek-ai/cordis'
import { Session, SessionId } from 'bake-session'
import { currentSessionMessageProjections } from 'bake-session-format-catalog/message-projections'
import { readColdSessionLog } from 'bake-session-query'
import JsonlSessionPersistence from '../src/index.ts'
import { generationLogFilename, sessionDir } from '../src/format.ts'

const REPO = new URL('../../../../', import.meta.url)
const SCHEMA = 'bake/session-conformance/migrated-restore-cases'
const ORACLE = "JsonlSessionPersistence({compression: 'none'}) with session.v<N>.jsonl in its Session directory, readColdSessionLog, then Session.fromRestore(..., currentSessionMessageProjections)"
/** Both harnesses pin the table size, so a dropped case fails. */
const CASE_COUNT = 18
/** Rust's native limits; each must be witnessed. */
const LIMITS = ['encode', 'scan']
const CLASSES = ['Error', 'SessionFormatUnsupportedError', 'SessionPersistenceCorruptionError']
const RESTORED_KEYS = [
  'outcome', 'header', 'inheritedEventCount', 'events', 'closers', 'endSeedAppended', 'messages',
  'requestHeader', 'toolHistory', 'requestContext',
]

type Json = null | boolean | number | string | Json[] | { [key: string]: Json }

interface Restored {
  outcome: 'restored'
  header: Json
  inheritedEventCount: number
  events: Json[]
  closers: Json[]
  endSeedAppended: boolean
  messages: Json[]
  requestHeader: Json
  toolHistory: Json
  requestContext: Json
}

type Outcome = Restored | { outcome: 'rejected'; class: string; message: string }

type RustOverride =
  | { outcome: 'native-subset'; limit: string }
  | { outcome: 'refused'; cause: string }

interface MigratedCase {
  id: string
  version: 0 | 1 | 2
  header: string
  rows: string[]
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

function parseOutcome(value: unknown, id: string): Outcome {
  if (isObject(value) && value.outcome === 'rejected' && sortedKeys(value) === 'class,message,outcome'
    && CLASSES.includes(value.class as string) && typeof value.message === 'string') {
    return value as Outcome
  }
  if (isObject(value) && value.outcome === 'restored' && sortedKeys(value) === [...RESTORED_KEYS].sort().join()
    && isCount(value.inheritedEventCount) && Array.isArray(value.events) && Array.isArray(value.closers)
    && typeof value.endSeedAppended === 'boolean' && Array.isArray(value.messages)) {
    return value as unknown as Outcome
  }
  throw new Error(`${id}: invalid outcome ${JSON.stringify(value)}`)
}

function parseRust(value: unknown, ts: Outcome, id: string): RustOverride {
  if (isObject(value) && value.outcome === 'native-subset' && sortedKeys(value) === 'limit,outcome'
    && LIMITS.includes(value.limit as string)) return value as RustOverride
  if (isObject(value) && value.outcome === 'refused' && sortedKeys(value) === 'cause,outcome'
    && typeof value.cause === 'string' && ts.outcome === 'rejected') return value as RustOverride
  throw new Error(`${id}: invalid rust override ${JSON.stringify(value)}`)
}

function loadTable(): MigratedCase[] {
  const table: unknown = JSON.parse(readFileSync(new URL('conformance/session/migrated-restore-cases.json', REPO), 'utf8'))
  if (!isObject(table) || sortedKeys(table) !== 'cases,history,oracle,schema,version' || table.schema !== SCHEMA
    || table.version !== 2 || table.oracle !== ORACLE || !Array.isArray(table.cases)
    || !Array.isArray(table.history) || !table.history.every(isLine)) {
    throw new Error('migrated-restore-cases.json does not match its version-2 schema')
  }
  return table.cases.map((entry: unknown): MigratedCase => {
    if (!isObject(entry) || typeof entry.id !== 'string') throw new Error(`invalid case ${JSON.stringify(entry)}`)
    const { id } = entry
    const unknown = Object.keys(entry)
      .filter(key => !['id', 'version', 'header', 'rows', 'sourceBudget', 'ts', 'rust', 'note'].includes(key))
    if (unknown.length > 0) throw new Error(`${id}: unknown keys ${unknown.join()}`)
    if (entry.version !== 0 && entry.version !== 1 && entry.version !== 2) throw new Error(`${id}: version must be 0, 1, or 2`)
    if (!isLine(entry.header) || !Array.isArray(entry.rows) || !entry.rows.every(isLine)) {
      throw new Error(`${id}: header and rows must be JSON text`)
    }
    // Rust's scan budget; TypeScript bounds nothing.
    if (entry.sourceBudget !== undefined && !isCount(entry.sourceBudget)) throw new Error(`${id}: invalid sourceBudget`)
    if (entry.note !== undefined && typeof entry.note !== 'string') throw new Error(`${id}: invalid note`)
    const ts = parseOutcome(entry.ts, id)
    if (ts.outcome === 'rejected' && entry.rust === undefined && ts.class !== 'SessionFormatUnsupportedError') {
      throw new Error(`${id}: only the final check's refusal needs no Rust outcome`)
    }
    const rust = entry.rust === undefined ? undefined : parseRust(entry.rust, ts, id)
    return {
      id,
      version: entry.version,
      header: entry.header,
      rows: entry.rows as string[],
      ts,
      ...(rust === undefined ? {} : { rust }),
    }
  })
}

const cases = loadTable()
let root: string

beforeAll(async () => {
  root = await mkdtemp(join(tmpdir(), 'bake-migrated-restore-conformance-'))
})

afterAll(async () => {
  await rm(root, { recursive: true, force: true })
})

/**
 * Write the case's historical file in a fresh root, read it through the
 * backend, and restore it. The Context owns the backend and is disposed
 * before returning; the source file must be the only entry of its directory
 * afterwards, with its bytes unchanged.
 */
async function readThroughBackend(entry: MigratedCase) {
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
    try {
      const cold = await readColdSessionLog(ctx.sessionPersistence, id)
      const session = Session.fromRestore(id, cold.events, cold.header, cold.inheritedEventCount, cold.eventState,
        currentSessionMessageProjections)
      return {
        result: {
          header: cold.header,
          inheritedEventCount: cold.inheritedEventCount,
          events: cold.events,
          endSeedAppended: session.seq > cold.events.length,
          messages: session.deriveMessages(),
          requestHeader: session.requestHeader() ?? null,
          toolHistory: session.toolHistory(),
          requestContext: session.requestContext() ?? null,
        },
        source,
      }
    } catch (error) {
      return { error, source }
    }
  } finally {
    await ctx.fiber.dispose()
    expect(await readdir(dir)).toStrictEqual([name])
    expect((await readFile(source)).equals(bytes)).toBe(true)
  }
}

describe('shared migrated restore cases', () => {
  it('pins the table', () => {
    expect(cases).toHaveLength(CASE_COUNT)
    expect(new Set(cases.map(entry => entry.id)).size).toBe(CASE_COUNT)
    const limits = new Set(cases.flatMap(entry => entry.rust?.outcome === 'native-subset' ? [entry.rust.limit] : []))
    expect(limits).toEqual(new Set(LIMITS))
    expect(cases.some(entry => entry.ts.outcome === 'rejected' && entry.rust === undefined)).toBe(true)
  })

  for (const entry of cases) {
    it(entry.id, async () => {
      const read = await readThroughBackend(entry)
      const { ts } = entry
      if ('error' in read) {
        const { error } = read
        if (!(error instanceof Error) || ts.outcome !== 'rejected') throw error
        expect(error.constructor.name).toBe(ts.class)
        expect(error.message).toBe(ts.message.replaceAll('{src}', () => read.source))
        return
      }
      if (ts.outcome !== 'restored') throw new Error(`${entry.id}: restored, expected ${ts.class}: ${ts.message}`)
      const { result } = read
      expect({
        header: result.header,
        inheritedEventCount: result.inheritedEventCount,
        events: result.events.slice(0, ts.events.length),
        closers: result.events.slice(ts.events.length),
        endSeedAppended: result.endSeedAppended,
        messages: result.messages,
        requestHeader: result.requestHeader,
        toolHistory: result.toolHistory,
        requestContext: result.requestContext,
      }).toStrictEqual({
        header: ts.header,
        inheritedEventCount: ts.inheritedEventCount,
        events: ts.events,
        closers: ts.closers,
        endSeedAppended: ts.endSeedAppended,
        messages: ts.messages,
        requestHeader: ts.requestHeader,
        toolHistory: ts.toolHistory,
        requestContext: ts.requestContext,
      })
      expect(JSON.stringify(result.messages)).toBe(JSON.stringify(ts.messages))
    })
  }
})
