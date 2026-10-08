/**
 * Runs the shared cases in `conformance/session/relationships-cases.json`
 * through `assertReleasedArtifactRelationships` from `relationships.ts`. The
 * development Rust `check_released_relationships` in `rust/crates/bake-session`
 * checks the same table. Each case's events are `JSON.parse`d rows; the spec
 * first checks them against the documented precondition, which must hold
 * exactly for cases not marked `outsidePrecondition`, and then records the
 * index of the event the check was visiting when it threw. A `rust`
 * native-subset marker names a case Rust deliberately does not decide;
 * TypeScript still asserts its own outcome.
 */

import { readFileSync } from 'node:fs'
import { SessionFormatError } from 'bake-session-format'
import type { SessionFormatArtifact, SessionFormatEvent } from 'bake-session-format'
import { describe, expect, it } from 'vitest'
import {
  RELEASED_V0_EVENT_DISPOSITIONS,
  assertReleasedArtifactRelationships,
  assertReleasedEventPayload,
} from '../src/index.ts'
import type { ReleasedRelationshipExtensions } from '../src/relationships.ts'

const REPO = new URL('../../../../', import.meta.url)
const SCHEMA = 'bake/session-format-conformance/relationships-cases'
const ORACLE = 'assertReleasedArtifactRelationships({header, inheritedEventCount, events}, extensions) from session-format-v0-to-v1'
/** Both harnesses pin the table size, so a dropped case fails. */
const CASE_COUNT = 182
/** Native limits: input outside the precondition, and an inherited member `deepEqualJson` reads through `in`. */
const LIMITS = ['precondition', 'prototype-member']
const SURFACE_TYPES = new Set(['user/message', 'assistant/message', 'tool/result'])

type Outcome =
  | { outcome: 'accepted' }
  | { outcome: 'rejected'; at: number; message: string }
  | { outcome: 'threw'; error: 'TypeError' }

interface RelationshipCase {
  id: string
  header: string
  inheritedEventCount: number
  extensions: ReleasedRelationshipExtensions
  events: string[]
  outsidePrecondition: boolean
  ts: Outcome
  limit?: string
}

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

function sortedKeys(value: Record<string, unknown>): string {
  return Object.keys(value).sort().join()
}

function isIndex(value: unknown): value is number {
  return Number.isSafeInteger(value) && (value as number) >= 0
}

function parseOutcome(value: unknown, id: string): Outcome {
  if (isObject(value)) {
    const keys = sortedKeys(value)
    if (value.outcome === 'accepted' && keys === 'outcome') return value as Outcome
    if (value.outcome === 'rejected' && keys === 'at,message,outcome' && isIndex(value.at) && typeof value.message === 'string') {
      return value as Outcome
    }
    if (value.outcome === 'threw' && keys === 'error,outcome' && value.error === 'TypeError') return value as Outcome
  }
  throw new Error(`${id}: invalid ts outcome ${JSON.stringify(value)}`)
}

function parseExtensions(value: unknown, id: string): ReleasedRelationshipExtensions {
  if (value === undefined) return {}
  if (!isObject(value)) throw new Error(`${id}: invalid extensions`)
  const unknown = Object.keys(value).filter(key => !['stepEvents', 'preservedSourceTitleRequestText', 'legacyInterruptedTurnRestart'].includes(key))
  if (unknown.length > 0) throw new Error(`${id}: unknown extensions ${unknown.join()}`)
  const { stepEvents, preservedSourceTitleRequestText, legacyInterruptedTurnRestart } = value
  if (stepEvents !== undefined && (!Array.isArray(stepEvents) || !stepEvents.every(type => typeof type === 'string'))) {
    throw new Error(`${id}: stepEvents must be strings`)
  }
  if (preservedSourceTitleRequestText !== undefined && preservedSourceTitleRequestText !== true) throw new Error(`${id}: invalid preservedSourceTitleRequestText`)
  if (legacyInterruptedTurnRestart !== undefined && legacyInterruptedTurnRestart !== true) throw new Error(`${id}: invalid legacyInterruptedTurnRestart`)
  return {
    ...(stepEvents === undefined ? {} : { stepEvents: new Set(stepEvents as string[]) }),
    ...(preservedSourceTitleRequestText === true ? { preservedSourceTitleRequestText } : {}),
    ...(legacyInterruptedTurnRestart === true ? { legacyInterruptedTurnRestart } : {}),
  }
}

function parseCase(value: unknown): RelationshipCase {
  if (!isObject(value) || typeof value.id !== 'string') throw new Error(`invalid case ${JSON.stringify(value)}`)
  const { id } = value
  const allowed = ['id', 'header', 'inheritedEventCount', 'extensions', 'events', 'outsidePrecondition', 'ts', 'rust', 'note']
  const unknown = Object.keys(value).filter(key => !allowed.includes(key))
  if (unknown.length > 0) throw new Error(`${id}: unknown keys ${unknown.join()}`)
  if (typeof value.header !== 'string' || !Array.isArray(value.events) || !value.events.every(row => typeof row === 'string')) {
    throw new Error(`${id}: header and events must be JSON text`)
  }
  if (!isIndex(value.inheritedEventCount)) throw new Error(`${id}: invalid inheritedEventCount`)
  if (value.outsidePrecondition !== undefined && value.outsidePrecondition !== true) throw new Error(`${id}: invalid outsidePrecondition`)
  if (value.note !== undefined && typeof value.note !== 'string') throw new Error(`${id}: invalid note`)
  const ts = parseOutcome(value.ts, id)
  let limit: string | undefined
  if (Object.hasOwn(value, 'rust')) {
    const { rust } = value
    if (!isObject(rust) || sortedKeys(rust) !== 'at,limit,outcome' || rust.outcome !== 'native-subset'
      || !LIMITS.includes(rust.limit as string) || !isIndex(rust.at)) {
      throw new Error(`${id}: rust may only name a native-subset limit and its event index`)
    }
    limit = rust.limit as string
  }
  if (ts.outcome === 'threw' && limit === undefined) throw new Error(`${id}: a TypeError needs a Rust limit`)
  if (limit === 'precondition' && value.outsidePrecondition !== true) throw new Error(`${id}: a precondition limit needs outsidePrecondition`)
  return {
    id,
    header: value.header,
    inheritedEventCount: value.inheritedEventCount,
    extensions: parseExtensions(value.extensions, id),
    events: value.events as string[],
    outsidePrecondition: value.outsidePrecondition === true,
    ts,
    ...(limit === undefined ? {} : { limit }),
  }
}

function loadCases(): RelationshipCase[] {
  const table: unknown = JSON.parse(readFileSync(new URL('conformance/session/relationships-cases.json', REPO), 'utf8'))
  if (!isObject(table) || sortedKeys(table) !== 'cases,history,oracle,schema,version'
    || table.schema !== SCHEMA || table.version !== 1 || table.oracle !== ORACLE || !Array.isArray(table.cases)
    || !Array.isArray(table.history) || !table.history.every(entry => typeof entry === 'string')) {
    throw new Error('relationships-cases.json does not match its version-1 schema')
  }
  const cases = table.cases.map(parseCase)
  if (new Set(cases.map(entry => entry.id)).size !== cases.length) throw new Error('case ids must be unique')
  return cases
}

/**
 * The precondition `relationships.rs` documents: object events with a string
 * type and dense seqs, own released types passing the version-1 payload
 * check, and an `"append"` or object `surfaceOp` on surface events.
 */
function assertPrecondition(events: readonly unknown[]): void {
  events.forEach((event, index) => {
    if (!isObject(event) || typeof event.type !== 'string' || event.seq !== index) throw new Error(`event ${index} envelope`)
    if (Object.hasOwn(RELEASED_V0_EVENT_DISPOSITIONS, event.type)) {
      assertReleasedEventPayload(event as unknown as SessionFormatEvent, 1)
    }
    const operation = event.surfaceOp
    if (SURFACE_TYPES.has(event.type) && operation !== undefined && operation !== 'append' && !isObject(operation)) {
      throw new Error(`event ${index} surfaceOp`)
    }
  })
}

/** The events with an iterator that records the index the check visits. */
function trackedEvents(events: readonly SessionFormatEvent[]): { events: SessionFormatEvent[]; at: () => number } {
  let visiting = -1
  const tracked = [...events]
  Object.defineProperty(tracked, Symbol.iterator, {
    value: function* iterate() {
      for (let index = 0; index < events.length; index += 1) {
        visiting = index
        yield events[index]
      }
    },
  })
  return { events: tracked, at: () => visiting }
}

function deepFreeze<T>(value: T): T {
  if (typeof value === 'object' && value !== null) {
    for (const item of Object.values(value)) deepFreeze(item)
    Object.freeze(value)
  }
  return value
}

function check(entry: RelationshipCase, header: unknown, events: readonly SessionFormatEvent[]): Outcome {
  const tracked = trackedEvents(events)
  const artifact = {
    header,
    inheritedEventCount: entry.inheritedEventCount,
    events: tracked.events,
  } as unknown as SessionFormatArtifact
  try {
    assertReleasedArtifactRelationships(artifact, entry.extensions)
  } catch (error) {
    if (error instanceof SessionFormatError && error.constructor === SessionFormatError) {
      return { outcome: 'rejected', at: tracked.at(), message: error.message }
    }
    if (error instanceof TypeError) return { outcome: 'threw', error: 'TypeError' }
    throw error
  }
  return { outcome: 'accepted' }
}

const cases = loadCases()

describe('shared released relationship cases', () => {
  it('pin the table size and witness every native limit', () => {
    expect(cases).toHaveLength(CASE_COUNT)
    expect(new Set(cases.flatMap(entry => entry.limit ?? []))).toEqual(new Set(LIMITS))
  })

  for (const entry of cases) {
    it(entry.id, () => {
      const header: unknown = deepFreeze(JSON.parse(entry.header))
      const events = deepFreeze(entry.events.map(row => JSON.parse(row) as SessionFormatEvent))
      if (entry.outsidePrecondition) {
        expect(() => assertPrecondition(events), `${entry.id} precondition`).toThrow()
      } else {
        assertPrecondition(events)
      }
      expect(check(entry, header, events), entry.id).toEqual(entry.ts)
    })
  }
})
