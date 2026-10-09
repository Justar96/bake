/**
 * Runs the shared cases in `conformance/session/fault-cases.json` through the
 * real JSONL backend with `compression: 'none'`, each in its own temporary
 * root. Each case seeds the files a crashed or failed Session writer left:
 * a file with its text, or a second hard link of a seeded file, which a
 * crash between publishing a log and removing its temporary file leaves.
 * Its steps then reopen the Session on one handle: a write `open`, `create`,
 * and the handle's `append`, `flush`, and `close`. After each step every
 * file beneath the root must have the expected text, a `session.lock` file
 * read only by its size, and no other file may exist.
 *
 * The states come from the Rust fault-injection sweep in
 * `rust/crates/bake-session/tests/fault_cases.rs`, which crashes or fails
 * the development writer `PlainLogFile` at every storage operation and
 * checks that each state it leaves is a case here, then runs that case's
 * steps on the crashed root. TypeScript's own fault behavior stays in its
 * persistence specs (D30); this table compares the runtimes only through
 * on-disk outcomes. The spec reads only the table.
 */

import { readFileSync } from 'node:fs'
import { link, mkdir, mkdtemp, readdir, readFile, rm, stat, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { dirname, join, sep } from 'node:path'
import { afterAll, beforeAll, describe, expect, it } from 'vitest'
import { Context } from '@deepseek-ai/cordis'
import { SessionId } from 'bake-session'
import type { SessionEvent, SessionHeader } from 'bake-session'
import { SessionPersistenceCorruptionError, SessionPersistenceNotFoundError } from 'bake-session-persistence'
import type { SessionHandle } from 'bake-session-persistence'
import JsonlSessionPersistence from '../src/index.ts'

const REPO = new URL('../../../../', import.meta.url)
const SCHEMA = 'bake/session-conformance/fault-cases'
const ORACLE = 'in an owned temporary root holding the seeded files, a state a crashed or failed Session writer leaves, run each step through the JSONL backend with compression none on one handle: a write open, create, or the open handle\'s append, flush, or close; after each step list every file beneath the root with its text and an empty session.lock by its size'
/** Both harnesses pin the table size, so a dropped case fails. */
const CASE_COUNT = 42
const LEASE_FILE = 'session.lock'
const SCENARIOS = [
  'create-flush', 'create-append', 'create-flush-appends', 'open-append', 'open-torn-append',
  'migrate-v0', 'migrate-v1', 'migrate-v2',
]
/** Classes are matched exactly. */
const CLASSES = new Map<string, abstract new (...args: never[]) => Error>([
  ['SessionPersistenceNotFoundError', SessionPersistenceNotFoundError],
  ['SessionPersistenceCorruptionError', SessionPersistenceCorruptionError],
])

type Outcome = { outcome: 'ok' } | { outcome: 'thrown'; class: string; message: string }
type Seed = { file: string; text: string } | { file: string; hardLinkTo: string }
type Tree = Record<string, string>
type Step =
  | { step: 'open'; id: string; ts: Outcome; tree: Tree }
  | { step: 'create'; header: unknown; ts: Outcome; tree: Tree }
  | { step: 'append'; events: unknown[]; ts: Outcome; tree: Tree }
  | { step: 'flush' | 'close'; ts: Outcome; tree: Tree }

interface FaultCase {
  id: string
  scenarios: string[]
  seed: Seed[]
  steps: Step[]
}

const STEP_KEYS: Record<Step['step'], string> = {
  open: 'id,step,tree,ts',
  create: 'header,step,tree,ts',
  append: 'events,step,tree,ts',
  flush: 'step,tree,ts',
  close: 'step,tree,ts',
}

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

function sortedKeys(value: object): string {
  return Object.keys(value).sort().join()
}

function parseOutcome(value: unknown, id: string): Outcome {
  if (isObject(value)) {
    if (sortedKeys(value) === 'outcome' && value.outcome === 'ok') return { outcome: 'ok' }
    if (sortedKeys(value) === 'class,message,outcome' && value.outcome === 'thrown'
      && typeof value.class === 'string' && CLASSES.has(value.class) && typeof value.message === 'string') {
      return { outcome: 'thrown', class: value.class, message: value.message }
    }
  }
  throw new Error(`${id}: invalid ts outcome ${JSON.stringify(value)}`)
}

function parseTree(value: unknown, id: string): Tree {
  if (!isObject(value) || !Object.values(value).every(text => typeof text === 'string')) {
    throw new Error(`${id}: invalid tree ${JSON.stringify(value)}`)
  }
  return value as Tree
}

function parseSeed(value: unknown, id: string): Seed {
  if (isObject(value) && sortedKeys(value) === 'file,text' && typeof value.file === 'string'
    && typeof value.text === 'string') return { file: value.file, text: value.text }
  if (isObject(value) && sortedKeys(value) === 'file,hardLinkTo' && typeof value.file === 'string'
    && typeof value.hardLinkTo === 'string') return { file: value.file, hardLinkTo: value.hardLinkTo }
  throw new Error(`${id}: invalid seed ${JSON.stringify(value)}`)
}

function parseStep(value: unknown, id: string): Step {
  if (!isObject(value) || typeof value.step !== 'string' || !(value.step in STEP_KEYS)
    || sortedKeys(value) !== STEP_KEYS[value.step as Step['step']]) {
    throw new Error(`${id}: invalid step ${JSON.stringify(value)}`)
  }
  const ts = parseOutcome(value.ts, id)
  const tree = parseTree(value.tree, id)
  switch (value.step) {
    case 'open':
      if (typeof value.id !== 'string') throw new Error(`${id}: invalid open id`)
      return { step: 'open', id: value.id, ts, tree }
    case 'create':
      return { step: 'create', header: value.header, ts, tree }
    case 'append':
      if (!Array.isArray(value.events)) throw new Error(`${id}: invalid events`)
      return { step: 'append', events: value.events, ts, tree }
    default:
      return { step: value.step as 'flush' | 'close', ts, tree }
  }
}

function loadTable(): FaultCase[] {
  const table: unknown = JSON.parse(readFileSync(new URL('conformance/session/fault-cases.json', REPO), 'utf8'))
  if (!isObject(table) || sortedKeys(table) !== 'cases,history,oracle,schema,version'
    || table.schema !== SCHEMA || table.version !== 1 || table.oracle !== ORACLE
    || !Array.isArray(table.history) || !table.history.every(line => typeof line === 'string')
    || !Array.isArray(table.cases)) {
    throw new Error('fault-cases.json: unexpected table envelope')
  }
  return table.cases.map((entry: unknown) => {
    if (!isObject(entry) || typeof entry.id !== 'string' || !Array.isArray(entry.scenarios)
      || !Array.isArray(entry.seed) || !Array.isArray(entry.steps)) {
      throw new Error(`invalid case ${JSON.stringify(entry)}`)
    }
    const { id } = entry
    const unknown = Object.keys(entry).filter(key => !['id', 'scenarios', 'seed', 'steps', 'note'].includes(key))
    if (unknown.length > 0) throw new Error(`${id}: unknown keys ${unknown.join()}`)
    if (entry.scenarios.length === 0 || !entry.scenarios.every(name => SCENARIOS.includes(name as string))) {
      throw new Error(`${id}: invalid scenarios`)
    }
    return {
      id,
      scenarios: entry.scenarios as string[],
      seed: entry.seed.map(seed => parseSeed(seed, id)),
      steps: entry.steps.map(step => parseStep(step, id)),
    }
  })
}

/** Run one step; an error of an unlisted class fails the case. */
async function outcome(run: () => Promise<void>): Promise<Outcome> {
  try {
    await run()
    return { outcome: 'ok' }
  } catch (error) {
    const constructor = (error as object).constructor
    const name = [...CLASSES].find(([, value]) => value === constructor)?.[0]
    if (name === undefined) throw error
    return { outcome: 'thrown', class: name, message: (error as Error).message }
  }
}

async function seedRoot(root: string, seeds: readonly Seed[]): Promise<void> {
  for (const seed of seeds) {
    if ('text' in seed) {
      const path = join(root, seed.file)
      await mkdir(dirname(path), { recursive: true })
      await writeFile(path, seed.text)
    }
  }
  for (const seed of seeds) {
    if ('hardLinkTo' in seed) await link(join(root, seed.hardLinkTo), join(root, seed.file))
  }
}

/** Every file beneath `root`, by `/`-joined relative path; a `session.lock` by its size. */
async function readTree(root: string): Promise<Tree> {
  const files: Tree = {}
  async function walk(dir: string, prefix: string): Promise<void> {
    for (const entry of await readdir(dir, { withFileTypes: true })) {
      const path = dir + sep + entry.name
      const relative = prefix + entry.name
      if (entry.isDirectory()) {
        await walk(path, `${relative}/`)
      } else if (entry.name === LEASE_FILE) {
        const { size } = await stat(path)
        files[relative] = size === 0 ? '' : `<${size} bytes>`
      } else {
        files[relative] = await readFile(path, 'utf8')
      }
    }
  }
  await walk(root, '')
  return files
}

const cases = loadTable()
let root: string

beforeAll(async () => {
  root = await mkdtemp(join(tmpdir(), 'bake-fault-conformance-'))
})

afterAll(async () => {
  await rm(root, { recursive: true, force: true })
})

describe('shared fault cases', () => {
  it('pin the table and name every scenario', () => {
    expect(cases).toHaveLength(CASE_COUNT)
    expect(new Set(cases.map(entry => entry.id)).size).toBe(CASE_COUNT)
    expect(new Set(cases.flatMap(entry => entry.scenarios))).toEqual(new Set(SCENARIOS))
  })

  it('refuses malformed outcomes, steps, and seeds', () => {
    expect(() => parseOutcome({ outcome: 'thrown', class: 'Error', message: 'x' }, 'malformed')).toThrow('invalid ts outcome')
    expect(() => parseStep({ step: 'flush', id: 'x', ts: { outcome: 'ok' }, tree: {} }, 'malformed')).toThrow('invalid step')
    expect(() => parseSeed({ file: 'x', hardLinkTo: 1 }, 'malformed')).toThrow('invalid seed')
  })

  for (const entry of cases) {
    it(entry.id, async () => {
      const caseRoot = await mkdtemp(join(root, 'case-'))
      const ctx = new Context()
      let handle: SessionHandle | undefined
      try {
        await seedRoot(caseRoot, entry.seed)
        await ctx.plugin(JsonlSessionPersistence, { root: caseRoot, compression: 'none' })
        for (const [index, step] of entry.steps.entries()) {
          const context = `step ${index} ${step.step}`
          let actual: Outcome
          switch (step.step) {
            case 'open':
              actual = await outcome(async () => {
                handle = await ctx.sessionPersistence.open(SessionId(step.id), 'write')
              })
              break
            case 'create':
              actual = await outcome(async () => {
                handle = await ctx.sessionPersistence.create(step.header as SessionHeader)
              })
              break
            case 'append':
              actual = await outcome(() => (handle as SessionHandle).append(step.events as SessionEvent[]))
              break
            case 'flush':
              actual = await outcome(() => (handle as SessionHandle).flush())
              break
            case 'close':
              actual = await outcome(() => (handle as SessionHandle).close())
              handle = undefined
              break
          }
          expect(actual, context).toStrictEqual(step.ts)
          expect(await readTree(caseRoot), context).toStrictEqual(step.tree)
        }
      } finally {
        await handle?.close()
        await ctx.fiber.dispose()
      }
    })
  }
})
