/**
 * Runs the shared cases in `conformance/session/plain-append-cases.json`
 * through the real JSONL backend with `compression: 'none'`, each in its own
 * temporary root. A case creates a Session with `create(header, {
 * inheritedEventCount })`, or writes the given bytes to the current log path
 * and opens it for write, then applies each `append` or `flush` in order and
 * reads the log file's bytes after it, `null` while no file exists. The
 * development Rust model `PlainAppendLog` in `rust/crates/bake-session`
 * checks the same table. `ts` is the operation's outcome, or the thrown class
 * with its exact message; an engine `TypeError` carries no message. A `rust`
 * override names a native limit, and TypeScript still asserts its own
 * outcome and bytes; on a create it covers every operation, and a log path
 * that cannot be resolved must leave no log file under the root. A `scan` reads the final bytes back with `scanLog`. The
 * lease and root-encoding files are not compared. The spec reads only the
 * table.
 */

import { readFileSync } from 'node:fs'
import { mkdir, mkdtemp, readdir, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { dirname, join } from 'node:path'
import { afterAll, beforeAll, describe, expect, it } from 'vitest'
import { Context } from '@deepseek-ai/cordis'
import { SessionId } from 'bake-session'
import type { SessionEvent, SessionHeader, SessionLogOffset } from 'bake-session'
import { SessionFormatError } from 'bake-session-format'
import type { SessionHandle } from 'bake-session-persistence'
import JsonlSessionPersistence from '../src/index.ts'
import { logPath, scanLog } from '../src/format.ts'

const REPO = new URL('../../../../', import.meta.url)
const SCHEMA = 'bake/session-conformance/plain-append-cases'
const ORACLE = 'create or write-open a handle of the JSONL backend with compression none in an owned temporary root, apply append and flush in order, and read the log file\'s bytes after each operation; scanLog reads the final bytes back'
/** Both harnesses pin the table size, so a dropped case fails. */
const CASE_COUNT = 45
const LIMITS = ['seq-value', 'encode', 'empty-id']
/** Classes are matched exactly. */
const CLASSES = new Map<string, abstract new (...args: never[]) => Error>([
  ['Error', Error],
  ['TypeError', TypeError],
  ['SessionFormatError', SessionFormatError],
])

type Outcome =
  | { outcome: 'appended' }
  | { outcome: 'flushed' }
  | { outcome: 'thrown'; class: string; message?: string }

type Operation =
  | { op: 'append'; events: unknown[]; ts: Outcome; log: string | null; rust?: string }
  | { op: 'flush'; ts: Outcome; log: string | null }

interface AppendCase {
  id: string
  create?: { header: unknown; inheritedEventCount?: number; ts?: Outcome; rust?: string }
  open?: string
  ops: Operation[]
  scan?: { events: number; committedBytes: number; inheritedEventCount: number }
}

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

function sortedKeys(value: object): string {
  return Object.keys(value).sort().join()
}

function parseOutcome(value: unknown, id: string): Outcome {
  if (isObject(value)) {
    const keys = sortedKeys(value)
    if (keys === 'outcome' && (value.outcome === 'appended' || value.outcome === 'flushed')) {
      return { outcome: value.outcome }
    }
    if (value.outcome === 'thrown' && typeof value.class === 'string' && CLASSES.has(value.class)) {
      if (keys === 'class,message,outcome' && typeof value.message === 'string') {
        return { outcome: 'thrown', class: value.class, message: value.message }
      }
      if (keys === 'class,outcome' && value.class === 'TypeError') return { outcome: 'thrown', class: 'TypeError' }
    }
  }
  throw new Error(`${id}: invalid ts outcome ${JSON.stringify(value)}`)
}

function parseRust(value: unknown, id: string): string | undefined {
  if (value === undefined) return undefined
  if (isObject(value) && sortedKeys(value) === 'limit,outcome' && value.outcome === 'native-subset'
    && LIMITS.includes(value.limit as string)) return value.limit as string
  throw new Error(`${id}: invalid rust override ${JSON.stringify(value)}`)
}

function parseOperation(value: unknown, id: string): Operation {
  if (!isObject(value) || (value.log !== null && typeof value.log !== 'string')) {
    throw new Error(`${id}: invalid operation ${JSON.stringify(value)}`)
  }
  const ts = parseOutcome(value.ts, id)
  const log = value.log as string | null
  if (value.op === 'flush' && sortedKeys(value) === 'log,op,ts' && ts.outcome === 'flushed') {
    return { op: 'flush', ts, log }
  }
  if (value.op === 'append' && Array.isArray(value.events) && ts.outcome !== 'flushed'
    && Object.keys(value).every(key => ['op', 'events', 'ts', 'log', 'rust'].includes(key))) {
    const rust = parseRust(value.rust, id)
    if (ts.outcome === 'thrown' && ts.message === undefined && rust === undefined) {
      throw new Error(`${id}: a TypeError without a message needs a rust limit`)
    }
    return { op: 'append', events: value.events, ts, log, ...(rust === undefined ? {} : { rust }) }
  }
  throw new Error(`${id}: invalid operation ${JSON.stringify(value)}`)
}

function loadTable(): AppendCase[] {
  const table: unknown = JSON.parse(readFileSync(new URL('conformance/session/plain-append-cases.json', REPO), 'utf8'))
  if (!isObject(table) || sortedKeys(table) !== 'cases,history,oracle,schema,version' || table.schema !== SCHEMA
    || table.version !== 1 || table.oracle !== ORACLE || !Array.isArray(table.cases)
    || !Array.isArray(table.history) || !table.history.every(line => typeof line === 'string')) {
    throw new Error('plain-append-cases.json does not match its version-1 schema')
  }
  return table.cases.map((entry: unknown): AppendCase => {
    if (!isObject(entry) || typeof entry.id !== 'string' || !Array.isArray(entry.ops)) {
      throw new Error(`invalid case ${JSON.stringify(entry)}`)
    }
    const { id } = entry
    const unknown = Object.keys(entry).filter(key => !['id', 'create', 'open', 'ops', 'scan', 'note'].includes(key))
    if (unknown.length > 0) throw new Error(`${id}: unknown keys ${unknown.join()}`)
    if (entry.note !== undefined && typeof entry.note !== 'string') throw new Error(`${id}: invalid note`)
    const ops = entry.ops.map(op => parseOperation(op, id))
    const limited = ops.findIndex(op => op.op === 'append' && op.rust !== undefined)
    if (limited !== -1 && (limited !== ops.length - 1 || entry.scan !== undefined)) {
      throw new Error(`${id}: a limit ends the case and scans nothing`)
    }
    let scan: AppendCase['scan']
    if (entry.scan !== undefined) {
      const value = entry.scan
      if (!isObject(value) || sortedKeys(value) !== 'committedBytes,events,inheritedEventCount'
        || !Object.values(value).every(Number.isSafeInteger)) throw new Error(`${id}: invalid scan`)
      scan = value as AppendCase['scan']
    }
    const base = { id, ops, ...(scan === undefined ? {} : { scan }) }
    if (isObject(entry.create) && entry.open === undefined) {
      const { header, inheritedEventCount, ts, rust: rustValue, ...rest } = entry.create
      if (Object.keys(rest).length > 0 || !Object.hasOwn(entry.create, 'header')
        || (inheritedEventCount !== undefined && !Number.isSafeInteger(inheritedEventCount))) {
        throw new Error(`${id}: invalid create`)
      }
      const outcome = ts === undefined ? undefined : parseOutcome(ts, id)
      if (outcome !== undefined && (outcome.outcome !== 'thrown' || ops.length > 0)) {
        throw new Error(`${id}: a refused create has no operations`)
      }
      const rust = parseRust(rustValue, id)
      if (rust !== undefined && (outcome !== undefined || limited !== -1 || scan !== undefined)) {
        throw new Error(`${id}: a limited create succeeds, ends the case, and scans nothing`)
      }
      return {
        ...base,
        create: {
          header,
          ...(inheritedEventCount === undefined ? {} : { inheritedEventCount: inheritedEventCount as number }),
          ...(outcome === undefined ? {} : { ts: outcome }),
          ...(rust === undefined ? {} : { rust }),
        },
      }
    }
    if (typeof entry.open === 'string' && entry.create === undefined) return { ...base, open: entry.open }
    throw new Error(`${id}: invalid start`)
  })
}

/** Run one operation; an error of an unlisted class fails the case. */
async function outcome(run: () => Promise<void>, done: Outcome): Promise<Outcome> {
  try {
    await run()
    return done
  } catch (error) {
    const constructor = (error as object).constructor
    const name = [...CLASSES].find(([, value]) => value === constructor)?.[0]
    if (name === undefined) throw error
    return { outcome: 'thrown', class: name, message: (error as Error).message }
  }
}

function expectOutcome(actual: Outcome, expected: Outcome, context: string): void {
  if (expected.outcome === 'thrown' && expected.message === undefined) {
    expect(actual.outcome === 'thrown' ? actual.class : actual, context).toBe(expected.class)
  } else {
    expect(actual, context).toStrictEqual(expected)
  }
}

/** The current log path, or `undefined` when the id or `cwd` has none. */
function tryLogPath(root: string, cwd: string | undefined, id: string): string | undefined {
  try {
    return logPath(root, cwd, SessionId(id), 'none')
  } catch {
    return undefined
  }
}

/** Every `.jsonl` file beneath `root`. */
async function logFiles(root: string): Promise<string[]> {
  const entries = await readdir(root, { recursive: true, withFileTypes: true })
  return entries.filter(entry => entry.isFile() && entry.name.endsWith('.jsonl')).map(entry => entry.name)
}

/**
 * The log file's text, or `null` when no file exists. Without a resolvable
 * path, no log file may exist anywhere beneath `root`.
 */
async function readLog(path: string | undefined, root: string): Promise<string | null> {
  if (path === undefined) {
    expect(await logFiles(root)).toStrictEqual([])
    return null
  }
  try {
    return await readFile(path, 'utf8')
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === 'ENOENT') return null
    throw error
  }
}

const cases = loadTable()
let root: string

beforeAll(async () => {
  root = await mkdtemp(join(tmpdir(), 'bake-plain-append-conformance-'))
})

afterAll(async () => {
  await rm(root, { recursive: true, force: true })
})

describe('shared plain-log append cases', () => {
  it('pin the table and witness every limit and class', () => {
    expect(cases).toHaveLength(CASE_COUNT)
    expect(new Set(cases.map(entry => entry.id)).size).toBe(CASE_COUNT)
    expect(new Set(cases.flatMap(entry => [entry.create?.rust, ...entry.ops.map(op => op.op === 'append' ? op.rust : undefined)]
      .flatMap(rust => rust === undefined ? [] : [rust]))))
      .toEqual(new Set(LIMITS))
    const thrown = cases.flatMap(entry => [entry.create?.ts, ...entry.ops.map(op => op.ts)])
      .flatMap(ts => ts?.outcome === 'thrown' ? [ts.class] : [])
    expect(new Set(thrown)).toEqual(new Set(CLASSES.keys()))
  })

  it('refuses malformed outcomes, operations, and overrides', () => {
    expect(() => parseOutcome({ outcome: 'thrown', class: 'RangeError', message: 'x' }, 'malformed')).toThrow('invalid ts outcome')
    expect(() => parseOutcome({ outcome: 'thrown', class: 'Error' }, 'malformed')).toThrow('invalid ts outcome')
    expect(() => parseOperation({ op: 'flush', ts: { outcome: 'appended' }, log: null }, 'malformed')).toThrow('invalid operation')
    expect(() => parseRust({ outcome: 'native-subset', limit: 'other' }, 'malformed')).toThrow('invalid rust override')
  })

  for (const entry of cases) {
    it(entry.id, async () => {
      const caseRoot = await mkdtemp(join(root, 'case-'))
      const ctx = new Context()
      let handle: SessionHandle | undefined
      try {
        await ctx.plugin(JsonlSessionPersistence, { root: caseRoot, compression: 'none' })
        let path: string | undefined
        if (entry.create !== undefined) {
          const { header, inheritedEventCount, ts } = entry.create
          const created = header as SessionHeader
          path = tryLogPath(caseRoot, isObject(header) && typeof header.cwd === 'string' ? header.cwd : undefined,
            isObject(header) && typeof header.id === 'string' ? header.id : 'unnamed')
          const options = inheritedEventCount === undefined ? undefined : { inheritedEventCount: inheritedEventCount as SessionLogOffset }
          const result = await outcome(async () => {
            handle = await ctx.sessionPersistence.create(created, options)
          }, { outcome: 'appended' })
          if (ts !== undefined) {
            expectOutcome(result, ts, 'create')
            expect(await readLog(path, caseRoot)).toBeNull()
            return
          }
          expect(result, 'create').toStrictEqual({ outcome: 'appended' })
        } else {
          const log = entry.open as string
          const header = JSON.parse(log.slice(0, log.indexOf('\n'))) as { id: string; cwd?: string }
          path = logPath(caseRoot, header.cwd, SessionId(header.id), 'none')
          await mkdir(dirname(path), { recursive: true })
          await writeFile(path, log)
          handle = await ctx.sessionPersistence.open(SessionId(header.id), 'write')
        }
        const writer = handle as SessionHandle
        for (const [index, op] of entry.ops.entries()) {
          const context = `op ${index}`
          const actual = op.op === 'flush'
            ? await outcome(() => writer.flush(), { outcome: 'flushed' })
            : await outcome(() => writer.append(op.events as SessionEvent[]), { outcome: 'appended' })
          expectOutcome(actual, op.ts, context)
          expect(await readLog(path, caseRoot), context).toBe(op.log)
        }
        if (entry.scan !== undefined) {
          const bytes = await readFile(path as string)
          const scan = scanLog(bytes)
          expect({
            events: scan.events.length,
            committedBytes: scan.committedBytes,
            inheritedEventCount: scan.inheritedEventCount,
          }).toStrictEqual(entry.scan)
        }
      } finally {
        await handle?.close()
        await ctx.fiber.dispose()
      }
    })
  }
})
