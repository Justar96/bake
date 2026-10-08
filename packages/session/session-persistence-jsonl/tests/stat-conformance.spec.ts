/**
 * Shared Session stat cases through the production JSONL backend under Node.
 * Each case builds its layout in an owned temporary directory and calls the
 * public `stat(id)`. To name the stage of a rejection, the spec first awaits
 * the backend's own memoized root-wide check and then its `findLog`, as the
 * lookup spec does; `stat` must then reject with the same error. The selected
 * generation's path and version come from that `findLog` call, because
 * `stat` reports neither. Native-only budgets in the table constrain only
 * the Rust arm.
 */
import { createHash } from 'node:crypto'
import { readFileSync } from 'node:fs'
import { lstat, mkdir, mkdtemp, readFile, readdir, readlink, rm, stat, symlink, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { dirname, join, relative, sep } from 'node:path'
import { afterAll, beforeAll, describe, expect, it } from 'vitest'
import { Context } from '@deepseek-ai/cordis'
import { SessionId } from 'bake-session'
import { SessionFormatUnsupportedError } from 'bake-session-persistence'
import JsonlSessionPersistence from '../src/index.ts'

type Entry =
  | { dir: string }
  | { file: string; text: string }
  | { file: string; frames: string[]; appendHex?: string }
  | { symlink: string; target: string }
type Expected = Record<string, unknown> & { outcome: 'found' | 'absent' | 'refused' | 'failure' }
interface StatCase {
  id: string
  platforms?: Array<'posix'>
  platformReason?: string
  compression?: 'none' | 'zstd'
  root: string
  sessionId: string
  maxHeaderBytes?: number
  maxEntries?: number
  layout: Entry[]
  ts: Expected
  rust?: Expected
  nativeDiagnostic?: { text: string; path: string }
}
interface Table {
  schema: string
  version: number
  defaults: { maxHeaderBytes: number; maxEntries: number }
  cases: StatCase[]
}
interface Selected { sourcePath: string; sourceVersion: number }

const REPO = new URL('../../../../', import.meta.url)
const CASE_COUNT = 46
const HEADER_KEYS = ['version', 'id', 'createdAt', 'cwd', 'parentSession', 'isSeeded', 'origin', 'delegationDepth',
  'agentPreset'] as const
const table = JSON.parse(readFileSync(new URL('conformance/session/stat-cases.json', REPO), 'utf8')) as Table

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
      out.push(`${prefix}${name} dir`, ...await snapshot(path, `${prefix}${name}/`))
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
  [/uses log format v\d+, but this harness reads only/, 'newer-format'],
  [/^corrupt Zstandard session log: header frame failed validation: /, 'corrupt-header-frame'],
  [/requested id ".*" does not match header id/s, 'id-mismatch'],
  [/header id ".*" and cwd identify/s, 'path-mismatch'],
]

function reasonOf(error: unknown): string | null {
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

function refusal(stage: string, error: unknown, path: string | null): Record<string, unknown> {
  if (isErrno(error)) return { outcome: 'failure', code: error.code }
  const reason = reasonOf(error)
  const kind = error instanceof SessionFormatUnsupportedError ? 'unsupported' : 'invalid'
  return { outcome: 'refused', stage, reason, kind, path }
}

async function rejection(promise: Promise<unknown>): Promise<unknown> {
  return promise.then(() => {
    throw new Error('expected a rejection')
  }, (error: unknown) => error)
}

/** Observe one case through the production backend; the Context is disposed before returning. */
async function observe(entry: StatCase, caseDir: string): Promise<{ observed: Record<string, unknown>; error?: unknown }> {
  const config = { root: join(caseDir, entry.root), ...entry.compression === undefined ? {} : { compression: entry.compression } }
  const root = config.root
  const id = SessionId(entry.sessionId)
  const ctx = new Context()
  try {
    await ctx.plugin(JsonlSessionPersistence, config)
    const backend = ctx.sessionPersistence as unknown as JsonlSessionPersistence
    const ensure = Reflect.get(backend, 'ensureRootEncoding') as () => Promise<void>
    const findLog = Reflect.get(backend, 'findLog') as (id: SessionId) => Promise<Selected | undefined>
    try {
      await ensure.call(backend)
    } catch (error) {
      expect(await rejection(backend.stat(id))).toBe(error)
      return { observed: refusal('layout', error, artifactPath(root, error)), error }
    }
    let selected: Selected | undefined
    try {
      selected = await findLog.call(backend, id)
    } catch (error) {
      expect((await rejection(backend.stat(id)) as Error).message).toBe((error as Error).message)
      return { observed: refusal('lookup', error, artifactPath(root, error)), error }
    }
    if (selected === undefined) {
      expect(await backend.stat(id)).toBeUndefined()
      return { observed: { outcome: 'absent', path: null, storedVersion: null } }
    }
    const path = relativePath(root, selected.sourcePath)
    let snapshot: Awaited<ReturnType<typeof backend.stat>>
    try {
      snapshot = await backend.stat(id)
    } catch (error) {
      const reason = reasonOf(error)
      const stage = reason === 'id-mismatch' || reason === 'path-mismatch' ? 'identity' : 'header'
      return { observed: refusal(stage, error, path), error }
    }
    if (snapshot === undefined) return { observed: { outcome: 'absent', path, storedVersion: selected.sourceVersion } }
    // `stat` reads the size from a separate `fs.stat` that follows links.
    expect(snapshot.sizeBytes).toBe((await stat(selected.sourcePath)).size)
    const header = snapshot.header as unknown as Record<string, unknown>
    expect(Object.keys(header).every(key => (HEADER_KEYS as readonly string[]).includes(key))).toBe(true)
    return {
      observed: {
        outcome: 'found',
        path,
        storedVersion: selected.sourceVersion,
        header: Object.fromEntries(HEADER_KEYS.map(key => [key, header[key] ?? null])),
      },
    }
  } finally {
    await ctx.fiber.dispose()
  }
}

let scratch: string | undefined

beforeAll(async () => {
  scratch = await mkdtemp(join(tmpdir(), 'bake-stat-conformance-'))
})

afterAll(async () => {
  if (scratch !== undefined) await rm(scratch, { recursive: true, force: true })
})

describe('shared Session stat cases', () => {
  it('pins the table', () => {
    expect(table.schema).toBe('bake/session-conformance/stat-cases')
    expect(table.version).toBe(1)
    expect(table.defaults).toEqual({ maxHeaderBytes: 65536, maxEntries: 1024 })
    expect(table.cases).toHaveLength(CASE_COUNT)
    expect(new Set(table.cases.map(entry => entry.id)).size).toBe(CASE_COUNT)
    for (const entry of table.cases) {
      expect(entry.platforms === undefined, entry.id).toBe(entry.platformReason === undefined)
      const native = entry.rust ?? entry.ts
      expect(native.outcome === 'failure', entry.id).toBe(entry.nativeDiagnostic !== undefined)
      // A Rust override is only ever a native budget.
      if (entry.rust !== undefined) expect(entry.rust.kind, entry.id).toBe('native-limit')
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
