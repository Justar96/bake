/**
 * Runs the shared request-derivation cases in
 * `conformance/runtime/request-derivation-cases.json` through the real
 * `replayRequests` helper. The development Rust derivation in
 * `rust/crates/bake-session` checks the same table. A case edits the unchanged
 * request-reconstruction fixture in memory, or supplies its own log, and
 * expects either normalized requests or a TypeScript rejection. Expected
 * requests are written as edits of the fixture's independent expectation.
 * A `rust` override names a native-subset limit, or the internal cause Rust
 * reports for a rejection; TypeScript still asserts its own outcome.
 */

import { createHash } from 'node:crypto'
import { readFileSync } from 'node:fs'
import { describe, expect, it } from 'vitest'
import type { JsonValue } from 'bake-util-values'
import { compareJson, normalizeRequests, replayRequests } from './runtime-fixture.ts'

const REPO = new URL('../../../../', import.meta.url)
const SCHEMA = 'bake/runtime-conformance/request-derivation-cases'
const ORACLE = 'normalizeRequests(replayRequests(log)) from packages/core/agent-loop/tests/runtime-fixture.ts'
const FIXTURE_LOG = 'conformance/runtime/request-reconstruction/tool-call-turn/session.jsonl'
const FIXTURE_EXPECTED = 'conformance/runtime/request-reconstruction/tool-call-turn/expected-requests.json'
const LOG_SHA256 = 'a7a8222990ef9f4c4f00a051c019de86d3156ca7f6c3c0d28af4be4cc3fbe657'
const EXPECTED_SHA256 = '460b031e6fd308bd9ac3fd97834aad64729b2384f0f3e803c424d7502f9dfa02'
/** Both harnesses pin the table size, so a dropped case fails. */
const CASE_COUNT = 201
/** Bounds each case's input edits. */
const MAX_EDITS = 8
const LIMITS = [
  'event-type', 'number', 'depth', 'coordinate',
  'repeated-coordinate', 'config-member', 'tool-schema', 'header', 'codec',
]
const SEED_CHECKS = [
  'lossless-json', 'message-identity', 'message-role', 'message-source', 'message-content', 'model-source', 'tool-source',
  'tool-result-block', 'tool-call-id', 'settlement', 'header-provider-model', 'header-reasoning-effort',
  'header-adapter-defaults', 'header-reason', 'header-starts-series', 'tool-update-data', 'non-surface-marker',
  'replace-start', 'replace-end', 'replace-order', 'replace-sources', 'tool-result-span', 'tool-result-target',
  'tool-result-rest', 'system-head', 'tool-update-header', 'tool-update-stale', 'tool-update-baseline',
  'tool-update-change', 'tool-update-anchor', 'tool-update-required', 'projection-required',
]
const CAUSES = [
  'header', 'codec', 'finish', 'uncommitted', 'seeded',
  ...SEED_CHECKS.map(check => `seed/${check}`), 'no-later-settlement', 'no-request-header',
]

type JsonObject = { [key: string]: JsonValue }

type LogEdit =
  | { header: string }
  | { row: number; text: string }
  | { row: number; pointer: string; value: JsonValue }
  | { row: number; pointer: string; remove: true }
  | { truncate: number }
  | { tail: string }

type RequestEdit =
  | { pointer: string; value: JsonValue }
  | { pointer: string; insert: JsonValue }
  | { pointer: string; remove: true }

type Outcome =
  | { outcome: 'requests'; requests: JsonValue[] }
  | { outcome: 'rejected'; message: string }
  | { outcome: 'rejected'; class: 'TypeError' }

type RustOverride = { outcome: 'native-subset'; limit: string } | { outcome: 'rejected'; cause: string }

interface DerivationCase {
  id: string
  /** The header record and rows, without newlines. */
  lines: string[]
  /** Bytes after the final LF, never a complete record. */
  tail: string
  ts: Outcome
  rust?: RustOverride
}

function isObject(value: unknown): value is JsonObject {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

function sortedKeys(value: object): string {
  return Object.keys(value).sort().join()
}

function isCount(value: unknown): value is number {
  return Number.isSafeInteger(value) && (value as number) >= 0
}

function sha256(bytes: Buffer): string {
  return createHash('sha256').update(bytes).digest('hex')
}

/** Row edits re-serialize the row, so their values may hold only safe integers. */
function assertIntegerNumbers(value: unknown, context: string): void {
  const pending = [value]
  for (let item = pending.pop(); item !== undefined || pending.length > 0; item = pending.pop()) {
    if (typeof item === 'number' && (!Number.isSafeInteger(item) || Object.is(item, -0))) {
      throw new Error(`${context}: a row edit number must be a safe integer other than -0`)
    }
    if (typeof item === 'object' && item !== null) pending.push(...Object.values(item))
  }
}

/** Resolve a JSON pointer without `~` escapes to its parent container and final token. */
function pointerParent(root: JsonValue, pointer: string): { parent: JsonObject | JsonValue[]; key: string } {
  if (!pointer.startsWith('/') || pointer.includes('~')) throw new Error(`unsupported pointer ${pointer}`)
  const path = pointer.slice(1).split('/')
  const key = path.pop() as string
  let parent: JsonValue = root
  for (const token of path) {
    const next: JsonValue | undefined = Array.isArray(parent)
      ? parent[Number(token)]
      : isObject(parent) && Object.hasOwn(parent, token) ? parent[token] : undefined
    if (next === undefined) throw new Error(`${pointer} does not exist`)
    parent = next
  }
  if (typeof parent !== 'object' || parent === null) throw new Error(`${pointer} has no container parent`)
  return { parent, key }
}

function arrayIndex(key: string, limit: number, pointer: string): number {
  const index = Number(key)
  if (!/^(0|[1-9]\d*)$/.test(key) || index > limit) throw new Error(`${pointer} is not an array position`)
  return index
}

/** Set, insert, or remove one value, defining object members so `__proto__` stays an own member. */
function applyEdit(root: JsonValue, pointer: string, operation: 'set' | 'insert' | 'remove', value?: JsonValue): void {
  const { parent, key } = pointerParent(root, pointer)
  if (Array.isArray(parent)) {
    const index = arrayIndex(key, operation === 'insert' ? parent.length : parent.length - 1, pointer)
    if (operation === 'insert') parent.splice(index, 0, value as JsonValue)
    else if (operation === 'remove') parent.splice(index, 1)
    else parent[index] = value as JsonValue
    return
  }
  if (operation === 'insert') throw new Error(`${pointer}: insert needs an array parent`)
  if (operation === 'remove') {
    if (!Object.hasOwn(parent, key)) throw new Error(`${pointer} does not exist`)
    Reflect.deleteProperty(parent, key)
    return
  }
  Object.defineProperty(parent, key, { value, enumerable: true, configurable: true, writable: true })
}

function parseLogEdit(value: unknown, rows: number, context: string): LogEdit {
  if (isObject(value)) {
    const keys = sortedKeys(value)
    if (keys === 'header' && typeof value.header === 'string') return { header: value.header }
    if (keys === 'truncate' && isCount(value.truncate) && value.truncate < rows) return { truncate: value.truncate }
    if (keys === 'tail' && typeof value.tail === 'string') return { tail: value.tail }
    if (isCount(value.row) && value.row < rows) {
      if (keys === 'row,text' && typeof value.text === 'string') return { row: value.row, text: value.text }
      if (keys === 'pointer,row,value' && typeof value.pointer === 'string') {
        assertIntegerNumbers(value.value, context)
        return { row: value.row, pointer: value.pointer, value: value.value as JsonValue }
      }
      if (keys === 'pointer,remove,row' && typeof value.pointer === 'string' && value.remove === true) {
        return { row: value.row, pointer: value.pointer, remove: true }
      }
    }
  }
  throw new Error(`${context}: invalid edit ${JSON.stringify(value)}`)
}

function parseRequestEdit(value: unknown, context: string): RequestEdit {
  if (isObject(value) && typeof value.pointer === 'string') {
    const keys = sortedKeys(value)
    if (keys === 'pointer,value') return { pointer: value.pointer, value: value.value as JsonValue }
    if (keys === 'insert,pointer') return { pointer: value.pointer, insert: value.insert as JsonValue }
    if (keys === 'pointer,remove' && value.remove === true) return { pointer: value.pointer, remove: true }
  }
  throw new Error(`${context}: invalid request edit ${JSON.stringify(value)}`)
}

function editLines(fixture: readonly string[], edits: readonly LogEdit[]): { lines: string[]; tail: string } {
  let lines = [...fixture]
  let tail = ''
  for (const edit of edits) {
    if ('header' in edit) lines[0] = edit.header
    else if ('tail' in edit) tail = edit.tail
    else if ('truncate' in edit) lines = lines.slice(0, edit.truncate + 1)
    else if ('text' in edit) lines[edit.row + 1] = edit.text
    else {
      const row = JSON.parse(lines[edit.row + 1] as string) as JsonValue
      if ('remove' in edit) applyEdit(row, edit.pointer, 'remove')
      else applyEdit(row, edit.pointer, 'set', edit.value)
      lines[edit.row + 1] = JSON.stringify(row)
    }
  }
  return { lines, tail }
}

function editRequests(expected: readonly JsonValue[], edits: readonly RequestEdit[]): JsonValue[] {
  const requests = structuredClone(expected) as JsonValue[]
  for (const edit of edits) {
    if ('remove' in edit) applyEdit(requests, edit.pointer, 'remove')
    else if ('insert' in edit) applyEdit(requests, edit.pointer, 'insert', edit.insert)
    else applyEdit(requests, edit.pointer, 'set', edit.value)
  }
  return requests
}

function parseOutcome(value: unknown, expected: readonly JsonValue[], context: string): Outcome {
  if (isObject(value)) {
    const keys = sortedKeys(value)
    if (value.outcome === 'requests' && keys === 'edits,outcome,requests' && value.requests === 'fixture'
      && Array.isArray(value.edits)) {
      return { outcome: 'requests', requests: editRequests(expected, value.edits.map(edit => parseRequestEdit(edit, context))) }
    }
    if (value.outcome === 'requests' && keys === 'outcome,requests' && Array.isArray(value.requests)) {
      return { outcome: 'requests', requests: value.requests }
    }
    if (value.outcome === 'rejected' && keys === 'message,outcome' && typeof value.message === 'string') {
      return { outcome: 'rejected', message: value.message }
    }
    if (value.outcome === 'rejected' && keys === 'class,outcome' && value.class === 'TypeError') {
      return { outcome: 'rejected', class: 'TypeError' }
    }
  }
  throw new Error(`${context}: invalid outcome ${JSON.stringify(value)}`)
}

/** Rust may name a limit for any case, but a rejection cause only where TypeScript rejects. */
function parseRust(value: unknown, ts: Outcome, context: string): RustOverride {
  if (isObject(value)) {
    const keys = sortedKeys(value)
    if (value.outcome === 'native-subset' && keys === 'limit,outcome' && LIMITS.includes(value.limit as string)) {
      return { outcome: 'native-subset', limit: value.limit as string }
    }
    if (value.outcome === 'rejected' && keys === 'cause,outcome' && CAUSES.includes(value.cause as string)
      && ts.outcome === 'rejected' && !('class' in ts)) {
      return { outcome: 'rejected', cause: value.cause as string }
    }
  }
  throw new Error(`${context}: rust override ${JSON.stringify(value)} does not match its TypeScript outcome`)
}

function loadTable() {
  const table: unknown = JSON.parse(readFileSync(new URL('conformance/runtime/request-derivation-cases.json', REPO), 'utf8'))
  if (!isObject(table) || sortedKeys(table) !== 'cases,fixture,history,oracle,schema,version'
    || table.schema !== SCHEMA || table.version !== 3 || table.oracle !== ORACLE || !Array.isArray(table.cases)
    || !Array.isArray(table.history) || !table.history.every(entry => typeof entry === 'string')
    || !isObject(table.fixture) || sortedKeys(table.fixture) !== 'expected,log'
    || table.fixture.log !== FIXTURE_LOG || table.fixture.expected !== FIXTURE_EXPECTED) {
    throw new Error('request-derivation-cases.json does not match its version-3 schema')
  }
  const log = readFileSync(new URL(FIXTURE_LOG, REPO))
  const expectedBytes = readFileSync(new URL(FIXTURE_EXPECTED, REPO))
  const fixtureLines = log.toString('utf8').split('\n')
  if (fixtureLines.pop() !== '') throw new Error('fixture log must end with a newline')
  const expectedFile: unknown = JSON.parse(expectedBytes.toString('utf8'))
  if (!isObject(expectedFile) || !Array.isArray(expectedFile.requests)) throw new Error('fixture expectation lacks requests')
  const expected = expectedFile.requests
  const cases = table.cases.map((value): DerivationCase => {
    if (!isObject(value) || typeof value.id !== 'string') throw new Error(`invalid case ${JSON.stringify(value)}`)
    const { id } = value
    const unknown = Object.keys(value).filter(key => !['id', 'log', 'edits', 'ts', 'rust'].includes(key))
    if (unknown.length > 0) throw new Error(`${id}: unknown keys ${unknown.join()}`)
    let lines: string[]
    let tail = ''
    if (value.log === 'fixture') {
      if (!Array.isArray(value.edits) || value.edits.length > MAX_EDITS) throw new Error(`${id}: a fixture case needs at most ${MAX_EDITS} edits`)
      const edited = editLines(fixtureLines, value.edits.map(edit => parseLogEdit(edit, fixtureLines.length - 1, id)))
      lines = edited.lines
      tail = edited.tail
    } else if (Array.isArray(value.log) && value.log.length > 0 && value.log.every(line => typeof line === 'string')
      && !Object.hasOwn(value, 'edits')) {
      lines = value.log as string[]
    } else {
      throw new Error(`${id}: log must be "fixture" with edits, or its own lines`)
    }
    if ([...lines, tail].some(line => line.includes('\n'))) throw new Error(`${id}: a line or tail holds a newline`)
    const ts = parseOutcome(value.ts, expected, id)
    if ('class' in ts && !(isObject(value.rust) && value.rust.outcome === 'native-subset')) {
      throw new Error(`${id}: Rust cannot claim a TypeError`)
    }
    const rust = Object.hasOwn(value, 'rust') ? parseRust(value.rust, ts, id) : undefined
    if (ts.outcome === 'rejected' && rust === undefined) throw new Error(`${id}: a rejection needs a Rust cause or limit`)
    return { id, lines, tail, ts, ...(rust === undefined ? {} : { rust }) }
  })
  if (new Set(cases.map(entry => entry.id)).size !== cases.length) throw new Error('case ids must be unique')
  return { cases, log, expectedBytes, fixtureLines }
}

/**
 * The helper's outcome; an error that is not an `Error` fails the case.
 * Normalization runs outside the `try`, so a non-generated message ID fails
 * the case instead of passing as a helper rejection.
 */
function replay(lines: readonly string[], tail: string): Outcome {
  let requests: ReturnType<typeof replayRequests>
  try {
    requests = replayRequests(Buffer.from(`${lines.join('\n')}\n${tail}`))
  } catch (error) {
    if (!(error instanceof Error)) throw error
    if (error.constructor === TypeError) return { outcome: 'rejected', class: 'TypeError' }
    return { outcome: 'rejected', message: error.message }
  }
  return { outcome: 'requests', requests: normalizeRequests(requests) }
}

const { cases, log, expectedBytes, fixtureLines } = loadTable()

describe('shared request-derivation cases', () => {
  it('read the unchanged request-reconstruction fixture', () => {
    expect(sha256(log)).toBe(LOG_SHA256)
    expect(sha256(expectedBytes)).toBe(EXPECTED_SHA256)
    expect(fixtureLines).toHaveLength(17)
  })

  it('pin the table size and witness every outcome, cause, and limit', () => {
    expect(cases).toHaveLength(CASE_COUNT)
    expect(new Set(cases.flatMap(entry => entry.rust?.outcome === 'native-subset' ? [entry.rust.limit] : [])))
      .toEqual(new Set(LIMITS))
    expect(new Set(cases.flatMap(entry => entry.rust?.outcome === 'rejected' ? [entry.rust.cause] : [])))
      .toEqual(new Set(CAUSES))
    // A limit claims nothing, so it must also cover input the helper accepts.
    for (const outcome of ['requests', 'rejected']) {
      expect(cases.some(entry => entry.rust?.outcome === 'native-subset' && entry.ts.outcome === outcome), outcome).toBe(true)
    }
    // Every fixture case but the first changes its input.
    const fixtureCases = cases.filter(entry => entry.lines.length > 1 && entry.id !== 'fixture'
      && entry.tail === '' && entry.lines.join('\n') === fixtureLines.join('\n'))
    expect(fixtureCases.map(entry => entry.id)).toEqual([])
  })

  for (const entry of cases) {
    it(entry.id, () => {
      const actual = replay(entry.lines, entry.tail)
      if (entry.ts.outcome === 'requests' && actual.outcome === 'requests') {
        expect(compareJson(entry.ts.requests as JsonValue, actual.requests as JsonValue, 'requests')).toEqual({ outcome: 'pass' })
      } else {
        expect(actual).toEqual(entry.ts)
      }
    })
  }
})
