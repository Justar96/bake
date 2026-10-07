/**
 * Shared source-authored Zstd logs through the production backend, including
 * physical recovery offsets and restored projections. Native-only byte budgets
 * do not constrain this arm; committed plaintext offsets are checked by Rust.
 */
import { readFileSync } from 'node:fs'
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { dirname, join } from 'node:path'
import { Context } from '@deepseek-ai/cordis'
import { describe, expect, it } from 'vitest'
import { Session, SessionId, interruptedTurnClosers } from 'bake-session'
import type { SessionEvent, SessionHeader, SessionLogOffset } from 'bake-session'
import { currentSessionMessageProjections } from 'bake-session-format-catalog/message-projections'
import JsonlSessionPersistence from '../src/index.ts'
import { logPath } from '../src/format.ts'

interface Prefix {
  meta: SessionHeader
  events: SessionEvent[]
  inheritedEventCount: SessionLogOffset
  tornTruncateTo?: number
  recoveredTail: SessionEvent[]
}
interface Case {
  id: string
  hex: string
  expected: Record<string, unknown> & { outcome: string; message?: string; rows?: unknown[]; committedBytes?: number }
}
const table = JSON.parse(readFileSync(new URL('../../../../conformance/session/zstd-cases.json', import.meta.url), 'utf8')) as {
  schema: string
  version: number
  cases: Case[]
}

/** Find the production scan's own message beneath its path-bearing wrapper. */
function messages(error: unknown): string[] {
  if (!(error instanceof Error)) throw error
  return [error.message, ...(error.cause instanceof Error ? messages(error.cause) : [])]
}

describe('shared Zstd restoration cases', () => {
  it('pins the corpus and distinct case names', () => {
    expect(table.schema).toBe('bake/session-conformance/zstd-cases')
    expect(table.version).toBe(1)
    expect(table.cases).toHaveLength(46)
    expect(new Set(table.cases.map(entry => entry.id)).size).toBe(46)
  })
  for (const entry of table.cases) {
    it(entry.id, async () => {
      const root = await mkdtemp(join(tmpdir(), 'bake-zstd-conformance-'))
      const ctx = new Context()
      try {
        await ctx.plugin(JsonlSessionPersistence, { root, compression: 'zstd' })
        const id = SessionId('zstd-fixture')
        const path = logPath(root, undefined, id, 'zstd')
        const bytes = Buffer.from(entry.hex, 'hex')
        await mkdir(dirname(path), { recursive: true })
        await writeFile(path, bytes)
        // Observe the backend's stored view before a write-open can repair it.
        const load = Reflect.get(ctx.sessionPersistence, 'requireStoredLog') as (id: SessionId) => Promise<Prefix>
        let stored: Prefix
        try {
          stored = await load.call(ctx.sessionPersistence, id)
        } catch (error) {
          if (entry.expected.outcome === 'unsupported') {
            expect(error).toBeInstanceOf(Error)
            expect((error as Error).constructor.name).toBe('SessionFormatUnsupportedError')
          } else {
            expect(entry.expected.outcome).toBe('refused')
            expect(messages(error)).toContain(entry.expected.message)
          }
          return
        }
        expect(entry.expected.outcome).toBe('restored')
        const closers = interruptedTurnClosers(stored.events)
        const session = Session.fromRestore(id, [...stored.events, ...closers], stored.meta,
          stored.inheritedEventCount, 'detached', currentSessionMessageProjections)
        // Committed plaintext bytes are not exposed by the stored-file view.
        // The independent table checks them on Rust; all other fields tie here.
        const { committedBytes: _committedBytes, ...expected } = entry.expected
        expect({
          outcome: 'restored', header: stored.meta, rows: stored.events,
          inheritedEventCount: stored.inheritedEventCount,
          torn: stored.tornTruncateTo === undefined ? null : {
            truncateTo: stored.tornTruncateTo, recoveredFrom: stored.events.length - stored.recoveredTail.length,
          },
          closers, endSeedAppended: session.seq > stored.events.length + closers.length,
          messages: session.deriveMessages(), requestHeader: session.requestHeader() ?? null,
          toolHistory: session.toolHistory(), requestContext: session.requestContext() ?? null,
        }).toStrictEqual(expected)
        expect(readFileSync(path)).toEqual(bytes)
      } finally {
        try {
          await ctx.fiber.dispose()
        } finally {
          await rm(root, { recursive: true, force: true })
        }
      }
    })
  }
})
