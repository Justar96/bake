/**
 * Shared Session list cases through the production JSONL backend under Node.
 * Each case builds its layout in an owned temporary directory and calls the
 * public `list()` on a fresh backend. `list` reports no artifact path, so a
 * listed record pairs each snapshot with the backend's own `listArtifacts`
 * and `resolveGenerationInDirectory` for its path and filename version. A
 * rejection is staged by probing the same backend: its memoized root-wide
 * check for the layout, the duplicate message for discovery, and otherwise
 * each Session directory's own header read for the artifact that rejects with
 * the same message. Native-only budgets in the table constrain only the Rust
 * arm.
 */
import { createHash } from 'node:crypto'
import { readFileSync } from 'node:fs'
import { lstat, mkdir, mkdtemp, readFile, readdir, readlink, rm, stat, symlink, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { dirname, join, relative, sep } from 'node:path'
import { afterAll, beforeAll, describe, expect, it } from 'vitest'
import { Context } from '@deepseek-ai/cordis'
import type { SessionHeader } from 'bake-session'
import { SessionFormatUnsupportedError, SessionPersistenceCorruptionError } from 'bake-session-persistence'
import JsonlSessionPersistence from '../src/index.ts'

type Entry =
  | { dir: string }
  | { file: string; text: string }
  | { file: string; frames: string[]; appendHex?: string }
  | { symlink: string; target: string }
type Expected = Record<string, unknown> & { outcome: 'listed' | 'refused' | 'failure' }
interface ListCase {
  id: string
  platforms?: Array<'posix'>
  platformReason?: string
  compression?: 'none' | 'zstd'
  root: string
  maxHeaderBytes?: number
  maxEntries?: number
  nativeDiagnostic?: { text: string; path: string }
  layout: Entry[]
  ts: Expected
  rust?: Expected
}
interface Table {
  schema: string
  version: number
  provenance: string
  notes: string
  defaults: { maxHeaderBytes: number; maxEntries: number }
  cases: ListCase[]
}
interface Selected { sourcePath: string; sourceVersion: number }
interface Artifact { header: SessionHeader; path: string }

const REPO = new URL('../../../../', import.meta.url)
const CASE_COUNT = 58
const HEADER_KEYS = ['version', 'id', 'createdAt', 'cwd', 'parentSession', 'isSeeded', 'origin', 'delegationDepth',
  'agentPreset'] as const
const CASE_KEYS = ['id', 'platforms', 'platformReason', 'compression', 'root', 'maxHeaderBytes', 'maxEntries',
  'nativeDiagnostic', 'layout', 'ts', 'rust']
const STAGES = ['root', 'layout', 'discovery', 'header', 'identity']
const table = JSON.parse(readFileSync(new URL('conformance/session/list-cases.json', REPO), 'utf8')) as Table

/** One Zstandard frame of raw blocks with a 128 KiB window and no content size or checksum. */
function rawFrame(content: Buffer): Buffer {
  const parts: Buffer[] = [Buffer.from([0x28, 0xb5, 0x2f, 0xfd, 0x00, 0x38])]
  let offset = 0
  do {
    const block = content.subarray(offset, offset + 131072)
    offset += block.length
    const header = (block.length << 3) | (offset >= content.length ? 1 : 0)
    parts.push(Buffer.from([header & 0xff, (header >> 8) & 0xff, (header >> 16) & 0xff]), block)
  } while (offset < content.length)
  return Buffer.concat(parts)
}

async function build(caseDir: string, layout: Entry[]): Promise<void> {
  for (const entry of layout) {
    if ('dir' in entry) {
      await mkdir(join(caseDir, entry.dir), { recursive: true })
    } else if ('symlink' in entry) {
      const path = join(caseDir, entry.symlink)
      await mkdir(dirname(path), { recursive: true })
      await symlink(entry.target, path)
    } else {
      const path = join(caseDir, entry.file)
      await mkdir(dirname(path), { recursive: true })
      await writeFile(path, 'text' in entry
        ? Buffer.from(entry.text)
        : Buffer.concat([...entry.frames.map(frame => rawFrame(Buffer.from(frame))),
          Buffer.from(entry.appendHex ?? '', 'hex')]))
    }
  }
}

/** Every entry under `dir` with its type and digest or link target, without following links. */
async function snapshot(dir: string, prefix = ''): Promise<string[]> {
  const out: string[] = []
  for (const name of (await readdir(dir)).sort()) {
    const path = join(dir, name)
    const info = await lstat(path)
    if (info.isSymbolicLink()) {
      out.push(`${prefix}${name} link ${await readlink(path)}`)
    } else if (info.isDirectory()) {
      out.push(`${prefix}${name} dir ${info.mtimeMs}`, ...await snapshot(path, `${prefix}${name}/`))
    } else {
      const digest = createHash('sha256').update(await readFile(path)).digest('hex')
      out.push(`${prefix}${name} file ${digest} ${info.mtimeMs}`)
    }
  }
  return out
}

const REASONS: Array<[RegExp, string]> = [
  [/uses the unsupported flat-file layout/, 'legacy-layout'],
  [/but this backend is configured for compression/, 'encoding-mismatch'],
  [/^duplicate JSONL session id/, 'duplicate-id'],
  [/^session generation filename identifies v\d+, but its header identifies v\d+$/, 'generation-header-mismatch'],
  [/^session header uses retired policy baseline fields$/, 'retired-header-fields'],
  [/header id ".*" and cwd identify/s, 'path-mismatch'],
  [/^corrupt session log ".*": header id cannot name a storage path$/s, 'unencodable-id'],
]
const IDENTITY_REASONS = new Set(['id-mismatch', 'path-mismatch', 'unencodable-id'])

function reasonOf(error: unknown): string | null {
  const message = error instanceof Error ? error.message : String(error)
  return REASONS.find(([pattern]) => pattern.test(message))?.[1] ?? null
}

function isErrno(error: unknown): error is NodeJS.ErrnoException {
  return error instanceof Error && typeof (error as NodeJS.ErrnoException).code === 'string'
}

/** Discovery skips only these classes; every other rejection aborts `list`. */
function isIsolated(error: unknown): boolean {
  return error instanceof SessionFormatUnsupportedError || error instanceof SessionPersistenceCorruptionError
}

/** The root-relative, `/`-separated spelling the shared table uses. */
function relativePath(root: string, path: string): string {
  return relative(root, path).split(sep).join('/')
}

function artifactPath(root: string, error: unknown): string | null {
  const match = /^session artifact ("(?:[^"\\]|\\.)*") uses/.exec(error instanceof Error ? error.message : '')
  return match === null ? null : relativePath(root, JSON.parse(match[1] as string) as string)
}

function refusal(stage: string, error: unknown, path: string | null): Record<string, unknown> {
  return { outcome: 'refused', stage, reason: reasonOf(error), kind: 'invalid', path }
}

/** Private backend members, called read-only and only to name what `list` already decided. */
function internals(backend: JsonlSessionPersistence) {
  const bind = <T>(name: string): T => (Reflect.get(backend, name) as (...args: never[]) => unknown).bind(backend) as T
  return {
    ensureRootEncoding: bind<() => Promise<void>>('ensureRootEncoding'),
    listArtifacts: bind<() => Promise<Artifact[]>>('listArtifacts'),
    listProjectDirs: bind<() => Promise<string[]>>('listProjectDirs'),
    listSessionDirs: bind<(project: string) => Promise<string[]>>('listSessionDirs'),
    resolveGeneration: bind<(dir: string) => Promise<Selected | undefined>>('resolveGenerationInDirectory'),
    readGenerationHeader: bind<(selected: Selected) => Promise<SessionHeader | undefined>>('readGenerationHeader'),
  }
}

/**
 * Name the stage and artifact of an aborting `list` rejection. Each Session
 * directory's selected generation is read alone, and exactly one must reject
 * with the same message outside the isolated classes.
 */
async function stageOf(backend: JsonlSessionPersistence, root: string, error: Error): Promise<Record<string, unknown>> {
  const probe = internals(backend)
  // The root-wide check is memoized, so a layout refusal is the same object.
  const layout = await probe.ensureRootEncoding().then(() => undefined, (cause: unknown) => cause)
  if (layout !== undefined) {
    expect(layout).toBe(error)
    return refusal('layout', error, artifactPath(root, error))
  }
  const reason = reasonOf(error)
  if (reason === 'duplicate-id') return refusal('discovery', error, null)
  const culprits: string[] = []
  for (const project of await probe.listProjectDirs()) {
    for (const dir of await probe.listSessionDirs(project)) {
      const selected = await probe.resolveGeneration(dir)
      if (selected === undefined) continue
      const cause = await probe.readGenerationHeader(selected).then(() => undefined, (failure: unknown) => failure)
      if (cause instanceof Error && !isIsolated(cause) && cause.message === error.message) {
        culprits.push(selected.sourcePath)
      }
    }
  }
  expect(culprits).toHaveLength(1)
  const stage = reason !== null && IDENTITY_REASONS.has(reason) ? 'identity' : 'header'
  return refusal(stage, error, relativePath(root, culprits[0] as string))
}

/** Observe one case through the production backend; the Context is disposed before returning. */
async function observe(entry: ListCase, caseDir: string): Promise<{ observed: Record<string, unknown>; error?: unknown }> {
  const config = { root: join(caseDir, entry.root), ...entry.compression === undefined ? {} : { compression: entry.compression } }
  const root = config.root
  const ctx = new Context()
  try {
    await ctx.plugin(JsonlSessionPersistence, config)
    const backend = ctx.sessionPersistence as unknown as JsonlSessionPersistence
    let snapshots: Awaited<ReturnType<typeof backend.list>>
    try {
      snapshots = await backend.list()
    } catch (error) {
      if (isErrno(error)) return { observed: { outcome: 'failure', code: error.code }, error }
      expect(error).toBeInstanceOf(Error)
      expect(isIsolated(error)).toBe(false)
      return { observed: await stageOf(backend, root, error as Error), error }
    }
    const probe = internals(backend)
    const artifacts = await probe.listArtifacts()
    expect(snapshots).toHaveLength(artifacts.length)
    const sessions: Array<Record<string, unknown>> = []
    for (const snap of snapshots) {
      expect(Object.keys(snap).sort()).toEqual(['header', 'revision', 'sizeBytes'])
      const matches = artifacts.filter(artifact => artifact.header.id === snap.header.id)
      expect(matches).toHaveLength(1)
      const artifact = matches[0] as Artifact
      expect(snap.header).toStrictEqual(artifact.header)
      // `list` reads the size from a separate `fs.stat` that follows links.
      expect(snap.sizeBytes).toBe((await stat(artifact.path)).size)
      const selected = await probe.resolveGeneration(dirname(artifact.path))
      expect(selected?.sourcePath).toBe(artifact.path)
      const header = snap.header as unknown as Record<string, unknown>
      expect(Object.keys(header).every(key => (HEADER_KEYS as readonly string[]).includes(key))).toBe(true)
      sessions.push({
        path: relativePath(root, artifact.path),
        storedVersion: selected?.sourceVersion,
        header: Object.fromEntries(HEADER_KEYS.map(key => [key, header[key] ?? null])),
      })
    }
    sessions.sort((left, right) => (left.path as string) < (right.path as string) ? -1 : 1)
    return { observed: { outcome: 'listed', sessions } }
  } finally {
    await ctx.fiber.dispose()
  }
}

function expectShape(id: string, expected: Expected): void {
  const keys = Object.keys(expected).sort()
  if (expected.outcome === 'listed') {
    expect(keys, id).toEqual(['outcome', 'sessions'])
    const sessions = expected.sessions as Array<Record<string, unknown>>
    const paths = sessions.map(session => session.path as string)
    expect(paths, id).toEqual([...paths].sort())
    for (const session of sessions) {
      expect(Object.keys(session).sort(), id).toEqual(['header', 'path', 'storedVersion'])
      expect(Object.keys(session.header as object), id).toEqual([...HEADER_KEYS])
    }
  } else if (expected.outcome === 'refused') {
    const required = ['kind', 'outcome', 'path', 'reason', 'stage']
    expect(keys.filter(key => key !== 'message'), id).toEqual(required)
    expect(STAGES, id).toContain(expected.stage)
    if (expected.reason === 'duplicate-id') expect([expected.stage, expected.path], id).toEqual(['discovery', null])
  } else {
    expect(keys, id).toEqual(['code', 'outcome'])
  }
}

let scratch: string | undefined

beforeAll(async () => {
  scratch = await mkdtemp(join(tmpdir(), 'bake-list-conformance-'))
})

afterAll(async () => {
  if (scratch !== undefined) await rm(scratch, { recursive: true, force: true })
})

describe('shared Session list cases', () => {
  it('pins the table', () => {
    expect(Object.keys(table)).toEqual(['schema', 'version', 'provenance', 'notes', 'defaults', 'cases'])
    expect(table.schema).toBe('bake/session-conformance/list-cases')
    expect(table.version).toBe(1)
    expect(table.defaults).toEqual({ maxHeaderBytes: 65536, maxEntries: 1024 })
    expect(table.cases).toHaveLength(CASE_COUNT)
    expect(new Set(table.cases.map(entry => entry.id)).size).toBe(CASE_COUNT)
    for (const entry of table.cases) {
      expect(Object.keys(entry).every(key => CASE_KEYS.includes(key)), entry.id).toBe(true)
      expect(entry.platforms === undefined, entry.id).toBe(entry.platformReason === undefined)
      if (entry.platforms !== undefined) expect(entry.platforms, entry.id).toEqual(['posix'])
      expectShape(entry.id, entry.ts)
      // TypeScript refusals here are all plain errors; unsupported headers are skipped.
      if (entry.ts.outcome === 'refused') expect(entry.ts.kind, entry.id).toBe('invalid')
      const native = entry.rust ?? entry.ts
      expect(native.outcome === 'failure', entry.id).toBe(entry.nativeDiagnostic !== undefined)
      // A Rust override names a native budget or representation limit.
      if (entry.rust !== undefined) {
        expectShape(entry.id, entry.rust)
        expect(entry.rust.kind, entry.id).toBe('native-limit')
      }
    }
  })

  for (const entry of table.cases) {
    it.skipIf(entry.platforms !== undefined && process.platform === 'win32')(entry.id, async () => {
      const caseDir = await mkdtemp(join(scratch as string, 'case-'))
      try {
        await build(caseDir, entry.layout)
        const before = await snapshot(caseDir)
        const { observed, error } = await observe(entry, caseDir)
        const { message, ...shape } = entry.ts
        expect(observed).toStrictEqual(shape)
        if (message !== undefined) expect((error as Error).message).toContain(String(message))
        expect(await snapshot(caseDir)).toEqual(before)
      } finally {
        await rm(caseDir, { recursive: true, force: true })
      }
    })
  }
})
