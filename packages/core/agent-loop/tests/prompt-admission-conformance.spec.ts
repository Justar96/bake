/**
 * Runs the shared cases in `conformance/session/prompt-admission-cases.json`
 * over the Session `restorePlainLog` restores: `scanLog`,
 * `validateStoredEvents`, `interruptedTurnClosers`, then `Session.fromRestore`
 * with the catalog's message projections, so `image/offload` decisions
 * project as production restores them. Each prompt query runs
 * `SystemPromptProjection.project`; the generation is
 * `session.surface.contentGeneration`; a tool query repeats the agent's
 * private `toolsChanged` body over the exported `headerEquals` and
 * `canonicalHeader`. A series query composes those production pieces as the
 * agent's `startsSeries` disjunction does; the composition is this spec's,
 * not production code. The development Rust functions in
 * `rust/crates/bake-session` check the same table. A case's `rust` marker
 * names the inherited restore limit where Rust refuses the log; this spec
 * still asserts TypeScript's outcome there.
 */

import { createHash } from 'node:crypto'
import { readFileSync } from 'node:fs'
import { describe, expect, it } from 'vitest'
import type { ToolSchema } from 'bake-llm'
import { Session, SessionId, SessionLogOffset, canonicalHeader, headerEquals, interruptedTurnClosers } from 'bake-session'
import { currentSessionMessageProjections } from 'bake-session-format-catalog/message-projections'
import { validateStoredEvents } from 'bake-session-persistence'
import { scanLog } from 'bake-session-persistence-jsonl/src/format.ts'
import { SystemPromptProjection } from '../src/runtime-context.ts'
import type { SystemPromptCommit } from '../src/runtime-context.ts'

const REPO = new URL('../../../../', import.meta.url)
const SCHEMA = 'bake/session-conformance/prompt-admission-cases'
const ORACLE = 'SystemPromptProjection.project, session.surface.contentGeneration, and headerEquals(baseline, canonicalHeader({...baseline, tools})) over the Session restorePlainLog(log) restores in packages/core/agent-loop/tests/prompt-admission-conformance.spec.ts; series is that spec\'s composition of the startsSeries disjunction in agent.ts, not production code'
const SOURCE = '@deepseek-ai/dsh-system-prompt'
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
const CASE_COUNT = 34

type Edit =
  | { truncate: number }
  | { header: string }
  | { append: string }
  | { row: number; text: string }
  | { row: number; find: string; replace: string }

type Intent = 'append' | { replace: number }

interface Commit { text: string; intent: Intent }

interface PromptQuery { rendered: string; inHistory: boolean; startsSeries: boolean; commits: Commit[] }

interface ToolsQuery { tools: unknown[]; changed: boolean }

interface SeriesQuery {
  declared: boolean
  generationAtLastRequest: number
  toolUpdateRoute: boolean
  tools: unknown[]
  startsSeries: boolean
}

interface Expected {
  contentGeneration: number
  prompts: PromptQuery[]
  toolsChanged: ToolsQuery[]
  series: SeriesQuery[]
}

/** Where Rust's restoration refuses the case's log with a named limit. */
interface RustLimit { outcome: 'native-subset'; limit: 'number'; seq: number }

interface PromptCase {
  id: string
  log: Buffer
  rowCount: number
  /** The edited rows as `JSON.parse` reads them, by seq, for `$log` references. */
  rows: unknown[]
  ts: Expected
  rust: RustLimit | undefined
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
 * `SessionStore.prepare` do, with the catalog's message projections.
 * @param log - one complete plain log.
 * @returns the restored Session and its stored row count.
 */
function restorePlainLog(log: Buffer): { session: Session; storedEventCount: number } {
  const { meta, inheritedEventCount, events, committedBytes } = scanLog(log)
  expect(committedBytes).toBe(log.length)
  validateStoredEvents(meta, events)
  const closers = interruptedTurnClosers(events)
  const session = Session.fromRestore(SessionId(meta.id), [...events, ...closers], meta,
    SessionLogOffset(inheritedEventCount), 'detached', currentSessionMessageProjections)
  return { session, storedEventCount: events.length }
}

/** Apply text edits to one capture's header and rows; a find must match exactly once. */
function caseLog(name: string, edits: Edit[], id: string): { log: Buffer; rows: string[] } {
  const source = LOGS[name]
  if (source === undefined) throw new Error(`${id}: unknown log ${name}`)
  const text = readFileSync(new URL(source.path, REPO), 'utf8')
  const lines = text.slice(0, -1).split('\n')
  let header = lines[0] as string
  let rows = lines.slice(1)
  for (const edit of edits) {
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
  return { log: Buffer.from([header, ...rows].map(line => `${line}\n`).join('')), rows }
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

function isCommit(value: unknown): value is Commit {
  if (!isObject(value) || sortedKeys(value) !== 'intent,text' || typeof value.text !== 'string') return false
  const { intent } = value
  return intent === 'append' || (isObject(intent) && sortedKeys(intent) === 'replace' && isCount(intent.replace))
}

/** Shape-check an expectation; the values themselves are compared strictly later. */
function parseExpected(value: unknown, id: string): Expected {
  const flags = (entry: Record<string, unknown>, names: string[]) =>
    names.every(name => typeof entry[name] === 'boolean')
  if (isObject(value) && sortedKeys(value) === 'contentGeneration,prompts,series,toolsChanged'
    && isCount(value.contentGeneration)
    && Array.isArray(value.prompts) && value.prompts.every(prompt => isObject(prompt)
      && sortedKeys(prompt) === 'commits,inHistory,rendered,startsSeries' && typeof prompt.rendered === 'string'
      && flags(prompt, ['inHistory', 'startsSeries']) && Array.isArray(prompt.commits) && prompt.commits.every(isCommit))
    && Array.isArray(value.toolsChanged) && value.toolsChanged.every(query => isObject(query)
      && sortedKeys(query) === 'changed,tools' && Array.isArray(query.tools) && flags(query, ['changed']))
    && Array.isArray(value.series) && value.series.every(query => isObject(query)
      && sortedKeys(query) === 'declared,generationAtLastRequest,startsSeries,toolUpdateRoute,tools'
      && isCount(query.generationAtLastRequest) && Array.isArray(query.tools)
      && flags(query, ['declared', 'startsSeries', 'toolUpdateRoute']))) {
    return value as unknown as Expected
  }
  throw new Error(`${id}: invalid expectation ${JSON.stringify(value)}`)
}

function loadTable(): PromptCase[] {
  const table: unknown = JSON.parse(readFileSync(new URL('conformance/session/prompt-admission-cases.json', REPO), 'utf8'))
  if (!isObject(table) || sortedKeys(table) !== 'cases,history,logs,oracle,schema,version' || table.schema !== SCHEMA
    || table.version !== 1 || table.oracle !== ORACLE || !Array.isArray(table.cases)
    || !Array.isArray(table.history) || !table.history.every(isLine)
    || JSON.stringify(table.logs) !== JSON.stringify(Object.fromEntries(Object.entries(LOGS).map(([name, { path }]) => [name, path])))) {
    throw new Error('prompt-admission-cases.json does not match its version-1 schema')
  }
  return table.cases.map((entry: unknown): PromptCase => {
    if (!isObject(entry) || typeof entry.id !== 'string' || typeof entry.log !== 'string' || !Array.isArray(entry.edits)) {
      throw new Error(`invalid case ${JSON.stringify(entry)}`)
    }
    const { id } = entry
    const unknown = Object.keys(entry).filter(key => !['id', 'log', 'edits', 'ts', 'rust', 'note'].includes(key))
    if (unknown.length > 0) throw new Error(`${id}: unknown keys ${unknown.join()}`)
    if (entry.note !== undefined && typeof entry.note !== 'string') throw new Error(`${id}: invalid note`)
    const { rust } = entry
    if (rust !== undefined && !(isObject(rust) && sortedKeys(rust) === 'limit,outcome,seq'
      && rust.outcome === 'native-subset' && rust.limit === 'number' && isCount(rust.seq))) {
      throw new Error(`${id}: invalid rust marker ${JSON.stringify(rust)}`)
    }
    const edits: Edit[] = []
    for (const value of entry.edits) {
      const rows = caseLog(entry.log, edits, id).rows.length
      edits.push(parseEdit(value, rows, id))
    }
    const { log, rows } = caseLog(entry.log, edits, id)
    return {
      id,
      log,
      rowCount: rows.length,
      rows: rows.map((row) => { try { return JSON.parse(row) as unknown } catch { return undefined } }),
      ts: parseExpected(entry.ts, id),
      rust: rust as RustLimit | undefined,
    }
  })
}

/**
 * Resolve a JSON pointer without `~` escapes, refusing a missing member and
 * an array step that is not a canonical decimal index, such as `01` or `length`.
 */
function at(root: unknown, pointer: string, id: string): unknown {
  if (!pointer.startsWith('/') || pointer.includes('~')) throw new Error(`${id}: unsupported pointer ${pointer}`)
  let node = root
  for (const key of pointer.slice(1).split('/')) {
    if (typeof node !== 'object' || node === null || !Object.hasOwn(node, key)
      || (Array.isArray(node) && !/^(?:0|[1-9]\d*)$/.test(key))) throw new Error(`${id}: ${pointer} does not exist`)
    node = (node as Record<string, unknown>)[key]
  }
  return node
}

/** Replace each `{ "$log": pointer }` with the value it names in the case's edited rows. */
function resolve(value: unknown, entry: PromptCase): unknown {
  if (Array.isArray(value)) return value.map(item => resolve(item, entry))
  if (!isObject(value)) return value
  const keys = sortedKeys(value)
  if (keys === '$log' && typeof value.$log === 'string') return at(entry.rows, value.$log, entry.id)
  if (keys.includes('$')) throw new Error(`${entry.id}: invalid reference ${JSON.stringify(value)}`)
  return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, resolve(item, entry)]))
}

/** A projection commit in the table's form, after checking its message and intent exactly. */
function commitOf(commit: SystemPromptCommit): Commit {
  const { message, intent } = commit
  const content = message.content as unknown[]
  const text = content.length === 0 ? '' : (content[0] as { text: string }).text
  expect(message).toStrictEqual({
    role: 'system',
    content: text.length === 0 ? [] : [{ type: 'text', text }],
    source: { kind: 'plugin', plugin: SOURCE },
    id: message.id,
  })
  expect(typeof message.id === 'string' && message.id.length > 0).toBe(true)
  if (intent.surfaceOp === 'append') {
    expect(intent).toStrictEqual({ surfaceOp: 'append' })
    return { text, intent: 'append' }
  }
  const seq = intent.surfaceOp.startSeq
  expect(intent).toStrictEqual({ surfaceOp: { op: 'replace', startSeq: seq, endSeq: seq }, sourceEventSeqs: [seq] })
  return { text, intent: { replace: seq } }
}

/** The agent's private `toolsChanged(tools)`, over the restored Session. */
function toolsChanged(session: Session, tools: readonly unknown[]): boolean {
  const baseline = session.requestHeader()
  if (baseline === undefined) return false
  return !headerEquals(baseline, canonicalHeader({ ...baseline, tools: [...tools] as ToolSchema[] }))
}

const cases = loadTable()

describe('shared prompt admission cases', () => {
  it('read the unchanged captures and pin the table', () => {
    for (const { path, sha256 } of Object.values(LOGS)) {
      expect(createHash('sha256').update(readFileSync(new URL(path, REPO))).digest('hex'), path).toBe(sha256)
    }
    expect(cases).toHaveLength(CASE_COUNT)
    expect(new Set(cases.map(entry => entry.id)).size).toBe(CASE_COUNT)
    expect(cases.some(entry => entry.rust !== undefined), 'the number limit is witnessed').toBe(true)
  })

  it('refuses malformed edits, expectations, and references', () => {
    expect(() => parseEdit({ truncate: 99 }, 16, 'malformed')).toThrow('invalid edit')
    expect(() => parseEdit({ tail: 'x' }, 16, 'malformed')).toThrow('invalid edit')
    expect(() => parseExpected({ contentGeneration: 0, prompts: [], toolsChanged: [] }, 'malformed'))
      .toThrow('invalid expectation')
    expect(() => parseExpected({ contentGeneration: 0, prompts: [{ rendered: '', inHistory: true, startsSeries: false,
      commits: [{ text: '', intent: { replace: -1 } }] }], toolsChanged: [], series: [] }, 'malformed'))
      .toThrow('invalid expectation')
    expect(() => resolve({ $log: '/99/data' }, cases[0] as PromptCase)).toThrow('does not exist')
    expect(() => resolve({ $log: '/06' }, cases[0] as PromptCase)).toThrow('does not exist')
    expect(() => resolve({ $log: '/length' }, cases[0] as PromptCase)).toThrow('does not exist')
    expect(() => resolve({ log$: '/0' }, cases[0] as PromptCase)).toThrow('invalid reference')
  })

  for (const entry of cases) {
    it(entry.id, () => {
      const { session, storedEventCount } = restorePlainLog(entry.log)
      expect(storedEventCount).toBe(entry.rowCount)
      const generation = session.surface.contentGeneration
      expect(generation).toBe(entry.ts.contentGeneration)
      const projection = new SystemPromptProjection(session)
      for (const { rendered, inHistory, startsSeries, commits } of entry.ts.prompts) {
        expect(projection.project(rendered, { inHistory, startsSeries }).map(commitOf), rendered).toStrictEqual(commits)
      }
      for (const { tools, changed } of entry.ts.toolsChanged) {
        expect(toolsChanged(session, resolve(tools, entry) as unknown[])).toBe(changed)
      }
      for (const query of entry.ts.series) {
        const tools = resolve(query.tools, entry) as unknown[]
        const startsSeries = query.declared
          || query.generationAtLastRequest !== generation
          || (!query.toolUpdateRoute && toolsChanged(session, tools))
        expect(startsSeries).toBe(query.startsSeries)
      }
    })
  }
})
