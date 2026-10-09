/**
 * Runs the shared cases in `conformance/session/plain-log-file-cases.json`
 * that apply to this host through the real JSONL backend with
 * `compression: 'none'`, each in its own temporary root. A case seeds files
 * and directories, then runs its steps in order: `create` and a write `open`
 * start a handle, and `append`, `flush`, and `close` use it. A step names its
 * handle, `a` unless `handle` says `b`, and each handle is a separate backend
 * instance in its own Context over the same root, so only the kernel write
 * lock arbitrates between them. After each step every file beneath the root
 * must have the expected text, a `session.lock` file read only by its size,
 * since Windows refuses to read a locked range, and no other file may exist;
 * directories are not compared.
 * The development Rust model `PlainLogFile` in `rust/crates/bake-session`
 * checks the same table. `ts` is the step's outcome, or the thrown class
 * with its exact message, in which `{src}` stands for the case root joined
 * with the create or open step's `src` path, and `{srcJson}` for that path
 * as `JSON.stringify` spells it. A `rust` override names a native limit or
 * marks the step outside the Rust model's domain; it ends the case in Rust,
 * and TypeScript still asserts every step. The spec reads only the table.
 */

import { readFileSync } from 'node:fs'
import { mkdir, mkdtemp, readdir, readFile, rm, stat, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { dirname, join, resolve, sep } from 'node:path'
import { afterAll, beforeAll, describe, expect, it } from 'vitest'
import { Context } from '@deepseek-ai/cordis'
import { SessionId } from 'bake-session'
import type { SessionEvent, SessionHeader, SessionLogOffset } from 'bake-session'
import { SessionFormatError } from 'bake-session-format'
import {
  SessionAlreadyExistsError, SessionAlreadyOwnedError, SessionFormatUnsupportedError,
  SessionPersistenceCorruptionError, SessionPersistenceNotFoundError,
} from 'bake-session-persistence'
import type { SessionHandle } from 'bake-session-persistence'
import JsonlSessionPersistence from '../src/index.ts'

const REPO = new URL('../../../../', import.meta.url)
const SCHEMA = 'bake/session-conformance/plain-log-file-cases'
const ORACLE = 'in an owned temporary root holding the seeded entries, run each step through the JSONL backend with compression none on the step\'s handle, a or b, each its own backend instance over the root: create, a write open, or the open handle\'s append, flush, or close; after each step list every file beneath the root with its text, an empty session.lock by its size'
/** Both harnesses pin the table size, so a dropped case fails. */
const CASE_COUNT = 75
const LIMITS = [
  'empty-id', 'encode', 'seq-value', 'windows-name', 'non-utf8-name', 'newer-generation', 'identity', 'scan', 'migration/v2-codec-recovery',
]
/** A `rust` override marking a step outside the Rust model's domain. */
const OUTSIDE_DOMAIN = 'outside-domain'
const LEASE_FILE = 'session.lock'
const HANDLES = ['a', 'b'] as const
/** Classes are matched exactly. */
const CLASSES = new Map<string, abstract new (...args: never[]) => Error>([
  ['Error', Error],
  ['TypeError', TypeError],
  ['SessionFormatError', SessionFormatError],
  ['SessionAlreadyExistsError', SessionAlreadyExistsError],
  ['SessionAlreadyOwnedError', SessionAlreadyOwnedError],
  ['SessionPersistenceNotFoundError', SessionPersistenceNotFoundError],
  ['SessionPersistenceCorruptionError', SessionPersistenceCorruptionError],
  ['SessionFormatUnsupportedError', SessionFormatUnsupportedError],
])

type Platform = 'posix' | 'linux' | 'win32'
type Outcome = { outcome: 'ok' } | { outcome: 'thrown'; class: string; message?: string }
type Seed = { file: string; text: string } | { dir: string; rawNameHex: string }
type Tree = Record<string, string>
type Handle = typeof HANDLES[number]
type Step = { handle: Handle } & (
  | { step: 'create'; header: unknown; inheritedEventCount?: number; src?: string; ts: Outcome; rust?: string; tree: Tree }
  | { step: 'open'; id: string; src?: string; ts: Outcome; rust?: string; tree: Tree }
  | { step: 'append'; events: unknown[]; ts: Outcome; rust?: string; tree: Tree }
  | { step: 'flush'; ts: Outcome; rust?: string; tree: Tree }
  | { step: 'close'; ts: Outcome; tree: Tree })

interface FileCase {
  id: string
  platforms?: Platform[]
  seed: Seed[]
  steps: Step[]
}

const STEP_KEYS: Record<Step['step'], string[]> = {
  create: ['step', 'handle', 'header', 'inheritedEventCount', 'src', 'ts', 'rust', 'tree'],
  open: ['step', 'handle', 'id', 'src', 'ts', 'rust', 'tree'],
  append: ['step', 'handle', 'events', 'ts', 'rust', 'tree'],
  flush: ['step', 'handle', 'ts', 'rust', 'tree'],
  close: ['step', 'handle', 'ts', 'tree'],
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
    if (keys === 'outcome' && value.outcome === 'ok') return { outcome: 'ok' }
    if (value.outcome === 'thrown' && typeof value.class === 'string' && CLASSES.has(value.class)) {
      if (keys === 'class,message,outcome' && typeof value.message === 'string') {
        return { outcome: 'thrown', class: value.class, message: value.message }
      }
      if (keys === 'class,outcome') return { outcome: 'thrown', class: value.class }
    }
  }
  throw new Error(`${id}: invalid ts outcome ${JSON.stringify(value)}`)
}

/** A limit name, `OUTSIDE_DOMAIN`, or `undefined` without an override. */
function parseRust(value: unknown, id: string): string | undefined {
  if (value === undefined) return undefined
  if (isObject(value) && sortedKeys(value) === 'outcome' && value.outcome === OUTSIDE_DOMAIN) return OUTSIDE_DOMAIN
  if (isObject(value) && sortedKeys(value) === 'limit,outcome' && value.outcome === 'native-subset'
    && LIMITS.includes(value.limit as string)) return value.limit as string
  throw new Error(`${id}: invalid rust override ${JSON.stringify(value)}`)
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
  if (isObject(value) && sortedKeys(value) === 'dir,rawNameHex' && typeof value.dir === 'string'
    && typeof value.rawNameHex === 'string' && /^(?:[0-9a-f]{2})+$/u.test(value.rawNameHex)) {
    return { dir: value.dir, rawNameHex: value.rawNameHex }
  }
  throw new Error(`${id}: invalid seed ${JSON.stringify(value)}`)
}

/**
 * Parse one step. Its handle is `a` unless named, a throw without a message
 * needs a limit, and `close` always succeeds.
 */
function parseStep(value: unknown, id: string): Step {
  if (!isObject(value) || typeof value.step !== 'string' || !Object.hasOwn(STEP_KEYS, value.step)) {
    throw new Error(`${id}: invalid step ${JSON.stringify(value)}`)
  }
  const name = value.step as Step['step']
  const allowed = STEP_KEYS[name]
  if (!Object.keys(value).every(key => allowed.includes(key))) {
    throw new Error(`${id}: invalid ${name} step ${JSON.stringify(value)}`)
  }
  const ts = parseOutcome(value.ts, id)
  const rust = parseRust(value.rust, id)
  const tree = parseTree(value.tree, id)
  if (ts.outcome === 'thrown' && ts.message === undefined && rust === undefined) {
    throw new Error(`${id}: a throw without a message needs a rust limit`)
  }
  const handle = value.handle ?? 'a'
  const src = value.src
  const placeholder = ts.outcome === 'thrown'
    && (ts.message?.includes('{src}') === true || ts.message?.includes('{srcJson}') === true)
  if ((src !== undefined && typeof src !== 'string') || placeholder !== (src !== undefined)) {
    throw new Error(`${id}: invalid src`)
  }
  const source = src === undefined ? {} : { src }
  if (!HANDLES.includes(handle as Handle)) throw new Error(`${id}: invalid handle ${JSON.stringify(handle)}`)
  const extra = { handle: handle as Handle, ...(rust === undefined ? {} : { rust }) }
  switch (name) {
    case 'create': {
      const count = value.inheritedEventCount
      if (!Object.hasOwn(value, 'header') || (count !== undefined && !Number.isSafeInteger(count))) {
        throw new Error(`${id}: invalid create`)
      }
      return {
        step: 'create', header: value.header, ts, tree, ...extra, ...source,
        ...(count === undefined ? {} : { inheritedEventCount: count as number }),
      }
    }
    case 'open':
      if (typeof value.id !== 'string') throw new Error(`${id}: invalid open`)
      return { step: 'open', id: value.id, ts, tree, ...extra, ...source }
    case 'append':
      if (!Array.isArray(value.events)) throw new Error(`${id}: invalid append`)
      return { step: 'append', events: value.events, ts, tree, ...extra }
    case 'flush':
      return { step: 'flush', ts, tree, ...extra }
    case 'close':
      if (ts.outcome !== 'ok') throw new Error(`${id}: close succeeds`)
      return { step: 'close', handle: handle as Handle, ts, tree }
  }
}

function loadTable(): FileCase[] {
  const table: unknown = JSON.parse(readFileSync(new URL('conformance/session/plain-log-file-cases.json', REPO), 'utf8'))
  if (!isObject(table) || sortedKeys(table) !== 'cases,history,oracle,schema,version' || table.schema !== SCHEMA
    || table.version !== 9 || table.oracle !== ORACLE || !Array.isArray(table.cases)
    || !Array.isArray(table.history) || !table.history.every(line => typeof line === 'string')) {
    throw new Error('plain-log-file-cases.json does not match its version-9 schema')
  }
  return table.cases.map((entry: unknown): FileCase => {
    if (!isObject(entry) || typeof entry.id !== 'string' || !Array.isArray(entry.steps) || !Array.isArray(entry.seed)) {
      throw new Error(`invalid case ${JSON.stringify(entry)}`)
    }
    const { id } = entry
    const unknown = Object.keys(entry).filter(key => !['id', 'platforms', 'platformReason', 'seed', 'steps', 'note'].includes(key))
    if (unknown.length > 0) throw new Error(`${id}: unknown keys ${unknown.join()}`)
    if (entry.note !== undefined && typeof entry.note !== 'string') throw new Error(`${id}: invalid note`)
    if ((entry.platforms === undefined) !== (entry.platformReason === undefined)
      || (entry.platformReason !== undefined && typeof entry.platformReason !== 'string')
      || (entry.platforms !== undefined && (!Array.isArray(entry.platforms)
        || !entry.platforms.every(platform => ['posix', 'linux', 'win32'].includes(platform as string))))) {
      throw new Error(`${id}: invalid platforms`)
    }
    const steps = entry.steps.map(step => parseStep(step, id))
    const open = new Set<Handle>()
    for (const step of steps) {
      if (step.step === 'create' || step.step === 'open') {
        if (open.has(step.handle)) throw new Error(`${id}: one open value per handle`)
        if (step.ts.outcome === 'ok') open.add(step.handle)
      } else if (!open.has(step.handle)) {
        throw new Error(`${id}: ${step.step} needs an open handle`)
      } else if (step.step === 'close') {
        open.delete(step.handle)
      }
    }
    return {
      id,
      ...(entry.platforms === undefined ? {} : { platforms: entry.platforms as Platform[] }),
      seed: entry.seed.map(seed => parseSeed(seed, id)),
      steps,
    }
  })
}

function applies(entry: FileCase): boolean {
  if (entry.platforms === undefined) return true
  return entry.platforms.some(platform => platform === 'linux'
    ? process.platform === 'linux'
    : platform === 'win32' ? process.platform === 'win32' : process.platform !== 'win32')
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

/**
 * Compare a step's outcome, rendering `{src}` in the expected message as the
 * backend's resolved root joined with `src`, and `{srcJson}` as that path's
 * `JSON.stringify` spelling.
 */
function expectOutcome(actual: Outcome, expected: Outcome, root: string, src: string | undefined, context: string): void {
  if (expected.outcome === 'thrown' && expected.message === undefined) {
    expect(actual.outcome === 'thrown' ? actual.class : actual, context).toBe(expected.class)
  } else if (expected.outcome === 'thrown' && src !== undefined) {
    const source = resolve(root, ...src.split('/'))
    const message = expected.message?.replaceAll('{srcJson}', JSON.stringify(source)).replaceAll('{src}', source)
    expect(actual, context).toStrictEqual({ ...expected, message })
  } else {
    expect(actual, context).toStrictEqual(expected)
  }
}

async function seedRoot(root: string, seeds: readonly Seed[]): Promise<void> {
  for (const seed of seeds) {
    if ('file' in seed) {
      const path = join(root, seed.file)
      await mkdir(dirname(path), { recursive: true })
      await writeFile(path, seed.text)
    } else {
      const dir = join(root, seed.dir)
      await mkdir(dir, { recursive: true })
      await mkdir(Buffer.concat([Buffer.from(dir + sep), Buffer.from(seed.rawNameHex, 'hex')]))
    }
  }
}

/**
 * Every file beneath `root`, by `/`-joined relative path; a `session.lock`
 * file is listed as empty when its size is 0 and otherwise by its size.
 * Listing by bytes reaches a directory whose name is not UTF-8.
 */
async function readTree(root: string): Promise<Tree> {
  const files: Tree = {}
  async function walk(dir: Buffer, prefix: string): Promise<void> {
    for (const entry of await readdir(dir, { withFileTypes: true, encoding: 'buffer' })) {
      const path = Buffer.concat([dir, Buffer.from(sep), entry.name])
      const relative = prefix + entry.name.toString('utf8')
      if (entry.isDirectory()) {
        await walk(path, `${relative}/`)
      } else if (entry.name.toString('utf8') === LEASE_FILE) {
        const { size } = await stat(path)
        files[relative] = size === 0 ? '' : `<${size} bytes>`
      } else {
        files[relative] = await readFile(path, 'utf8')
      }
    }
  }
  await walk(Buffer.from(root), '')
  return files
}

const cases = loadTable()
let root: string

beforeAll(async () => {
  root = await mkdtemp(join(tmpdir(), 'bake-plain-log-file-conformance-'))
})

afterAll(async () => {
  await rm(root, { recursive: true, force: true })
})

describe('shared plain-log file cases', () => {
  it('pin the table and name every limit and class', () => {
    expect(cases).toHaveLength(CASE_COUNT)
    expect(new Set(cases.map(entry => entry.id)).size).toBe(CASE_COUNT)
    const overrides = cases.flatMap(entry => entry.steps.flatMap(step => 'rust' in step && step.rust !== undefined ? [step.rust] : []))
    expect(new Set(overrides.filter(rust => rust !== OUTSIDE_DOMAIN))).toEqual(new Set(LIMITS))
    const thrown = cases.flatMap(entry => entry.steps.flatMap(step => step.ts.outcome === 'thrown' ? [step.ts.class] : []))
    expect(new Set(thrown)).toEqual(new Set(CLASSES.keys()))
  })

  it('refuses malformed outcomes, steps, seeds, and overrides', () => {
    expect(() => parseOutcome({ outcome: 'thrown', class: 'RangeError', message: 'x' }, 'malformed')).toThrow('invalid ts outcome')
    expect(() => parseStep({ step: 'close', ts: { outcome: 'ok' }, tree: {}, rust: { outcome: 'native-subset', limit: 'scan' } }, 'malformed'))
      .toThrow('invalid close step')
    expect(() => parseStep({ step: 'flush', ts: { outcome: 'thrown', class: 'Error' }, tree: {} }, 'malformed'))
      .toThrow('needs a rust limit')
    expect(() => parseSeed({ dir: '', rawNameHex: 'f' }, 'malformed')).toThrow('invalid seed')
    expect(() => parseRust({ outcome: 'native-subset', limit: 'other' }, 'malformed')).toThrow('invalid rust override')
    expect(() => parseStep({ step: 'flush', handle: 'c', ts: { outcome: 'ok' }, tree: {} }, 'malformed')).toThrow('invalid handle')
    expect(() => parseStep({ step: 'open', id: 'x', ts: { outcome: 'thrown', class: 'Error', message: '{src}' }, tree: {} }, 'malformed'))
      .toThrow('invalid src')
    expect(() => parseStep({ step: 'create', header: {}, ts: { outcome: 'thrown', class: 'Error', message: '{srcJson}' }, tree: {} }, 'malformed'))
      .toThrow('invalid src')
    expect(() => parseStep({ step: 'flush', src: 'x', ts: { outcome: 'ok' }, tree: {} }, 'malformed'))
      .toThrow('invalid flush step')
  })

  for (const entry of cases) {
    it.runIf(applies(entry))(entry.id, async () => {
      const caseRoot = await mkdtemp(join(root, 'case-'))
      // One backend instance per handle, so their in-process write claims
      // are separate and only the kernel lock arbitrates between them.
      const contexts = new Map(HANDLES.map(label => [label, new Context()]))
      const handles = new Map<Handle, SessionHandle>()
      try {
        await seedRoot(caseRoot, entry.seed)
        for (const ctx of contexts.values()) {
          await ctx.plugin(JsonlSessionPersistence, { root: caseRoot, compression: 'none' })
        }
        for (const [index, step] of entry.steps.entries()) {
          const context = `step ${index} ${step.handle} ${step.step}`
          const ctx = contexts.get(step.handle) as Context
          const handle = handles.get(step.handle)
          let actual: Outcome
          switch (step.step) {
            case 'create': {
              const options = step.inheritedEventCount === undefined
                ? undefined
                : { inheritedEventCount: step.inheritedEventCount as SessionLogOffset }
              actual = await outcome(async () => {
                handles.set(step.handle, await ctx.sessionPersistence.create(step.header as SessionHeader, options))
              })
              break
            }
            case 'open':
              actual = await outcome(async () => {
                handles.set(step.handle, await ctx.sessionPersistence.open(SessionId(step.id), 'write'))
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
              handles.delete(step.handle)
              break
          }
          expectOutcome(actual, step.ts, caseRoot, step.step === 'open' || step.step === 'create' ? step.src : undefined, context)
          expect(await readTree(caseRoot), context).toStrictEqual(step.tree)
        }
      } finally {
        for (const handle of handles.values()) await handle.close()
        for (const ctx of contexts.values()) await ctx.fiber.dispose()
      }
    })
  }
})
