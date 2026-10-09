/**
 * Runs the shared cases in `conformance/session/number-cases.json`. A
 * printing case requires `JSON.stringify(JSON.parse(lexeme))` to write the
 * hand-written text, ECMAScript `Number::toString` of the parsed double. A
 * derivation case requires `JSON.stringify` of each request the real
 * `replayRequests` helper in `runtime-fixture.ts` returns to equal the
 * expected bytes, and so of each request derived from the Session the
 * production read path restores: `scanLog`, `validateStoredEvents`,
 * `interruptedTurnClosers`, then, for each Assistant settlement,
 * `Session.fromRestore` over its prefix with the catalog's message
 * projections and `foldRequestHeader`, assembled as `replayRequests`
 * assembles a request, the composition
 * `restored-request-derivation-conformance.spec.ts` uses. The development
 * Rust `json_text`, `replay_requests`, and `replay_restored_requests` in
 * `rust/crates/bake-session` check the same table. A `rust` marker names the
 * number limit with which Rust refuses a spelling no writer produces; this
 * spec still asserts TypeScript's outcome there.
 */

import { readFileSync } from 'node:fs'
import { describe, expect, it } from 'vitest'
import { Session, SessionId, SessionLogOffset, foldRequestHeader, interruptedTurnClosers } from 'bake-session'
import type { SessionEvent } from 'bake-session'
import { currentSessionMessageProjections } from 'bake-session-format-catalog/message-projections'
import { validateStoredEvents } from 'bake-session-persistence'
import { scanLog } from 'bake-session-persistence-jsonl/src/format.ts'
import { snapshotJsonValue } from 'bake-util-values'
import { replayRequests } from './runtime-fixture.ts'

const REPO = new URL('../../../../', import.meta.url)
const SCHEMA = 'bake/session-conformance/number-cases'
const ORACLE = 'JSON.stringify(JSON.parse(lexeme)) for printing; for derivation, JSON.stringify of each request replayRequests(log) returns in packages/core/agent-loop/tests/runtime-fixture.ts and of each request number-conformance.spec.ts derives from the Session restorePlainLog(log) restores, as restored-request-derivation-conformance.spec.ts derives one'
/** Both harnesses pin the table sizes, so a dropped case fails. */
const PRINTING_COUNT = 56
const DERIVATION_COUNT = 5

interface PrintingCase { lexeme: string; text: string }

interface DerivationCase {
  id: string
  lines: string[]
  requests: string[]
  rust?: { limit: 'number'; seq: number }
}

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

function sortedKeys(value: object): string {
  return Object.keys(value).sort().join()
}

function isLine(value: unknown): value is string {
  return typeof value === 'string' && !value.includes('\n')
}

function isLines(value: unknown): value is string[] {
  return Array.isArray(value) && value.every(isLine)
}

function isCount(value: unknown): value is number {
  return Number.isSafeInteger(value) && (value as number) >= 0 && !Object.is(value, -0)
}

function loadTable(): { printing: PrintingCase[]; derivation: DerivationCase[] } {
  const table: unknown = JSON.parse(readFileSync(new URL('conformance/session/number-cases.json', REPO), 'utf8'))
  if (!isObject(table) || sortedKeys(table) !== 'derivation,history,oracle,printing,schema,version'
    || table.schema !== SCHEMA || table.version !== 1 || table.oracle !== ORACLE || !isLines(table.history)
    || !Array.isArray(table.printing) || !Array.isArray(table.derivation)) {
    throw new Error('number-cases.json does not match its version-1 schema')
  }
  const printing = table.printing.map((entry: unknown): PrintingCase => {
    if (!isObject(entry) || sortedKeys(entry) !== 'lexeme,text' || !isLine(entry.lexeme) || !isLine(entry.text)) {
      throw new Error(`invalid printing case ${JSON.stringify(entry)}`)
    }
    return { lexeme: entry.lexeme, text: entry.text }
  })
  const derivation = table.derivation.map((entry: unknown): DerivationCase => {
    if (!isObject(entry) || typeof entry.id !== 'string') throw new Error(`invalid case ${JSON.stringify(entry)}`)
    const { id } = entry
    const unknown = Object.keys(entry).filter(key => !['id', 'lines', 'requests', 'rust', 'note'].includes(key))
    if (unknown.length > 0) throw new Error(`${id}: unknown keys ${unknown.join()}`)
    if (entry.note !== undefined && typeof entry.note !== 'string') throw new Error(`${id}: invalid note`)
    if (!isLines(entry.lines) || !isLines(entry.requests)) throw new Error(`${id}: invalid lines or requests`)
    const { rust } = entry
    if (rust !== undefined && !(isObject(rust) && sortedKeys(rust) === 'limit,seq' && rust.limit === 'number' && isCount(rust.seq))) {
      throw new Error(`${id}: invalid rust marker ${JSON.stringify(rust)}`)
    }
    return { id, lines: entry.lines, requests: entry.requests, ...(rust === undefined ? {} : { rust: rust as NonNullable<DerivationCase['rust']> }) }
  })
  return { printing, derivation }
}

/**
 * The requests of a restored unseeded Session, one per Assistant settlement,
 * assembled as `replayRequests` assembles them.
 */
function deriveRestored(log: Buffer): unknown[] {
  const { meta, inheritedEventCount, events } = scanLog(log)
  validateStoredEvents(meta, events)
  const restored: SessionEvent[] = [...events, ...interruptedTurnClosers(events)]
  Session.fromRestore(SessionId(meta.id), restored, meta, SessionLogOffset(inheritedEventCount), 'detached',
    currentSessionMessageProjections)
  return restored.filter(event => event.type === 'step/start').flatMap((start) => {
    const settlements = restored.filter(event =>
      (event.type === 'assistant/message' || event.type === 'assistant/attempt')
      && event.data.turn === start.data.turn
      && event.data.step === start.data.step)
    return settlements.map((settlement) => {
      const prefix = restored.slice(0, settlement.seq)
      const session = Session.fromRestore(SessionId(meta.id), prefix, meta,
        SessionLogOffset(inheritedEventCount), 'detached', currentSessionMessageProjections)
      const header = foldRequestHeader(prefix)
      if (header === undefined) throw new Error('no request header')
      return snapshotJsonValue<unknown>({
        ...header.config,
        messages: session.deriveMessages(),
        toolHistory: session.toolHistory(),
        ...header.tools !== undefined ? { tools: header.tools } : {},
        sessionId: meta.id,
      })
    })
  })
}

const { printing, derivation } = loadTable()

describe('shared number cases', () => {
  it('pins the tables and witnesses the number limit', () => {
    expect(printing).toHaveLength(PRINTING_COUNT)
    expect(new Set(printing.map(entry => entry.lexeme)).size).toBe(PRINTING_COUNT)
    expect(derivation).toHaveLength(DERIVATION_COUNT)
    expect(new Set(derivation.map(entry => entry.id)).size).toBe(DERIVATION_COUNT)
    expect(derivation.some(entry => entry.rust !== undefined), 'the number limit is witnessed').toBe(true)
  })

  for (const entry of printing) {
    it(`prints ${entry.lexeme}`, () => {
      expect(JSON.stringify(JSON.parse(entry.lexeme))).toBe(entry.text)
    })
  }

  for (const entry of derivation) {
    it(entry.id, () => {
      const log = Buffer.from(entry.lines.map(line => `${line}\n`).join(''))
      expect(replayRequests(log).map(request => JSON.stringify(request))).toStrictEqual(entry.requests)
      expect(deriveRestored(log).map(request => JSON.stringify(request))).toStrictEqual(entry.requests)
    })
  }
})
