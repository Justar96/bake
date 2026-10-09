/**
 * Runs the shared cases in `conformance/session/json-parse-cases.json`
 * through `JSON.parse`, which `scanLog` in `src/format.ts` applies to every
 * header and row record, and `JSON.stringify`, which writes a derived request.
 * The Rust `parse_json` and `json_text` in `rust/crates/bake-session` check the
 * same table, so every outcome here is the oracle for the Rust expectation. A
 * `rust` override names a refusal the Rust parser keeps; TypeScript still
 * asserts its own outcome.
 */

import { readFileSync } from 'node:fs'
import { describe, expect, it } from 'vitest'

const REPO = new URL('../../../../', import.meta.url)
const SCHEMA = 'bake/session-conformance/json-parse-cases'
const ORACLE = 'JSON.stringify(JSON.parse(text)), the parse scanLog runs on every record and the serialization a request takes; a thrown SyntaxError is syntax-error'
/** Both harnesses pin the table size, so a dropped case fails. */
const CASE_COUNT = 50
const REFUSALS = ['lone-surrogate', 'number-out-of-range']

type Outcome = { outcome: 'parsed'; text: string } | { outcome: 'syntax-error' }

interface ParseCase {
  id: string
  text: string
  ts: Outcome
  refusal?: string
}

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

function sortedKeys(value: Record<string, unknown>): string {
  return Object.keys(value).sort().join()
}

function parseOutcome(value: unknown, id: string): Outcome {
  if (isObject(value) && sortedKeys(value) === 'outcome,text' && value.outcome === 'parsed'
    && typeof value.text === 'string') {
    return { outcome: 'parsed', text: value.text }
  }
  if (isObject(value) && sortedKeys(value) === 'outcome' && value.outcome === 'syntax-error') {
    return { outcome: 'syntax-error' }
  }
  throw new Error(`${id}: invalid ts outcome`)
}

function parseCase(value: unknown): ParseCase {
  if (!isObject(value) || typeof value.id !== 'string' || typeof value.text !== 'string') {
    throw new Error('json-parse case needs a string id and text')
  }
  const { id, text } = value
  const keys = sortedKeys(value)
  if (keys !== 'id,text,ts' && keys !== 'id,rust,text,ts') throw new Error(`${id}: unexpected members ${keys}`)
  const ts = parseOutcome(value.ts, id)
  if (value.rust === undefined) return { id, text, ts }
  const { rust } = value
  if (!isObject(rust) || sortedKeys(rust) !== 'refusal' || typeof rust.refusal !== 'string'
    || !REFUSALS.includes(rust.refusal)) {
    throw new Error(`${id}: invalid rust override`)
  }
  return { id, text, ts, refusal: rust.refusal }
}

function loadTable(): ParseCase[] {
  const table: unknown = JSON.parse(readFileSync(new URL('conformance/session/json-parse-cases.json', REPO), 'utf8'))
  if (!isObject(table) || sortedKeys(table) !== 'cases,history,oracle,schema,version' || table.schema !== SCHEMA
    || table.version !== 1 || table.oracle !== ORACLE || !Array.isArray(table.cases)
    || !Array.isArray(table.history) || !table.history.every(line => typeof line === 'string')) {
    throw new Error('json-parse-cases.json does not match its version-1 schema')
  }
  return table.cases.map(parseCase)
}

const cases = loadTable()

describe('shared JSON.parse cases', () => {
  it('pin the table and witness every refusal', () => {
    expect(cases).toHaveLength(CASE_COUNT)
    expect(new Set(cases.map(entry => entry.id)).size).toBe(CASE_COUNT)
    expect(new Set(cases.flatMap(entry => entry.refusal === undefined ? [] : [entry.refusal]))).toEqual(new Set(REFUSALS))
  })

  for (const entry of cases) {
    it(entry.id, () => {
      let parsed: unknown
      try {
        parsed = JSON.parse(entry.text)
      } catch (error) {
        expect(error).toBeInstanceOf(SyntaxError)
        expect({ outcome: 'syntax-error' }).toEqual(entry.ts)
        return
      }
      expect({ outcome: 'parsed', text: JSON.stringify(parsed) }).toEqual(entry.ts)
    })
  }
})
