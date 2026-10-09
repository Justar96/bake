/**
 * Shared Session lookup cases through the production JSONL backend under
 * Node. Each case builds its layout in an owned temporary directory and opens
 * the requested id with `open(id, 'read')`, the call `readColdSessionLog`
 * makes, then restores the read events as `SessionStore.prepare` does.
 *
 * `open` exposes one rejection, so the spec first awaits the backend's own
 * memoized root-wide check and then its `findLog`: a rejection there is the
 * stage that `open` reports, and `open` must reject with the same error or
 * message. A selected generation's version separates the header-only read of
 * another format from the current read's scan, identity, and restore checks,
 * which are told apart by their messages. Native-only budgets in the table
 * constrain only the Rust arm.
 */
import { createHash } from 'node:crypto'
import { readFileSync } from 'node:fs'
import { link, lstat, mkdir, mkdtemp, readFile, readdir, readlink, rm, symlink, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { dirname, join, relative, sep } from 'node:path'
import { afterAll, beforeAll, describe, expect, it } from 'vitest'
import { Context } from '@deepseek-ai/cordis'
import { Session, SessionId, interruptedTurnClosers } from 'bake-session'
import { currentSessionMessageProjections } from 'bake-session-format-catalog/message-projections'
import {
  SessionFormatUnsupportedError, SessionPersistenceCorruptionError, SessionPersistenceNotFoundError,
} from 'bake-session-persistence'
import JsonlSessionPersistence from '../src/index.ts'
import { encodeSegment, projectKey } from '../src/format.ts'

type Platform = 'posix' | 'linux' | 'win32'
type Edit = { row: number; text: string } | { row: number; find: string; replace: string }
type Entry =
  | { dir: string; rawNameHex?: string }
  | { file: string; text: string }
  | { file: string; zstdCase: string }
  | { file: string; frames: string[]; appendHex?: string }
  | { file: string; log: { input: string; header?: string; edits?: Edit[]; tail?: string; encode?: 'zstd-raw' } }
  | { symlink: string; target: string }
  | { hardlink: string; target: string }
type Expected = Record<string, unknown> & { outcome: 'restored' | 'refused' | 'failure' | 'type-error' }
interface LookupCase {
  id: string
  platforms?: Platform[]
  platformReason?: string
  compression?: 'none' | 'zstd'
  root: string
  sessionId: string
  layout: Entry[]
  maxBytes?: number
  maxSourceSeqs?: number
  maxEntries?: number
  ts: Expected
  rust?: Expected
  whenCaseInsensitive?: Expected
  nativeDiagnostic?: { text: string; path: string }
}
interface Table {
  schema: string
  version: number
  inputs: Record<string, { path: string; sha256: string }>
  defaults: { maxBytes: number; maxSourceSeqs: number; maxEntries: number }
  reviewAdditions: string[]
  versionFourAdditions: string[]
  segments: Array<[string, string]>
  projectKeys: Array<[string, string]>
  cases: LookupCase[]
}
interface Selected { sourcePath: string; sourceVersion: number }
interface StoredPrefix { tornTruncateTo?: number; recoveredTail: unknown[]; events: unknown[] }

const REPO = new URL('../../../../', import.meta.url)
const CASE_COUNT = 106
const RESTORED_KEYS = ['outcome', 'path', 'header', 'storedEventCount', 'closerCount', 'messageCount',
  'inheritedEventCount', 'endSeedAppended', 'torn']
const table = JSON.parse(readFileSync(new URL('conformance/session/lookup-cases.json', REPO), 'utf8')) as Table
const zstdTable = JSON.parse(readFileSync(new URL('conformance/session/zstd-cases.json', REPO), 'utf8')) as {
  cases: Array<{ id: string; hex: string }>
}

function input(name: string): Buffer {
  const spec = table.inputs[name]
  if (spec === undefined) throw new Error(`unknown input ${name}`)
  return readFileSync(new URL(spec.path, REPO))
}

/** One Zstandard frame of raw blocks with a 128 KiB window and no content size or checksum. */
function rawFrame(content: Buffer): Buffer {
  const parts: Buffer[] = [Buffer.from([0x28, 0xb5, 0x2f, 0xfd, 0x00, 0x38])]
  let offset = 0
  do {
    const block = content.subarray(offset, offset + 131072)
    offset += block.length
    const last = offset >= content.length ? 1 : 0
    const header = (block.length << 3) | last
    parts.push(Buffer.from([header & 0xff, (header >> 8) & 0xff, (header >> 16) & 0xff]), block)
  } while (offset < content.length)
  return Buffer.concat(parts)
}

function logBytes(spec: Extract<Entry, { log: unknown }>['log']): Buffer {
  const text = input(spec.input).toString('utf8')
  if (!text.endsWith('\n')) throw new Error(`input ${spec.input} must end with LF`)
  const lines = text.slice(0, -1).split('\n')
  const header = spec.header ?? (lines[0] as string)
  const rows = lines.slice(1)
  for (const edit of spec.edits ?? []) {
    const row = rows[edit.row]
    if (row === undefined) throw new Error(`no row ${edit.row}`)
    if ('text' in edit) {
      rows[edit.row] = edit.text
    } else {
      if (row.split(edit.find).length !== 2) throw new Error(`edit must match row ${edit.row} once`)
      rows[edit.row] = row.replace(edit.find, edit.replace)
    }
  }
  const body = rows.map(row => `${row}\n`).join('')
  if (spec.encode === 'zstd-raw') {
    if (spec.tail !== undefined) throw new Error('a raw-frame log takes no tail')
    return Buffer.concat([rawFrame(Buffer.from(`${header}\n`)), rawFrame(Buffer.from(body))])
  }
  return Buffer.from(`${header}\n${body}${spec.tail ?? ''}`)
}

function entryBytes(entry: Extract<Entry, { file: string }>): Buffer {
  if ('text' in entry) return Buffer.from(entry.text)
  if ('zstdCase' in entry) {
    const found = zstdTable.cases.find(candidate => candidate.id === entry.zstdCase)
    if (found === undefined) throw new Error(`unknown Zstd case ${entry.zstdCase}`)
    return Buffer.from(found.hex, 'hex')
  }
  if ('frames' in entry) {
    return Buffer.concat([...entry.frames.map(frame => rawFrame(Buffer.from(frame))),
      Buffer.from(entry.appendHex ?? '', 'hex')])
  }
  return logBytes(entry.log)
}

async function build(caseDir: string, layout: Entry[]): Promise<void> {
  for (const entry of layout) {
    if ('dir' in entry) {
      const dir = join(caseDir, entry.dir)
      await mkdir(dir, { recursive: true })
      if (entry.rawNameHex !== undefined) {
        await mkdir(Buffer.concat([Buffer.from(dir + sep), Buffer.from(entry.rawNameHex, 'hex')]))
      }
    } else if ('symlink' in entry) {
      const path = join(caseDir, entry.symlink)
      await mkdir(dirname(path), { recursive: true })
      await symlink(entry.target, path)
    } else if ('hardlink' in entry) {
      const path = join(caseDir, entry.hardlink)
      await mkdir(dirname(path), { recursive: true })
      await link(join(caseDir, entry.target), path)
    } else {
      const path = join(caseDir, entry.file)
      await mkdir(dirname(path), { recursive: true })
      await writeFile(path, entryBytes(entry))
    }
  }
}

/** Every entry under `dir` by raw name, with its type and bytes or link target, without following links. */
async function snapshot(dir: Buffer, prefix = ''): Promise<string[]> {
  const out: string[] = []
  for (const name of (await readdir(dir, { encoding: 'buffer' })).sort(Buffer.compare)) {
    const path = Buffer.concat([dir, Buffer.from(sep), name])
    const label = `${prefix}${name.toString('hex')}`
    const info = await lstat(path)
    if (info.isSymbolicLink()) {
      out.push(`${label} link ${(await readlink(path, { encoding: 'buffer' })).toString('hex')}`)
    } else if (info.isDirectory()) {
      out.push(`${label} dir`, ...await snapshot(path, `${label}/`))
    } else {
      const digest = createHash('sha256').update(await readFile(path)).digest('hex')
      out.push(`${label} file ${digest} ${info.mtimeMs}`)
    }
  }
  return out
}

function runsHere(entry: LookupCase): boolean {
  if (entry.platforms === undefined) return true
  return entry.platforms.some(platform => platform === 'linux'
    ? process.platform === 'linux'
    : platform === 'win32' ? process.platform === 'win32' : process.platform !== 'win32')
}

function messages(error: unknown): string[] {
  if (!(error instanceof Error)) return [String(error)]
  return [error.message, ...(error.cause === undefined ? [] : messages(error.cause))]
}

function kindOf(error: unknown): string {
  if (error instanceof SessionFormatUnsupportedError) return 'unsupported'
  if (error instanceof SessionPersistenceNotFoundError) return 'not-found'
  return 'invalid'
}

const REASONS: Array<[RegExp, string]> = [
  [/uses the unsupported flat-file layout/, 'legacy-layout'],
  [/but this backend is configured for compression/, 'encoding-mismatch'],
  [/^duplicate JSONL session id/, 'duplicate-id'],
  [/^session generation filename identifies v\d+, but its header identifies v\d+$/, 'generation-header-mismatch'],
  [/stored log has a malformed header/, 'malformed-header'],
  [/^session header uses retired policy baseline fields$/, 'retired-header-fields'],
  [/uses log format v\d+, but this harness reads only/, 'newer-format'],
  [/requested id ".*" does not match header id/, 'id-mismatch'],
  [/header id ".*" and cwd identify/, 'path-mismatch'],
]

function reasonOf(error: unknown): string | null {
  if (error instanceof SessionPersistenceNotFoundError) return 'not-found'
  const message = error instanceof Error ? error.message : String(error)
  return REASONS.find(([pattern]) => pattern.test(message))?.[1] ?? null
}

function isErrno(error: unknown): error is NodeJS.ErrnoException {
  return error instanceof Error && typeof (error as NodeJS.ErrnoException).code === 'string'
}

/** The root-relative, `/`-separated spelling the shared table uses. */
function relativePath(root: string, path: string): string {
  return relative(root, path).split(sep).join('/')
}

function artifactPath(root: string, error: unknown): string | null {
  const match = /^session artifact ("(?:[^"\\]|\\.)*") uses/.exec(error instanceof Error ? error.message : '')
  return match === null ? null : relativePath(root, JSON.parse(match[1] as string) as string)
}

/**
 * A rejection in the table's terms. Reason codes name lookup and header
 * outcomes only: a scan or restore refusal is identified by its kind and
 * message, even where its text resembles a header refusal's.
 */
function refusal(stage: string, error: unknown, path: string | null): Record<string, unknown> {
  if (isErrno(error)) return { outcome: 'failure', stage, code: error.code }
  if (error instanceof TypeError) return { outcome: 'type-error', stage }
  const reason = stage === 'scan' || stage === 'restore' ? null : reasonOf(error)
  return { outcome: 'refused', stage, reason, kind: kindOf(error), path }
}

/** The stage of a current-format read's rejection, told apart by its message. */
function currentStage(error: unknown): string {
  const reason = reasonOf(error)
  if (reason === 'id-mismatch' || reason === 'path-mismatch') return 'identity'
  const message = error instanceof Error ? error.message : ''
  if (/^stored session ".*" failed validation: |contains event type|contains a request\/header event/.test(message)) {
    return 'restore'
  }
  if (error instanceof SessionPersistenceCorruptionError || error instanceof SessionFormatUnsupportedError) return 'scan'
  // `sameFile` is the only realpath caller; any other errno comes from the stat or read.
  return isErrno(error) && error.syscall === 'realpath' ? 'identity' : 'read'
}

async function rejection(promise: Promise<unknown>): Promise<unknown> {
  return promise.then(() => {
    throw new Error('expected a rejection')
  }, (error: unknown) => error)
}

/** Observe one case through the production backend; the Context is disposed before returning. */
async function observe(entry: LookupCase, caseDir: string): Promise<{ observed: Record<string, unknown>; error?: unknown }> {
  // Concatenated, not joined, so the backend's own `resolve` normalizes the spelling.
  const config = { root: `${caseDir}${sep}${entry.root}`, ...entry.compression === undefined ? {} : { compression: entry.compression } }
  const root = join(caseDir, entry.root)
  const id = SessionId(entry.sessionId)
  const ctx = new Context()
  try {
    try {
      await ctx.plugin(JsonlSessionPersistence, config)
    } catch (error) {
      return { observed: refusal('root', error, null), error }
    }
    const backend = ctx.sessionPersistence as unknown as JsonlSessionPersistence
    const ensure = Reflect.get(backend, 'ensureRootEncoding') as () => Promise<void>
    const findLog = Reflect.get(backend, 'findLog') as (id: SessionId) => Promise<Selected | undefined>
    const requireStoredLog = Reflect.get(backend, 'requireStoredLog') as (id: SessionId) => Promise<StoredPrefix>
    try {
      await ensure.call(backend)
    } catch (error) {
      expect(await rejection(backend.open(id, 'read'))).toBe(error)
      return { observed: refusal('layout', error, artifactPath(root, error)), error }
    }
    let selected: Selected | undefined
    try {
      selected = await findLog.call(backend, id)
    } catch (error) {
      expect(messages(await rejection(backend.open(id, 'read')))).toEqual(messages(error))
      return { observed: refusal('lookup', error, artifactPath(root, error)), error }
    }
    if (selected === undefined) {
      const error = await rejection(backend.open(id, 'read'))
      expect(error).toBeInstanceOf(SessionPersistenceNotFoundError)
      return { observed: refusal('lookup', error, null), error }
    }
    const path = relativePath(root, selected.sourcePath)
    let handle: Awaited<ReturnType<typeof backend.open>>
    try {
      handle = await backend.open(id, 'read')
    } catch (error) {
      const stage = selected.sourceVersion === 3 ? currentStage(error) : 'generation'
      return { observed: refusal(stage, error, path), error }
    }
    const read = await handle.read(0)
    await handle.close()
    const closers = interruptedTurnClosers(read.events)
    const session = Session.fromRestore(id, [...read.events, ...closers], handle.header, handle.inheritedEventCount,
      read.eventState, currentSessionMessageProjections)
    let torn: unknown = null
    if (selected.sourceVersion === 3) {
      // The memoized stored view carries the physical recovery offset that the read handle does not expose.
      const stored = await requireStoredLog.call(backend, id)
      torn = stored.tornTruncateTo === undefined ? null : {
        truncateTo: stored.tornTruncateTo, recoveredFrom: stored.events.length - stored.recoveredTail.length,
      }
    }
    return {
      observed: {
        outcome: 'restored', path,
        header: { id: handle.header.id, cwd: handle.header.cwd ?? null },
        storedEventCount: read.events.length, closerCount: closers.length,
        messageCount: session.deriveMessages().length, inheritedEventCount: handle.inheritedEventCount,
        endSeedAppended: session.seq > read.events.length + closers.length, torn,
      },
    }
  } finally {
    await ctx.fiber.dispose()
  }
}

let scratch: string | undefined
let caseInsensitive: boolean

beforeAll(async () => {
  scratch = await mkdtemp(join(tmpdir(), 'bake-lookup-conformance-'))
  await writeFile(join(scratch, 'probe-case'), '')
  caseInsensitive = await lstat(join(scratch, 'PROBE-CASE')).then(() => true, () => false)
})

afterAll(async () => {
  if (scratch !== undefined) await rm(scratch, { recursive: true, force: true })
})

describe('shared Session lookup cases', () => {
  it('pins the inputs, the table, and its expectations', () => {
    expect(table.schema).toBe('bake/session-conformance/lookup-cases')
    expect(table.version).toBe(6)
    expect(table.defaults).toEqual({ maxBytes: 1048576, maxSourceSeqs: 64, maxEntries: 1024 })
    for (const additions of [table.reviewAdditions, table.versionFourAdditions]) {
      expect(additions.every(id => table.cases.some(entry => entry.id === id))).toBe(true)
    }
    for (const spec of Object.values(table.inputs)) {
      expect(createHash('sha256').update(readFileSync(new URL(spec.path, REPO))).digest('hex'), spec.path).toBe(spec.sha256)
    }
    expect(table.cases).toHaveLength(CASE_COUNT)
    expect(new Set(table.cases.map(entry => entry.id)).size).toBe(CASE_COUNT)
    for (const entry of table.cases) {
      expect(entry.platforms === undefined, entry.id).toBe(entry.platformReason === undefined)
      // A native failure names its diagnostic; a TypeScript failure names its error code or a native budget.
      const native = entry.whenCaseInsensitive ?? entry.rust ?? entry.ts
      expect(native.outcome === 'failure', entry.id).toBe(entry.nativeDiagnostic !== undefined)
      for (const expected of [entry.ts, entry.rust]) {
        if (expected?.outcome === 'failure') expect(typeof (expected.code ?? expected.budget), entry.id).toBe('string')
      }
      for (const expected of [entry.ts, entry.rust, entry.whenCaseInsensitive]) {
        // A migrated generation's restored counts are claimed only where a source test pins them.
        if (expected?.outcome !== 'restored' || !/\/session\.v3\.jsonl(\.zstd)?$/.test(String(expected.path))) continue
        expect(Object.keys(expected), entry.id).toEqual(RESTORED_KEYS)
      }
    }
  })

  it('encodes Session ids and project keys as the format helpers do', () => {
    for (const [raw, encoded] of table.segments) expect(encodeSegment(raw), raw).toBe(encoded)
    for (const [cwd, key] of table.projectKeys) expect(projectKey(cwd), cwd).toBe(key)
  })

  for (const entry of table.cases) {
    it.skipIf(!runsHere(entry))(entry.id, async () => {
      const caseDir = await mkdtemp(join(scratch as string, 'case-'))
      try {
        await build(caseDir, entry.layout)
        const before = await snapshot(Buffer.from(caseDir))
        const { observed, error } = await observe(entry, caseDir)
        const expected = caseInsensitive && entry.whenCaseInsensitive !== undefined ? entry.whenCaseInsensitive : entry.ts
        const { message, ...shape } = expected
        const compared = expected.outcome === 'restored'
          ? Object.fromEntries(Object.keys(shape).map(key => [key, observed[key]]))
          : observed
        expect(compared).toStrictEqual(shape)
        if (message !== undefined) expect(messages(error).some(text => text.includes(String(message)))).toBe(true)
        expect(await snapshot(Buffer.from(caseDir))).toEqual(before)
      } finally {
        await rm(caseDir, { recursive: true, force: true })
      }
    })
  }
})
