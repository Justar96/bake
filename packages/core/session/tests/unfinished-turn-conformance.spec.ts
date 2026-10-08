/**
 * Runs the `turn` and `tools` fields of the shared unfinished-work cases in
 * `conformance/session/unfinished-work-cases.json` through
 * `interruptedTurnClosers`. Each case is a row prefix of a runtime capture
 * or of a log committed in the table. Its rows are parsed with `JSON.parse`,
 * and `Session.fromRestore` must admit them with their closers and the
 * restored inherited cut. The open turn and step are the ones the closers
 * end; each synthetic `tool/result` is one pending call, in closer order,
 * with its code and the recorded `tool/call` seq it cites. Three sibling
 * specs check the table's other fields with their own production functions,
 * and no Rust arm reads the table yet. This spec also rebuilds the two
 * logs materialized from `snapshots/` by their stated rule.
 */

import { createHash } from 'node:crypto'
import { readFileSync } from 'node:fs'
import { describe, expect, it } from 'vitest'
import { Session, SessionId, SessionLogOffset, interruptedTurnClosers } from 'bake-session'
import type { SessionEvent, SessionHeader } from 'bake-session'

const REPO = new URL('../../../../', import.meta.url)
const TABLE = 'conformance/session/unfinished-work-cases.json'
const SCHEMA = 'bake/session-conformance/unfinished-work-cases'
const ORACLE_KEY = 'turn'
const ORACLE = 'interruptedTurnClosers in packages/core/session/src/repair.ts over each prefix\'s parsed rows: the turn and step its closers end, and each synthetic tool/result in closer order'
const CAPTURES: Record<string, { path: string; sha256: string }> = {
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
const CASE_COUNT = 224
const INBOX_LIMITS = ['target', 'count', 'inserted', 'message-id']
/** Committed logs materialized from frozen snapshot templates. */
const MATERIALIZED: Record<string, { path: string; sha256: string }> = {
  'compaction-recovery': {
    path: 'snapshots/session/compaction-recovery/session.v3.jsonl',
    sha256: 'eaa272cab37c9086a0bf5c6beb863a835b69c24ce5cec7099a8a084434e7310e',
  },
  'subagent-continuable': {
    path: 'snapshots/sdk/subagent-continuable/session.v3.jsonl',
    sha256: '62c004a8b74557e5c0ac7cb8d83914ca8c82667ab5308d315be49e6d408fc46b',
  },
}

interface Log { lines: string[]; sweep: boolean }

/** A restored case: the stored events and closers before any appended end seed, and the Session. */
interface Restored {
  header: SessionHeader
  inherited: number
  stored: SessionEvent[]
  closers: SessionEvent[]
  session: Session
}

interface UnfinishedCase {
  id: string
  log: string
  rows: number
  /** The header line and the case's committed rows. */
  lines: string[]
  /** The rows as `JSON.parse` reads them, by seq, for `$log` references. */
  parsed: unknown[]
  ts: Record<string, unknown>
  rust: Record<string, unknown> | undefined
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

function isRef(value: unknown): boolean {
  return isObject(value) && sortedKeys(value) === '$log' && typeof value.$log === 'string'
}

function readLog(name: string, value: unknown): Log {
  const capture = CAPTURES[name]
  if (capture !== undefined) {
    if (!isObject(value) || sortedKeys(value) !== 'path,sweep' || value.path !== capture.path || value.sweep !== true) {
      throw new Error(`${TABLE}: invalid capture ${name}`)
    }
    const bytes = readFileSync(new URL(capture.path, REPO))
    if (createHash('sha256').update(bytes).digest('hex') !== capture.sha256) throw new Error(`${capture.path} changed`)
    return { lines: bytes.toString('utf8').slice(0, -1).split('\n'), sweep: true }
  }
  if (!isObject(value) || !['derivation,lines,sweep', 'derivation,lines,source,sweep'].includes(sortedKeys(value))
    || typeof value.derivation !== 'string' || typeof value.sweep !== 'boolean'
    || (value.source !== undefined && typeof value.source !== 'string')
    || !Array.isArray(value.lines) || value.lines.length === 0 || !value.lines.every(isLine)) {
    throw new Error(`${TABLE}: invalid log ${name}`)
  }
  return { lines: value.lines as string[], sweep: value.sweep }
}

/** One expected pending call: a `$log` step and the closer's code and cited `tool/call` seq. */
function isTool(value: unknown): boolean {
  return isObject(value) && sortedKeys(value) === 'callId,callSeq,closerSeq,code,step' && typeof value.callId === 'string'
    && isCount(value.closerSeq) && isRef(value.step)
    && ((value.code === 'TOOL_OUTCOME_UNKNOWN' && isCount(value.callSeq)) || (value.code === 'TOOL_NOT_STARTED' && value.callSeq === null))
}

function isChild(value: unknown): boolean {
  if (!isObject(value) || !isCount(value.seq) || typeof value.id !== 'string') return false
  if (!(Number.isSafeInteger(value.createdAt) && (value.createdAt as number) >= 0)) return false
  const keys = sortedKeys(value)
  if (value.mode === 'continuable') return keys === 'createdAt,id,label,mode,seq' && typeof value.label === 'string'
  return value.mode === 'one-shot'
    && (keys === 'createdAt,id,mode,seq' || (keys === 'createdAt,id,label,mode,seq' && typeof value.label === 'string'))
}

function parseExpected(value: unknown, id: string): Record<string, unknown> {
  if (isObject(value) && sortedKeys(value) === 'children,compaction,inbox,tools,turn') {
    const { turn, tools, compaction, children, inbox } = value
    const turnValid = turn === null || (isObject(turn) && sortedKeys(turn) === 'step,turn' && isRef(turn.turn)
      && (turn.step === null || isRef(turn.step)))
    const toolsValid = Array.isArray(tools) && tools.every(isTool) && (turn !== null || tools.length === 0)
    const compactionValid = compaction === null
      || (isObject(compaction) && sortedKeys(compaction) === 'data,startSeq' && isCount(compaction.startSeq) && isRef(compaction.data))
    const childrenValid = (Array.isArray(children) && children.every(isChild))
      || (isObject(children) && sortedKeys(children) === 'refusal' && isObject(children.refusal)
        && sortedKeys(children.refusal) === 'seq' && isCount(children.refusal.seq))
    const inboxValid = (isObject(inbox) && sortedKeys(inbox) === 'nextStep,nextTurn'
      && Array.isArray(inbox.nextTurn) && Array.isArray(inbox.nextStep))
      || (isObject(inbox) && sortedKeys(inbox) === 'refusal' && typeof inbox.refusal === 'string')
    if (turnValid && toolsValid && compactionValid && childrenValid && inboxValid) return value
  }
  throw new Error(`${id}: invalid ts outcome ${JSON.stringify(value)}`)
}

/** A `rust` override names an inbox native limit and its seq; TypeScript still asserts its own outcome. */
function parseRust(value: unknown, id: string): Record<string, unknown> | undefined {
  if (value === undefined) return undefined
  if (isObject(value) && sortedKeys(value) === 'inbox' && isObject(value.inbox)
    && sortedKeys(value.inbox) === 'limit,outcome,seq' && value.inbox.outcome === 'native-subset'
    && INBOX_LIMITS.includes(value.inbox.limit as string) && isCount(value.inbox.seq)) {
    return value
  }
  throw new Error(`${id}: invalid rust override ${JSON.stringify(value)}`)
}

function loadTable(): { logs: Map<string, Log>; cases: UnfinishedCase[] } {
  const table: unknown = JSON.parse(readFileSync(new URL(TABLE, REPO), 'utf8'))
  if (!isObject(table) || sortedKeys(table) !== 'cases,history,logs,oracles,schema,version' || table.schema !== SCHEMA
    || table.version !== 1 || !isObject(table.oracles) || sortedKeys(table.oracles) !== 'children,compaction,inbox,turn'
    || !Object.values(table.oracles).every(oracle => typeof oracle === 'string') || table.oracles[ORACLE_KEY] !== ORACLE
    || !Array.isArray(table.history) || !table.history.every(isLine) || !Array.isArray(table.cases) || !isObject(table.logs)) {
    throw new Error(`${TABLE} does not match its version-1 schema`)
  }
  const logs = new Map(Object.entries(table.logs).map(([name, value]) => [name, readLog(name, value)]))
  const cases = table.cases.map((entry: unknown): UnfinishedCase => {
    if (!isObject(entry) || typeof entry.id !== 'string') throw new Error(`invalid case ${JSON.stringify(entry)}`)
    const { id } = entry
    const unknown = Object.keys(entry).filter(key => !['id', 'log', 'rows', 'ts', 'rust', 'note'].includes(key))
    if (unknown.length > 0) throw new Error(`${id}: unknown keys ${unknown.join()}`)
    if (entry.note !== undefined && typeof entry.note !== 'string') throw new Error(`${id}: invalid note`)
    const log = typeof entry.log === 'string' ? logs.get(entry.log) : undefined
    if (log === undefined || !isCount(entry.rows) || entry.rows >= log.lines.length || id !== `${entry.log as string}/${entry.rows}`) {
      throw new Error(`${id}: invalid log or rows`)
    }
    const lines = log.lines.slice(0, entry.rows + 1)
    return {
      id,
      log: entry.log as string,
      rows: entry.rows,
      lines,
      parsed: lines.slice(1).map(line => JSON.parse(line) as unknown),
      ts: parseExpected(entry.ts, id),
      rust: parseRust(entry.rust, id),
    }
  })
  return { logs, cases }
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

/** Replace each `{ "$log": pointer }` with the value it names in the case's own rows. */
function resolve(value: unknown, entry: UnfinishedCase): unknown {
  if (Array.isArray(value)) return value.map(item => resolve(item, entry))
  if (!isObject(value)) return value
  const keys = sortedKeys(value)
  if (keys === '$log' && typeof value.$log === 'string') return at(entry.parsed, value.$log, entry.id)
  if (keys.includes('$')) throw new Error(`${entry.id}: invalid reference ${JSON.stringify(value)}`)
  return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, resolve(item, entry)]))
}

/**
 * Restore a case's rows as the catalog conformance spec does: the parsed
 * rows and their closers, admitted by `Session.fromRestore` with the cut on
 * the last inherited end seed of a seeded log.
 */
function restore(entry: UnfinishedCase): Restored {
  const { type: _type, ...header } = JSON.parse(entry.lines[0] as string) as SessionHeader & { type: string }
  const stored = entry.lines.slice(1).map(line => JSON.parse(line) as SessionEvent)
  const closers = interruptedTurnClosers(stored)
  const marker = stored.findLast(event => event.type === 'session/end-seed'
    && (event.data as { inherited?: unknown }).inherited === true)
  const inherited = header.isSeeded && marker !== undefined ? marker.seq : 0
  const session = Session.fromRestore(SessionId(header.id), [...stored, ...closers], header,
    SessionLogOffset(inherited), 'detached')
  return { header, inherited, stored, closers, session }
}

/** The open turn and pending calls the closers record. */
function turnAndTools(closers: readonly SessionEvent[]): { turn: unknown; tools: unknown[] } {
  const end = closers.at(-1)
  if (end === undefined) return { turn: null, tools: [] }
  if (end.type !== 'turn/end') throw new Error('closers must end with turn/end')
  const step = closers.find(event => event.type === 'step/end')
  const tools = closers.flatMap((event) => {
    if (event.type !== 'tool/result') return []
    const { error, message, step: callStep } = event.data
    const cited = event.sourceEventSeqs
    return [{ callId: message.source.callId, closerSeq: event.seq, step: callStep, code: error?.code, callSeq: cited?.[0] ?? null }]
  })
  return { turn: { turn: end.data.turn, step: step?.type === 'step/end' ? step.data.step : null }, tools }
}

/** Rebuild a snapshot template by the table's stated materialization rule. */
function materialize(path: string): string[] {
  const text = readFileSync(new URL(path, REPO), 'utf8')
  const substitute = (line: string): string => line
    .replaceAll(',"tools":"{{tools}}"', '')
    .replaceAll('{{system}}', 'system')
    .replace(/\{\{(session|message|id):(\d+)\}\}/g, (_, kind: string, n: string) => `${kind}-${n}`)
  const [header, ...rows] = text.slice(0, -1).split('\n').map(substitute)
  const { cwd: _cwd, ...meta } = JSON.parse(header as string) as Record<string, unknown>
  return [JSON.stringify(meta), ...rows.map((row, index) => {
    const { type, ...rest } = JSON.parse(row) as Record<string, unknown>
    return JSON.stringify({ type, seq: index, time: index, ...rest })
  })]
}

const { logs, cases } = loadTable()

describe('shared unfinished-work cases: open turn and pending tools', () => {
  it('read the unchanged sources and pin the table', () => {
    expect(cases).toHaveLength(CASE_COUNT)
    expect(new Set(cases.map(entry => entry.id)).size).toBe(CASE_COUNT)
    for (const [name, log] of logs) {
      if (!log.sweep) continue
      const swept = cases.filter(entry => entry.log === name).map(entry => entry.rows)
      expect(swept, name).toEqual(Array.from({ length: log.lines.length }, (_, rows) => rows))
    }
    for (const [name, { path, sha256 }] of Object.entries(MATERIALIZED)) {
      expect(createHash('sha256').update(readFileSync(new URL(path, REPO))).digest('hex'), path).toBe(sha256)
      expect(logs.get(name)?.lines, name).toEqual(materialize(path))
    }
    const tools = cases.flatMap(entry => entry.ts.tools as Array<{ code: string }>)
    expect(new Set(tools.map(tool => tool.code))).toEqual(new Set(['TOOL_NOT_STARTED', 'TOOL_OUTCOME_UNKNOWN']))
    const turns = cases.flatMap(entry => entry.ts.turn === null ? [] : [entry.ts.turn as { step: unknown }])
    expect(turns.some(turn => turn.step === null)).toBe(true)
    expect(turns.some(turn => turn.step !== null)).toBe(true)
  })

  it('refuses malformed outcomes, overrides, and references', () => {
    const entry = cases[0] as UnfinishedCase
    const tool = { callId: 'c', closerSeq: 1, step: { $log: '/0' }, code: 'TOOL_NOT_STARTED', callSeq: null }
    for (const ts of [
      { ...entry.ts, extra: 1 },
      { ...entry.ts, turn: null, tools: [tool] },
      { ...entry.ts, turn: { turn: 1, step: null } },
      { ...entry.ts, turn: { turn: { $log: '/0' }, step: null }, tools: [{ ...tool, callSeq: 1 }] },
      { ...entry.ts, children: [{ seq: 1, id: 'c', createdAt: 1.5, mode: 'one-shot' }] },
      { ...entry.ts, inbox: { refusal: 1 } },
    ]) {
      expect(() => parseExpected(ts, 'malformed')).toThrow('invalid ts outcome')
    }
    expect(() => parseRust({ inbox: { outcome: 'native-subset', limit: 'data', seq: 1 } }, 'malformed'))
      .toThrow('invalid rust override')
    expect(() => resolve({ $log: '/99/data' }, entry)).toThrow('does not exist')
    expect(() => resolve({ $log: '/0', extra: 1 }, entry)).toThrow('invalid reference')
  })

  for (const entry of cases) {
    it(entry.id, () => {
      const { closers } = restore(entry)
      const { turn, tools } = turnAndTools(closers)
      expect({ turn, tools }).toStrictEqual(resolve({ turn: entry.ts.turn, tools: entry.ts.tools }, entry))
    })
  }
})
