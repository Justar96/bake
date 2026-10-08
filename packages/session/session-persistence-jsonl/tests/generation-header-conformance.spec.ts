/**
 * Runs the shared cases in `conformance/session/generation-header-cases.json`
 * through the production backend's public `stat(id)`, which reads one selected
 * generation's header with `readGenerationHeader`. The development Rust reader
 * `read_generation_header_record` checks the same table, so each outcome here
 * is the oracle for its Rust expectation; a `rust` limit names a case Rust
 * declines without changing what TypeScript must produce.
 *
 * `stat` also checks the stored identity, which the Rust reader leaves to its
 * caller, so a case that expects a header places its record at the path that
 * header's id and cwd name. Other cases use id `s1` without a cwd. The table
 * cannot place a Windows `cwd` on a POSIX host, so each host checks only its
 * own platform's expectation.
 */
import { readFileSync } from 'node:fs'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { dirname, join } from 'node:path'
import { afterAll, beforeAll, describe, expect, it } from 'vitest'
import { Context } from '@deepseek-ai/cordis'
import { SessionId } from 'bake-session'
import { SessionFormatUnsupportedError, SessionPersistenceCorruptionError } from 'bake-session-persistence'
import JsonlSessionPersistence from '../src/index.ts'
import { generationLogPath } from '../src/format.ts'

type Outcome =
  | { outcome: 'absent' }
  | { outcome: 'header'; header: Record<string, unknown> }
  | { outcome: 'rejected' | 'unsupported'; message: string }
interface GenerationHeaderCase {
  id: string
  sourceVersion: number
  record?: string
  bytesHex?: string
  expect: Outcome
  win32?: Outcome
  rust?: { limit: string }
}

const REPO = new URL('../../../../', import.meta.url)
const CASE_COUNT = 95
const LIMITS = ['invalid-utf8', 'json-parser', 'float-lexeme', 'version-diagnostic']
const table = JSON.parse(readFileSync(new URL('conformance/session/generation-header-cases.json', REPO), 'utf8')) as {
  schema: string
  version: number
  cases: GenerationHeaderCase[]
}

function bytesOf(entry: GenerationHeaderCase): Buffer {
  if ((entry.record === undefined) === (entry.bytesHex === undefined)) throw new Error(`${entry.id}: one record source`)
  return entry.record === undefined ? Buffer.from(entry.bytesHex as string, 'hex') : Buffer.from(`${entry.record}\n`)
}

async function observe(root: string, entry: GenerationHeaderCase, expected: Outcome): Promise<Outcome> {
  const placed = expected.outcome === 'header' ? expected.header : { id: 's1' }
  const id = SessionId(placed['id'] as string)
  const path = generationLogPath(root, placed['cwd'] as string | undefined, id, entry.sourceVersion, 'none')
  await mkdir(dirname(path), { recursive: true })
  const bytes = bytesOf(entry)
  await writeFile(path, bytes)
  const ctx = new Context()
  try {
    await ctx.plugin(JsonlSessionPersistence, { root, compression: 'none' })
    const backend = ctx.sessionPersistence as unknown as JsonlSessionPersistence
    let observed: Outcome
    try {
      const snapshot = await backend.stat(id)
      observed = snapshot === undefined ? { outcome: 'absent' } : { outcome: 'header', header: { ...snapshot.header } }
    } catch (error) {
      if (error instanceof SessionFormatUnsupportedError) {
        const suffix = ` (raw log: ${path})`
        expect(error.message.endsWith(suffix), error.message).toBe(true)
        observed = { outcome: 'unsupported', message: error.message.slice(0, -suffix.length) }
      } else {
        expect(error).toBeInstanceOf(Error)
        expect(error).not.toBeInstanceOf(SessionPersistenceCorruptionError)
        expect(error).not.toBeInstanceOf(TypeError)
        observed = { outcome: 'rejected', message: (error as Error).message }
      }
    }
    expect(await readFile(path)).toEqual(bytes)
    return observed
  } finally {
    await ctx.fiber.dispose()
  }
}

let scratch: string | undefined

beforeAll(async () => {
  scratch = await mkdtemp(join(tmpdir(), 'bake-generation-header-conformance-'))
})

afterAll(async () => {
  if (scratch !== undefined) await rm(scratch, { recursive: true, force: true })
})

describe('shared Session generation header cases', () => {
  it('pins the table', () => {
    expect(table.schema).toBe('bake/session-format-conformance/generation-header-cases')
    expect(table.version).toBe(1)
    expect(table.cases).toHaveLength(CASE_COUNT)
    expect(new Set(table.cases.map(entry => entry.id)).size).toBe(CASE_COUNT)
    for (const entry of table.cases) {
      if (entry.rust !== undefined) expect(LIMITS, entry.id).toContain(entry.rust.limit)
    }
  })

  for (const entry of table.cases) {
    it(entry.id, async () => {
      const expected = process.platform === 'win32' ? entry.win32 ?? entry.expect : entry.expect
      const root = await mkdtemp(join(scratch as string, 'case-'))
      try {
        expect(await observe(root, entry, expected)).toStrictEqual(expected)
      } finally {
        await rm(root, { recursive: true, force: true })
      }
    })
  }
})
