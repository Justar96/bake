/**
 * Runs the shared cases in `conformance/session/migration-publication-cases.json`
 * that apply to this host through the real JSONL backend with
 * `compression: 'none'`, each in its own temporary root. A case seeds files,
 * then write-opens a v2 Session, whose migration publication races another
 * writer or fails: when the backend creates its `session.migration.*`
 * temporary file, the case's race actions run first, writing, replacing
 * through a rename, or removing a file, or creating a symbolic link, and its
 * fault, an errno, is injected at that creation, at the temporary file's
 * first write, or at the hard link that publishes it. The remaining steps
 * append to and close the handle. After each step every file beneath the
 * root must have the expected text, a `session.lock` file read only by its
 * size, and a symbolic link by its target, and no other file may exist.
 *
 * The race and the fault reach the backend through a pass-through mock of
 * `node:fs/promises`, which only acts on the temporary file's creation and
 * link; the verifier worker reads the real files. `ts` is the step's
 * outcome: success, a thrown class with its exact message, in which `{src}`
 * stands for the root joined with the step's `src` path and `{dst}` for the
 * root joined with its `dst` path, or an errno error by its code. A `rust`
 * override names the native limit the development Rust model
 * `PlainLogFile` refuses the step by; it ends the case in Rust, and
 * TypeScript still asserts every step. The Rust arm is
 * `rust/crates/bake-session/tests/migration_publication_cases.rs`. The spec
 * reads only the table.
 */

import { readFileSync } from 'node:fs'
import { basename, dirname, join, resolve } from 'node:path'
import { afterAll, beforeAll, describe, expect, it, vi } from 'vitest'
import { Context } from '@deepseek-ai/cordis'
import { SessionId } from 'bake-session'
import type { SessionEvent } from 'bake-session'
import { SessionPersistenceCorruptionError } from 'bake-session-persistence'
import type { SessionHandle } from 'bake-session-persistence'
import JsonlSessionPersistence from '../src/index.ts'
import { JsonlGenerationSourceChangedError } from '../src/generation.ts'

/** The race and fault of the case running now, which the mocked file system applies. */
const hooks = vi.hoisted(() => ({
  active: undefined as undefined | {
    readonly race: () => Promise<void>
    readonly fault?: { readonly op: string; readonly code: string }
    fired: boolean
  },
}))

vi.mock('node:fs/promises', async (importOriginal) => {
  const actual = await importOriginal<typeof import('node:fs/promises')>()
  const { basename: base } = await import('node:path')
  const errno = (code: string, syscall: string): NodeJS.ErrnoException =>
    Object.assign(new Error(`${code}: injected fault, ${syscall}`), { code, syscall })
  const isTemporary = (path: unknown): boolean => base(String(path)).startsWith('session.migration.')
  const open: typeof actual.open = async (path, flags, mode) => {
    const hook = hooks.active
    if (hook === undefined || flags !== 'wx' || !isTemporary(path) || hook.fired) return actual.open(path, flags, mode)
    hook.fired = true
    await hook.race()
    if (hook.fault?.op === 'create-temporary') throw errno(hook.fault.code, 'open')
    const handle = await actual.open(path, flags, mode)
    if (hook.fault?.op === 'write-temporary') {
      const code = hook.fault.code
      Object.defineProperty(handle, 'writeFile', { value: async () => { throw errno(code, 'write') } })
    }
    return handle
  }
  const link: typeof actual.link = async (existing, created) => {
    const hook = hooks.active
    if (hook?.fault?.op === 'link' && isTemporary(existing)) throw errno(hook.fault.code, 'link')
    return actual.link(existing, created)
  }
  return { ...actual, default: { ...actual, open, link }, open, link }
})

// The spec's own file operations; the mock passes them through.
const { mkdir, mkdtemp, readdir, readFile, readlink, rename, rm, stat, symlink, writeFile } = await import('node:fs/promises')

const REPO = new URL('../../../../', import.meta.url)
const SCHEMA = 'bake/session-conformance/migration-publication-cases'
const ORACLE = 'in an owned temporary root holding the seeded entries, write-open the Session through the JSONL backend with compression none; when the backend creates its session.migration temporary file, first apply the case\'s race actions, writing, replacing through a rename, or removing a file, or creating a symbolic link, beneath the root, and inject the case\'s fault, an errno at that creation, at the temporary file\'s first write, or at the hard link; then run the remaining steps on the handle, and after each step list every file beneath the root with its text, an empty session.lock by its size, and every symbolic link by its target'
/** Both harnesses pin the table size, so a dropped case fails. */
const CASE_COUNT = 15
const VERSION = 1
const LIMITS = ['migration/target-tail']
const FAULT_OPS = ['create-temporary', 'write-temporary', 'link']
const LEASE_FILE = 'session.lock'
/** Classes are matched exactly. */
const CLASSES = new Map<string, abstract new (...args: never[]) => Error>([
  ['SessionPersistenceCorruptionError', SessionPersistenceCorruptionError],
  ['JsonlGenerationSourceChangedError', JsonlGenerationSourceChangedError],
])
const PLATFORMS: readonly string[] = ['posix', 'linux', 'darwin', 'win32']

type Outcome = { outcome: 'ok' } | { outcome: 'thrown'; class: string; message: string } | { outcome: 'errno'; code: string }
type Seed = { file: string; text: string }
type Race = { write: string; text: string } | { replace: string; text: string } | { remove: string } | { link: string; target: string }
type Tree = Record<string, string>
type Step =
  | { step: 'open'; id: string; src?: string; dst?: string; ts: Outcome; rust?: string; tree: Tree }
  | { step: 'append'; events: unknown[]; ts: Outcome; tree: Tree }
  | { step: 'close'; ts: Outcome; tree: Tree }

interface PublicationCase {
  id: string
  platforms?: string[]
  seed: Seed[]
  race: Race[]
  fault?: { op: string; code: string }
  steps: Step[]
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
    if (keys === 'class,message,outcome' && value.outcome === 'thrown' && typeof value.class === 'string'
      && CLASSES.has(value.class) && typeof value.message === 'string') {
      return { outcome: 'thrown', class: value.class, message: value.message }
    }
    if (keys === 'code,outcome' && value.outcome === 'errno' && typeof value.code === 'string') {
      return { outcome: 'errno', code: value.code }
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

function parseRace(value: unknown, id: string): Race {
  if (isObject(value)) {
    const keys = sortedKeys(value)
    if ((keys === 'text,write' && typeof value.write === 'string' && typeof value.text === 'string')
      || (keys === 'replace,text' && typeof value.replace === 'string' && typeof value.text === 'string')
      || (keys === 'remove' && typeof value.remove === 'string')
      || (keys === 'link,target' && typeof value.link === 'string' && typeof value.target === 'string')) {
      return value as Race
    }
  }
  throw new Error(`${id}: invalid race action ${JSON.stringify(value)}`)
}

function parseStep(value: unknown, id: string, first: boolean): Step {
  if (!isObject(value) || typeof value.step !== 'string') throw new Error(`${id}: invalid step`)
  const ts = parseOutcome(value.ts, id)
  const tree = parseTree(value.tree, id)
  if (first !== (value.step === 'open')) throw new Error(`${id}: the open step comes first, and only there`)
  switch (value.step) {
    case 'open': {
      if (!Object.keys(value).every(key => ['step', 'id', 'src', 'dst', 'ts', 'rust', 'tree'].includes(key))
        || typeof value.id !== 'string') throw new Error(`${id}: invalid open step`)
      const message = ts.outcome === 'thrown' ? ts.message : ''
      for (const [key, placeholder] of [['src', '{src}'], ['dst', '{dst}']] as const) {
        const path = value[key]
        if ((path !== undefined && typeof path !== 'string') || message.includes(placeholder) !== (path !== undefined)) {
          throw new Error(`${id}: invalid ${key}`)
        }
      }
      let rust: string | undefined
      if (value.rust !== undefined) {
        const override = value.rust
        if (!isObject(override) || sortedKeys(override) !== 'limit,outcome' || override.outcome !== 'native-subset'
          || !LIMITS.includes(override.limit as string)) throw new Error(`${id}: invalid rust override`)
        rust = override.limit as string
      }
      return {
        step: 'open', id: value.id, ts, tree,
        ...(value.src === undefined ? {} : { src: value.src as string }),
        ...(value.dst === undefined ? {} : { dst: value.dst as string }),
        ...(rust === undefined ? {} : { rust }),
      }
    }
    case 'append':
      if (sortedKeys(value) !== 'events,step,tree,ts' || !Array.isArray(value.events)) throw new Error(`${id}: invalid append step`)
      return { step: 'append', events: value.events, ts, tree }
    case 'close':
      if (sortedKeys(value) !== 'step,tree,ts' || ts.outcome !== 'ok') throw new Error(`${id}: invalid close step`)
      return { step: 'close', ts, tree }
    default:
      throw new Error(`${id}: unknown step ${String(value.step)}`)
  }
}

function loadTable(): PublicationCase[] {
  const table: unknown = JSON.parse(readFileSync(new URL('conformance/session/migration-publication-cases.json', REPO), 'utf8'))
  if (!isObject(table) || sortedKeys(table) !== 'cases,history,oracle,schema,version' || table.schema !== SCHEMA
    || table.version !== VERSION || table.oracle !== ORACLE || !Array.isArray(table.cases)
    || !Array.isArray(table.history) || !table.history.every(line => typeof line === 'string')) {
    throw new Error(`migration-publication-cases.json does not match its version-${VERSION} schema`)
  }
  return table.cases.map((entry: unknown): PublicationCase => {
    if (!isObject(entry) || typeof entry.id !== 'string' || !Array.isArray(entry.seed) || !Array.isArray(entry.race)
      || !Array.isArray(entry.steps) || entry.steps.length === 0) {
      throw new Error(`invalid case ${JSON.stringify(entry)}`)
    }
    const { id } = entry
    if (!Object.keys(entry).every(key => ['id', 'platforms', 'platformReason', 'seed', 'race', 'fault', 'steps', 'note'].includes(key))) {
      throw new Error(`${id}: unknown keys`)
    }
    if ((entry.platforms === undefined) !== (entry.platformReason === undefined)
      || (entry.platforms !== undefined && (!Array.isArray(entry.platforms)
        || !entry.platforms.every(platform => PLATFORMS.includes(platform as string))))) {
      throw new Error(`${id}: invalid platforms`)
    }
    const fault = entry.fault
    if (fault !== undefined && (!isObject(fault) || sortedKeys(fault) !== 'code,op'
      || !FAULT_OPS.includes(fault.op as string) || typeof fault.code !== 'string')) {
      throw new Error(`${id}: invalid fault`)
    }
    const seed = entry.seed.map((value: unknown): Seed => {
      if (!isObject(value) || sortedKeys(value) !== 'file,text' || typeof value.file !== 'string' || typeof value.text !== 'string') {
        throw new Error(`${id}: invalid seed`)
      }
      return { file: value.file, text: value.text }
    })
    return {
      id,
      ...(entry.platforms === undefined ? {} : { platforms: entry.platforms as string[] }),
      seed,
      race: entry.race.map(value => parseRace(value, id)),
      ...(fault === undefined ? {} : { fault: fault as { op: string; code: string } }),
      steps: entry.steps.map((step, index) => parseStep(step, id, index === 0)),
    }
  })
}

function applies(entry: PublicationCase): boolean {
  if (entry.platforms === undefined) return true
  return entry.platforms.some(platform => platform === 'posix'
    ? process.platform !== 'win32'
    : process.platform === platform)
}

/** `relative`, `/`-separated, beneath `root`. */
const beneath = (root: string, relative: string): string => resolve(root, ...relative.split('/'))

async function applyRace(root: string, actions: readonly Race[]): Promise<void> {
  for (const action of actions) {
    if ('write' in action) {
      const path = beneath(root, action.write)
      await mkdir(dirname(path), { recursive: true })
      await writeFile(path, action.text)
    } else if ('replace' in action) {
      const path = beneath(root, action.replace)
      const sibling = join(dirname(path), `.replacement-${basename(path)}`)
      await writeFile(sibling, action.text)
      await rename(sibling, path)
    } else if ('remove' in action) {
      await rm(beneath(root, action.remove))
    } else {
      await symlink(action.target, beneath(root, action.link))
    }
  }
}

/** Every file beneath `root`, as `plain-log-file-conformance.spec.ts` lists it. */
async function readTree(root: string): Promise<Tree> {
  const files: Tree = {}
  async function walk(dir: string, prefix: string): Promise<void> {
    for (const entry of await readdir(dir, { withFileTypes: true })) {
      const path = join(dir, entry.name)
      const relative = prefix + entry.name
      if (entry.isSymbolicLink()) {
        files[relative] = `<link to ${await readlink(path, 'utf8')}>`
      } else if (entry.isDirectory()) {
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

/** Run one step; an error of an unlisted class without an errno code fails the case. */
async function outcome(run: () => Promise<void>): Promise<Outcome> {
  try {
    await run()
    return { outcome: 'ok' }
  } catch (error) {
    const name = [...CLASSES].find(([, value]) => value === (error as object).constructor)?.[0]
    if (name !== undefined) return { outcome: 'thrown', class: name, message: (error as Error).message }
    const code = (error as NodeJS.ErrnoException).code
    if (typeof code === 'string') return { outcome: 'errno', code }
    throw error
  }
}

const cases = loadTable()
let root: string

beforeAll(async () => {
  root = await mkdtemp(join((await import('node:os')).tmpdir(), 'bake-migration-publication-conformance-'))
})

afterAll(async () => {
  hooks.active = undefined
  await rm(root, { recursive: true, force: true })
})

describe('shared migration publication cases', () => {
  it('pin the table and name every limit, class, and fault', () => {
    expect(cases).toHaveLength(CASE_COUNT)
    expect(new Set(cases.map(entry => entry.id)).size).toBe(CASE_COUNT)
    const steps = cases.flatMap(entry => entry.steps)
    expect(new Set(steps.flatMap(step => step.step === 'open' && step.rust !== undefined ? [step.rust] : []))).toEqual(new Set(LIMITS))
    expect(new Set(steps.flatMap(step => step.ts.outcome === 'thrown' ? [step.ts.class] : []))).toEqual(new Set(CLASSES.keys()))
    expect(new Set(cases.flatMap(entry => entry.fault === undefined ? [] : [entry.fault.op]))).toEqual(new Set(FAULT_OPS))
  })

  it('refuses malformed outcomes, races, and steps', () => {
    expect(() => parseOutcome({ outcome: 'errno' }, 'malformed')).toThrow('invalid ts outcome')
    expect(() => parseOutcome({ outcome: 'thrown', class: 'Error', message: 'x' }, 'malformed')).toThrow('invalid ts outcome')
    expect(() => parseRace({ write: 'x' }, 'malformed')).toThrow('invalid race action')
    expect(() => parseStep({ step: 'append', events: [], ts: { outcome: 'ok' }, tree: {} }, 'malformed', true)).toThrow('comes first')
    expect(() => parseStep({ step: 'open', id: 'x', ts: { outcome: 'thrown', class: 'JsonlGenerationSourceChangedError', message: '{src}' }, tree: {} }, 'malformed', true))
      .toThrow('invalid src')
    expect(() => parseStep({ step: 'open', id: 'x', rust: { outcome: 'native-subset', limit: 'scan' }, ts: { outcome: 'ok' }, tree: {} }, 'malformed', true))
      .toThrow('invalid rust override')
  })

  for (const entry of cases) {
    it.runIf(applies(entry))(entry.id, async () => {
      const caseRoot = await mkdtemp(join(root, 'case-'))
      const ctx = new Context()
      let handle: SessionHandle | undefined
      try {
        for (const seed of entry.seed) {
          const path = beneath(caseRoot, seed.file)
          await mkdir(dirname(path), { recursive: true })
          await writeFile(path, seed.text)
        }
        await ctx.plugin(JsonlSessionPersistence, { root: caseRoot, compression: 'none' })
        const hook = {
          race: () => applyRace(caseRoot, entry.race),
          ...(entry.fault === undefined ? {} : { fault: entry.fault }),
          fired: false,
        }
        hooks.active = hook
        for (const [index, step] of entry.steps.entries()) {
          const context = `step ${index} ${step.step}`
          let actual: Outcome
          if (step.step === 'open') {
            actual = await outcome(async () => {
              handle = await ctx.sessionPersistence.open(SessionId(step.id), 'write')
            })
            expect(hook.fired, `${context}: the migration created its temporary file`).toBe(true)
            hooks.active = undefined
            let expected = step.ts
            if (expected.outcome === 'thrown') {
              let message = expected.message
              if (step.src !== undefined) message = message.replaceAll('{src}', beneath(caseRoot, step.src))
              if (step.dst !== undefined) message = message.replaceAll('{dst}', beneath(caseRoot, step.dst))
              expected = { ...expected, message }
            }
            expect(actual, context).toStrictEqual(expected)
          } else if (step.step === 'append') {
            actual = await outcome(() => (handle as SessionHandle).append(step.events as SessionEvent[]))
            expect(actual, context).toStrictEqual(step.ts)
          } else {
            actual = await outcome(() => (handle as SessionHandle).close())
            handle = undefined
            expect(actual, context).toStrictEqual(step.ts)
          }
          expect(await readTree(caseRoot), context).toStrictEqual(step.tree)
        }
      } finally {
        hooks.active = undefined
        await handle?.close()
        await ctx.fiber.dispose()
      }
    })
  }
})
