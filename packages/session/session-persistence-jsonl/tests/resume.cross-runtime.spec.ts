/**
 * Cross-runtime resume between the TypeScript JSONL backend and the Rust
 * `bake-session` writer, over one shared root and real independent
 * processes (D31): TypeScript writes and Rust resumes, Rust writes and
 * TypeScript resumes, each after the writer was killed mid-append, and an
 * older generation migrated by either runtime and resumed by the other, in
 * a plain root and in a Zstd root, TypeScript's default.
 * Migrations also compete: one runtime's migration pauses after it read
 * and migrated the source and before it publishes, holding the write lock,
 * while the other runtime is refused, reads the unpublished migration, and
 * then resumes the one target; a migrating writer of either runtime is
 * killed while writing its temporary file, after linking it into place, or
 * while stopped with `SIGSTOP`; and a read-only Session directory refuses
 * both runtimes' migrations alike. The Rust side is the development-only
 * `bake-session-lease-probe` binary from `rust/crates/bake-conformance`;
 * the TypeScript side is the production backend in this process and, for a
 * writer killed or paused mid-write, the built package running in
 * `fixtures/resume-cross-runtime-writer.mjs` or
 * `fixtures/migration-cross-runtime-writer.mjs`.
 *
 * Expected bytes are TypeScript's: each event row is `JSON.stringify` of the
 * event and an LF, as the backend writes it, each Zstd batch is the frame
 * the backend's own `compressZstdFrame` writes for its rows, and a migrated
 * log is the one TypeScript's own migration writes from the same source in
 * another root.
 * Every final log is read back through a fresh TypeScript backend, every
 * older generation is hashed before and after, and every Session directory
 * is listed at the end, so a file either runtime leaves outside its design
 * fails the case.
 *
 * Opt-in, as `lease.cross-runtime.spec.ts` is: run `bun run test:rust:lease`,
 * which passes the built probe to both specs, or set `DSH_RUST_LEASE_PROBE`
 * to the probe's absolute path for a direct Vitest run. With neither name the
 * suite is skipped by name. The writer needs the built package
 * (`bun run build:runtime`).
 */

import { spawn, type ChildProcess } from 'node:child_process'
import { createHash } from 'node:crypto'
import { existsSync, readFileSync } from 'node:fs'
import { chmod, mkdir, mkdtemp, readdir, readFile, rm, stat, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { isAbsolute, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { afterEach, beforeAll, describe, expect, it } from 'vitest'
import { Context } from '@deepseek-ai/cordis'
import { SESSION_FORMAT_VERSION, SessionId, SessionSeq } from 'bake-session'
import type { SessionEvent } from 'bake-session'
import { SessionAlreadyOwnedError, SessionPersistenceNotFoundError } from 'bake-session-persistence'
import type { SessionPersistence } from 'bake-session-persistence'
import JsonlSessionPersistence from 'bake-session-persistence-jsonl'
import { LEASE_FILENAME } from '../src/lease.ts'
import { compressZstdFrame } from '../src/zstd.ts'

/** The probe path, or `undefined` when the suite is not opted in. */
const PROBE = process.env.BAKE_RUST_LEASE_PROBE ?? process.env.DSH_RUST_LEASE_PROBE
const WRITER = fileURLToPath(new URL('./fixtures/resume-cross-runtime-writer.mjs', import.meta.url))
const MIGRATOR = fileURLToPath(new URL('./fixtures/migration-cross-runtime-writer.mjs', import.meta.url))
const BUILT_LIB = fileURLToPath(new URL('../lib/index.js', import.meta.url))
const MIGRATED_CASES = fileURLToPath(new URL('../../../../conformance/session/migrated-restore-cases.json', import.meta.url))
const POSIX = process.platform !== 'win32'
// Root ignores a directory's write permission, so the read-only case would refuse nothing.
const ROOT = process.getuid?.() === 0

/** Bound on every single wait for a child's output or exit. */
const WAIT_MS = 20_000
/** Bound on captured stdout and stderr per child. */
const MAX_OUTPUT = 64 * 1024

type Compression = 'none' | 'zstd'

interface Exit {
  readonly code: number | null
  readonly signal: NodeJS.Signals | null
}

/** One owned child: bounded output capture, line reads and exit waits with a deadline. */
class Child {
  readonly proc: ChildProcess
  exit: Exit | undefined
  private stdout = ''
  private stderr = ''
  private consumed = 0
  private overflow = false
  private readonly watchers = new Set<() => void>()

  constructor(readonly label: string, command: string, args: readonly string[], at: Area, stdin: 'pipe' | 'ignore') {
    this.proc = spawn(command, args, { cwd: at.cwd, env: at.env, stdio: [stdin, 'pipe', 'pipe'], windowsHide: true })
    children.add(this)
    this.proc.once('close', (code, signal) => {
      this.exit = { code, signal }
      this.notify()
    })
    this.proc.once('error', () => this.notify())
    this.proc.stdin?.on('error', () => {})
    const capture = (keep: (chunk: string) => void) => (chunk: string): void => {
      if (this.stdout.length + this.stderr.length + chunk.length > MAX_OUTPUT) this.overflow = true
      else keep(chunk)
      this.notify()
    }
    this.proc.stdout!.setEncoding('utf8').on('data', capture((chunk) => { this.stdout += chunk }))
    this.proc.stderr!.setEncoding('utf8').on('data', capture((chunk) => { this.stderr += chunk }))
  }

  describe(): string {
    return `${this.label} (pid ${this.proc.pid}): exit ${JSON.stringify(this.exit)}, stdout ${JSON.stringify(this.stdout)}, stderr ${JSON.stringify(this.stderr)}`
  }

  private notify(): void {
    for (const watcher of [...this.watchers]) watcher()
  }

  private waitFor<T>(what: string, check: () => T | undefined): Promise<T> {
    return new Promise<T>((resolve, reject) => {
      const done = (): void => {
        clearTimeout(timer)
        this.watchers.delete(poke)
      }
      const poke = (): void => {
        let value: T | undefined
        try {
          if (this.overflow) throw new Error(`output exceeded ${MAX_OUTPUT} bytes: ${this.describe()}`)
          value = check()
        } catch (error) {
          done()
          reject(error instanceof Error ? error : new Error(String(error)))
          return
        }
        if (value !== undefined) {
          done()
          resolve(value)
        }
      }
      const timer = setTimeout(() => {
        done()
        reject(new Error(`timed out after ${WAIT_MS} ms waiting for ${what}: ${this.describe()}`))
      }, WAIT_MS)
      this.watchers.add(poke)
      poke()
    })
  }

  /** The next complete stdout line, parsed as JSON. */
  nextJson(): Promise<unknown> {
    return this.waitFor('a stdout line', () => {
      const end = this.stdout.indexOf('\n', this.consumed)
      if (end >= 0) {
        const line = this.stdout.slice(this.consumed, end)
        this.consumed = end + 1
        try {
          return { value: JSON.parse(line) as unknown }
        } catch {
          throw new Error(`stdout line is not JSON: ${this.describe()}`)
        }
      }
      if (this.exit !== undefined) throw new Error(`closed before a complete stdout line: ${this.describe()}`)
      return undefined
    }).then(line => line.value)
  }

  /** Stdout not yet returned, and stderr; both must be empty for a clean exchange. */
  rest(): { stdout: string; stderr: string } {
    return { stdout: this.stdout.slice(this.consumed), stderr: this.stderr }
  }

  closed(): Promise<Exit> {
    return this.waitFor('exit', () => this.exit)
  }

  send(line: string): void {
    this.proc.stdin!.write(`${line}\n`)
  }

  /** Crash a live child: SIGKILL (TerminateProcess on Windows), no release code runs. */
  async crash(): Promise<void> {
    expect(this.exit, this.describe()).toBeUndefined()
    expect(this.proc.kill('SIGKILL'), this.describe()).toBe(true)
    const exit = await this.closed()
    if (POSIX) expect(exit, this.describe()).toEqual({ code: null, signal: 'SIGKILL' })
    else expect(exit.signal, this.describe()).toBe('SIGKILL')
  }

  /** Teardown: kill when still running, then await the close. */
  async reap(): Promise<void> {
    if (this.exit === undefined) this.proc.kill('SIGKILL')
    await this.closed()
  }
}

const children = new Set<Child>()
const contexts = new Set<Context>()
const dirs: string[] = []

afterEach(async () => {
  // Children first: on Windows their open handles keep the directories busy.
  const live = [...children]
  children.clear()
  const exits = await Promise.allSettled(live.map(child => child.reap()))
  for (const ctx of [...contexts]) await ctx.fiber.dispose()
  contexts.clear()
  for (const dir of dirs.splice(0)) await rm(dir, { recursive: true, force: true, maxRetries: 5 })
  for (const exit of exits) if (exit.status === 'rejected') throw exit.reason
})

/** A private test area: the Session root plus a home and temp directory for children. */
interface Area {
  readonly root: string
  readonly env: NodeJS.ProcessEnv
  readonly cwd: string
}

async function area(): Promise<Area> {
  const base = await mkdtemp(join(tmpdir(), 'bake-resume-xrt-'))
  dirs.push(base)
  const root = join(base, 'sessions')
  const home = join(base, 'home')
  const temp = join(base, 'tmp')
  await Promise.all([mkdir(root), mkdir(home), mkdir(temp)])
  // No model credentials, private homes, no inherited BAKE_/DSH_ names.
  const env: NodeJS.ProcessEnv = { HOME: home, USERPROFILE: home, BAKE_HOME: home, DSH_HOME: home, TMPDIR: temp, TMP: temp, TEMP: temp }
  for (const name of ['PATH', 'SystemRoot', 'SystemDrive', 'windir', 'PATHEXT', 'ComSpec']) {
    const value = process.env[name]
    if (value !== undefined) env[name] = value
  }
  return { root, env, cwd: home }
}

/** Run `use` against a fresh, independent backend, disposed afterwards. */
async function withBackend<T>(root: string, compression: Compression, use: (persistence: SessionPersistence) => Promise<T>): Promise<T> {
  const ctx = new Context()
  contexts.add(ctx)
  try {
    await ctx.plugin(JsonlSessionPersistence, { root, compression })
    return await use(ctx.sessionPersistence)
  } finally {
    contexts.delete(ctx)
    await ctx.fiber.dispose()
  }
}

/** Every event a fresh backend's read handle returns. */
async function readEvents(root: string, id: string, compression: Compression = 'none'): Promise<readonly SessionEvent[]> {
  return withBackend(root, compression, async (persistence) => {
    const reader = await persistence.open(SessionId(id), 'read')
    try {
      return (await reader.read()).events
    } finally {
      await reader.close()
    }
  })
}

async function readSeqs(root: string, id: string, compression: Compression = 'none'): Promise<number[]> {
  return (await readEvents(root, id, compression)).map(event => event.seq)
}

/** Write-open through a fresh backend, append `events` when given, and close. */
async function tsResume(root: string, id: string, events: readonly SessionEvent[], compression: Compression = 'none'): Promise<void> {
  await withBackend(root, compression, async (persistence) => {
    const handle = await persistence.open(SessionId(id), 'write')
    try {
      if (events.length > 0) {
        await handle.append(events)
        await handle.flush()
      }
    } finally {
      await handle.close()
    }
  })
}

/** What a fresh backend's write `open` throws, or `undefined` when it opens. */
async function tsOpenError(root: string, id: string, compression: Compression = 'none'): Promise<unknown> {
  return withBackend(root, compression, async (persistence) => {
    try {
      await (await persistence.open(SessionId(id), 'write')).close()
      return undefined
    } catch (error) {
      return error
    }
  })
}

const turnStart = (seq: number, time: number, turn: number): SessionEvent =>
  ({ type: 'turn/start', seq: SessionSeq(seq), time, data: { turn } }) as SessionEvent
const turnEnd = (seq: number, time: number, turn: number): SessionEvent =>
  ({ type: 'turn/end', seq: SessionSeq(seq), time, data: { turn, reason: { kind: 'completed' } } }) as SessionEvent
/** The rows TypeScript writes for `events`: each `JSON.stringify` and an LF. */
const rows = (...events: readonly SessionEvent[]): string => events.map(event => `${JSON.stringify(event)}\n`).join('')
/** The probe's `open <seq>` row. */
const probeRow = (seq: number): string => rows(turnStart(seq, 3, 2))
/** The header both runtimes write for a Session the probe or the writer created. */
const HEADER = '{"type":"session","version":3,"id":"<id>","createdAt":1000,"isSeeded":false,"delegationDepth":0}\n'
const FIRST_TURN = [turnStart(0, 1, 1), turnEnd(1, 2, 1)] as const

const sessionDir = (root: string, id: string): string => join(root, '_no-cwd', id)
const plainLog = (root: string, id: string): string => join(sessionDir(root, id), 'session.v3.jsonl')
const sha256 = (bytes: Uint8Array): string => createHash('sha256').update(bytes).digest('hex')

/** Every file under `root` but lock files, by relative path, as SHA-256 of its bytes. */
async function snapshot(root: string): Promise<Record<string, string>> {
  const entries: Record<string, string> = {}
  for (const entry of await readdir(root, { recursive: true, withFileTypes: true })) {
    if (!entry.isFile() || entry.name === LEASE_FILENAME) continue
    const path = join(entry.parentPath, entry.name)
    entries[path.slice(root.length + 1).split('\\').join('/')] = sha256(await readFile(path))
  }
  return entries
}

/** The root's single project directory, `_no-cwd`, must hold exactly the Session directory `id` with exactly `names`. */
async function expectTree(root: string, id: string, names: readonly (string | RegExp)[]): Promise<void> {
  expect(await readdir(root)).toEqual(['_no-cwd'])
  expect(await readdir(join(root, '_no-cwd'))).toEqual([id])
  const listed = (await readdir(sessionDir(root, id))).sort()
  expect(listed, JSON.stringify(listed)).toHaveLength(names.length)
  for (const name of names) {
    const index = listed.findIndex(entry => typeof name === 'string' ? entry === name : name.test(entry))
    expect(index, `${String(name)} in ${JSON.stringify(listed)}`).toBeGreaterThanOrEqual(0)
    listed.splice(index, 1)
  }
}

/** Run the probe to completion: its exit, its one stdout line parsed, and nothing on stderr unless it failed. */
async function runProbe(at: Area, args: readonly string[]): Promise<{ exit: Exit; output: unknown; child: Child }> {
  const child = new Child(`probe ${args[0] ?? ''}`, PROBE!, args, at, 'ignore')
  const exit = await child.closed()
  const output = await child.nextJson()
  expect(child.rest(), child.describe()).toEqual({ stdout: '', stderr: '' })
  return { exit, output, child }
}

/** The probe's `open` must append `seq` and report every stored seq. */
async function rustResume(at: Area, id: string, seq: number): Promise<void> {
  const { exit, output, child } = await runProbe(at, ['open', at.root, id, String(seq)])
  expect(exit, child.describe()).toEqual({ code: 0, signal: null })
  expect(output).toEqual({ outcome: 'opened', seqs: Array.from({ length: seq + 1 }, (_, index) => index) })
}

/** The probe's `open` must be refused as owned, leaving every non-lock byte unchanged. */
async function expectRustOwned(at: Area, id: string): Promise<void> {
  const before = await snapshot(at.root)
  const { exit, output, child } = await runProbe(at, ['open', at.root, id, '2'])
  expect(exit, child.describe()).toEqual({ code: 3, signal: null })
  expect(output).toEqual({ outcome: 'owned', message: `session "${id}" is already owned by an active write handle` })
  expect(await snapshot(at.root)).toEqual(before)
}

/** A TypeScript write `open` must be refused as owned, leaving every non-lock byte unchanged. */
async function expectTsOwned(root: string, id: string): Promise<void> {
  const before = await snapshot(root)
  const error = await tsOpenError(root, id)
  expect(error).toBeInstanceOf(SessionAlreadyOwnedError)
  expect(await snapshot(root)).toEqual(before)
}

/** Create the Session through the probe's `hold`, then release it gracefully. */
async function rustCreate(at: Area, id: string): Promise<void> {
  const holder = new Child('probe hold', PROBE!, ['hold', at.root, id], at, 'pipe')
  expect(await holder.nextJson(), holder.describe()).toEqual({ state: 'holding' })
  holder.send('release')
  expect(await holder.nextJson(), holder.describe()).toEqual({ state: 'released' })
  expect(await holder.closed(), holder.describe()).toEqual({ code: 0, signal: null })
  expect(holder.rest(), holder.describe()).toEqual({ stdout: '', stderr: '' })
}

/** Start a probe that tears its writes after `bytes` bytes and await its `torn` line. */
async function rustTear(at: Area, args: readonly string[]): Promise<Child> {
  const writer = new Child(`probe ${args[0] ?? ''}`, PROBE!, args, at, 'pipe')
  expect(await writer.nextJson(), writer.describe()).toEqual({ state: 'torn' })
  expect(writer.rest(), writer.describe()).toEqual({ stdout: '', stderr: '' })
  return writer
}

/**
 * Start the TypeScript writer, await its committed first turn, and tear its
 * next append after `bytes` bytes.
 * @returns the live writer, the committed bytes, and the buffer it was appending.
 */
async function tsTear(at: Area, id: string, compression: Compression, bytes: number): Promise<TornWriter> {
  const writer = new Child('node writer', process.execPath, [WRITER, at.root, id, compression, String(bytes)], at, 'pipe')
  expect(await writer.nextJson(), writer.describe()).toEqual({ state: 'committed' })
  const path = compression === 'none' ? plainLog(at.root, id) : `${plainLog(at.root, id)}.zstd`
  const committed = await readFile(path)
  writer.send('tear')
  const torn = await writer.nextJson() as { state: string; content: string }
  expect(torn.state, writer.describe()).toBe('torn')
  expect(writer.rest(), writer.describe()).toEqual({ stdout: '', stderr: '' })
  const content = Buffer.from(torn.content, 'base64')
  expect(await readFile(path)).toEqual(Buffer.concat([committed, content.subarray(0, bytes)]))
  return { writer, committed, content }
}

interface TornWriter { readonly writer: Child; readonly committed: Buffer; readonly content: Buffer }

interface MigratedCase { readonly id: string; readonly version: number; readonly header: string; readonly rows: readonly string[] }

const zstdLog = (root: string, id: string): string => `${plainLog(root, id)}.zstd`
/** The frame TypeScript writes for one batch's rows, or a header line. */
const frame = (text: string): Promise<Buffer> => compressZstdFrame(text)

/** The probe's `zstd-open` must append `seq` and report every stored seq. */
async function rustZstdResume(at: Area, id: string, seq: number): Promise<void> {
  const { exit, output, child } = await runProbe(at, ['zstd-open', at.root, id, String(seq)])
  expect(exit, child.describe()).toEqual({ code: 0, signal: null })
  expect(output).toEqual({ outcome: 'opened', seqs: Array.from({ length: seq + 1 }, (_, index) => index) })
}

/** The probe's `zstd-open` must be refused as owned, leaving every non-lock byte unchanged. */
async function expectRustZstdOwned(at: Area, id: string): Promise<void> {
  const before = await snapshot(at.root)
  const { exit, output, child } = await runProbe(at, ['zstd-open', at.root, id, '2'])
  expect(exit, child.describe()).toEqual({ code: 3, signal: null })
  expect(output).toEqual({ outcome: 'owned', message: `session "${id}" is already owned by an active write handle` })
  expect(await snapshot(at.root)).toEqual(before)
}

/** Create the Zstd Session through the probe's `zstd-hold`, then release it gracefully. */
async function rustZstdCreate(at: Area, id: string): Promise<void> {
  const holder = new Child('probe zstd-hold', PROBE!, ['zstd-hold', at.root, id], at, 'pipe')
  expect(await holder.nextJson(), holder.describe()).toEqual({ state: 'holding' })
  holder.send('release')
  expect(await holder.nextJson(), holder.describe()).toEqual({ state: 'released' })
  expect(await holder.closed(), holder.describe()).toEqual({ code: 0, signal: null })
  expect(holder.rest(), holder.describe()).toEqual({ stdout: '', stderr: '' })
}

/**
 * The released source `caseId` as a Zstd writer of its version stores it,
 * its header and its rows each one frame, migrated and resumed as
 * {@link migratedResume} does in a plain root, in Zstd roots.
 */
async function migratedZstdResume(caseId: string): Promise<void> {
  const source = releasedSource(caseId)
  const headerEnd = source.text.indexOf('\n') + 1
  const bytes = Buffer.concat([await frame(source.text.slice(0, headerEnd)), await frame(source.text.slice(headerEnd))])
  const name = `${source.version === 0 ? 'session.jsonl' : `session.v${source.version}.jsonl`}.zstd`
  const seed = async (root: string): Promise<void> => {
    await mkdir(sessionDir(root, source.id), { recursive: true })
    await writeFile(join(sessionDir(root, source.id), name), bytes)
  }
  const oracle = await area()
  await seed(oracle.root)
  await tsResume(oracle.root, source.id, [], 'zstd')
  const migrated = await readFile(zstdLog(oracle.root, source.id))
  const next = (await readEvents(oracle.root, source.id, 'zstd')).length
  await tsResume(oracle.root, source.id, [turnStart(next, 3, 2)], 'zstd')
  const expected = Buffer.concat([migrated, await frame(probeRow(next))])
  expect(await readFile(zstdLog(oracle.root, source.id))).toEqual(expected)

  const rustFirst = await area()
  await seed(rustFirst.root)
  await rustZstdResume(rustFirst, source.id, next)

  const tsFirst = await area()
  await seed(tsFirst.root)
  await tsResume(tsFirst.root, source.id, [], 'zstd')
  expect(await readFile(zstdLog(tsFirst.root, source.id))).toEqual(migrated)
  await rustZstdResume(tsFirst, source.id, next)

  const oracleEvents = await readEvents(oracle.root, source.id, 'zstd')
  for (const at of [oracle, rustFirst, tsFirst]) {
    expect(await readFile(zstdLog(at.root, source.id))).toEqual(expected)
    expect(await readEvents(at.root, source.id, 'zstd')).toEqual(oracleEvents)
    expect(sha256(await readFile(join(sessionDir(at.root, source.id), name)))).toBe(sha256(bytes))
    await expectTree(at.root, source.id, [LEASE_FILENAME, name, 'session.v3.jsonl.zstd'])
  }
}

/** A released v0, v1, or v2 source, by case id from the committed migrated restoration table. */
function releasedSource(caseId: string): { id: string; version: number; text: string } {
  const table = JSON.parse(readFileSync(MIGRATED_CASES, 'utf8')) as { cases: MigratedCase[] }
  const found = table.cases.find(entry => entry.id === caseId)
  if (found === undefined) throw new Error(`no migrated restoration case ${caseId}`)
  const header = JSON.parse(found.header) as { id: string }
  return { id: header.id, version: found.version, text: [found.header, ...found.rows].map(row => `${row}\n`).join('') }
}

/** Seed `text` as the Session's canonical generation `version`, `session.jsonl` for 0, returning its name and hash. */
async function seedSource(root: string, id: string, version: number, text: string): Promise<{ name: string; hash: string }> {
  const name = version === 0 ? 'session.jsonl' : `session.v${version}.jsonl`
  await mkdir(sessionDir(root, id), { recursive: true })
  await writeFile(join(sessionDir(root, id), name), text)
  return { name, hash: sha256(Buffer.from(text)) }
}

/**
 * Migrate the released source `caseId` in three roots and resume each to one
 * appended row: TypeScript alone, the oracle; Rust migrating and appending,
 * read back by TypeScript; and TypeScript migrating, then Rust appending.
 * Every root must end with the oracle's bytes and events and the source's
 * original hash.
 */
async function migratedResume(caseId: string): Promise<void> {
  const source = releasedSource(caseId)
  const oracle = await area()
  const seeded = await seedSource(oracle.root, source.id, source.version, source.text)
  await tsResume(oracle.root, source.id, [])
  const migrated = await readFile(plainLog(oracle.root, source.id), 'utf8')
  const next = (await readEvents(oracle.root, source.id)).length
  expect(await readSeqs(oracle.root, source.id)).toEqual(Array.from({ length: next }, (_, index) => index))
  await tsResume(oracle.root, source.id, [turnStart(next, 3, 2)])
  const expected = migrated + probeRow(next)
  expect(await readFile(plainLog(oracle.root, source.id), 'utf8')).toBe(expected)

  const rustFirst = await area()
  await seedSource(rustFirst.root, source.id, source.version, source.text)
  await rustResume(rustFirst, source.id, next)

  const tsFirst = await area()
  await seedSource(tsFirst.root, source.id, source.version, source.text)
  await tsResume(tsFirst.root, source.id, [])
  expect(await readFile(plainLog(tsFirst.root, source.id), 'utf8')).toBe(migrated)
  await rustResume(tsFirst, source.id, next)

  const oracleEvents = await readEvents(oracle.root, source.id)
  for (const at of [oracle, rustFirst, tsFirst]) {
    expect(await readFile(plainLog(at.root, source.id), 'utf8')).toBe(expected)
    expect(await readEvents(at.root, source.id)).toEqual(oracleEvents)
    expect(sha256(await readFile(join(sessionDir(at.root, source.id), seeded.name)))).toBe(seeded.hash)
    await expectTree(at.root, source.id, [LEASE_FILENAME, seeded.name, 'session.v3.jsonl'])
  }
}

/** TypeScript's own migration of `source` in a fresh root: the migrated text, its event count, and its events after one appended row. */
async function oracleMigration(source: { id: string; version: number; text: string }): Promise<{ migrated: string; next: number }> {
  const oracle = await area()
  await seedSource(oracle.root, source.id, source.version, source.text)
  await tsResume(oracle.root, source.id, [])
  const migrated = await readFile(plainLog(oracle.root, source.id), 'utf8')
  const next = (await readEvents(oracle.root, source.id)).length
  expect(await readSeqs(oracle.root, source.id)).toEqual(Array.from({ length: next }, (_, index) => index))
  return { migrated, next }
}

type Runtime = 'Rust' | 'TypeScript'
const other = (runtime: Runtime): Runtime => runtime === 'Rust' ? 'TypeScript' : 'Rust'
const seqsTo = (last: number): number[] => Array.from({ length: last + 1 }, (_, index) => index)
const MIGRATION_TEMPORARY = /^session\.migration\.[^/]+\.jsonl\.tmp$/u

/**
 * Start a migrating write open of `id` in `runtime` that pauses `at` a step,
 * appending `turn/start` at `seq` once resumed, and await its `paused` line.
 */
async function pausedMigration(runtime: Runtime, at: Area, id: string, step: 'publish' | 'unlink', seq: number): Promise<Child> {
  const child = runtime === 'Rust'
    ? new Child('probe pause-migrate', PROBE!, ['pause-migrate', at.root, id, step, String(seq)], at, 'pipe')
    : new Child('node migrator', process.execPath, [MIGRATOR, at.root, id, `pause-${step}`, String(seq)], at, 'pipe')
  expect(await child.nextJson(), child.describe()).toEqual({ state: 'paused' })
  expect(child.rest(), child.describe()).toEqual({ stdout: '', stderr: '' })
  return child
}

/** Resume a paused migration and await its report of the appended `seq`. */
async function resumeMigration(runtime: Runtime, child: Child, seq: number): Promise<void> {
  child.send('go')
  const report = await child.nextJson()
  expect(report, child.describe()).toEqual(runtime === 'Rust' ? { outcome: 'opened', seqs: seqsTo(seq) } : { outcome: 'opened' })
  expect(await child.closed(), child.describe()).toEqual({ code: 0, signal: null })
  expect(child.rest(), child.describe()).toEqual({ stdout: '', stderr: '' })
}

/** `runtime`'s write open must be refused as owned, leaving every non-lock byte unchanged. */
async function expectOwned(runtime: Runtime, at: Area, id: string): Promise<void> {
  if (runtime === 'Rust') await expectRustOwned(at, id)
  else await expectTsOwned(at.root, id)
}

/** `runtime` write-opens `id` and appends `turn/start` at `seq`, at time 3 from Rust and 5 from TypeScript; returns the row. */
async function resumeIn(runtime: Runtime, at: Area, id: string, seq: number): Promise<string> {
  if (runtime === 'Rust') {
    await rustResume(at, id, seq)
    return probeRow(seq)
  }
  await tsResume(at.root, id, [turnStart(seq, 5, 2)])
  return rows(turnStart(seq, 5, 2))
}

/** Wait until `ps` reports `pid` stopped (state `T`), polling until the bound. */
async function observeStopped(pid: number, at: Area): Promise<void> {
  const deadline = Date.now() + WAIT_MS
  for (;;) {
    const ps = new Child('ps', 'ps', ['-o', 'stat=', '-p', String(pid)], at, 'ignore')
    const exit = await ps.closed()
    children.delete(ps)
    if (exit.code === 0 && ps.rest().stdout.trim().startsWith('T')) return
    if (Date.now() > deadline) throw new Error(`process ${pid} was not observed stopped within ${WAIT_MS} ms: ${ps.describe()}`)
    await new Promise(resolve => setTimeout(resolve, 10))
  }
}

/**
 * Two runtimes migrate one released source concurrently: `first`'s migration
 * pauses after reading and migrating the source and before publishing,
 * holding the write lock, so `second`'s write open is refused while a
 * TypeScript read open still reads the unpublished migration; resumed,
 * `first` publishes and appends, and `second` resumes the one target. The
 * root must end with TypeScript's own migration and both rows, the source
 * unchanged, and no temporary file.
 */
async function concurrentMigration(caseId: string, first: Runtime, stop?: 'SIGSTOP'): Promise<void> {
  const source = releasedSource(caseId)
  const { migrated, next } = await oracleMigration(source)
  const oracleEvents = await (async () => {
    const at = await area()
    await seedSource(at.root, source.id, source.version, source.text)
    return readEvents(at.root, source.id)
  })()
  const at = await area()
  const seeded = await seedSource(at.root, source.id, source.version, source.text)
  const paused = await pausedMigration(first, at, source.id, 'publish', next)
  await expectTree(at.root, source.id, [LEASE_FILENAME, seeded.name])
  if (stop !== undefined) {
    expect(paused.proc.kill(stop), paused.describe()).toBe(true)
    await observeStopped(paused.proc.pid!, at)
  }
  await expectOwned(other(first), at, source.id)
  // Reads take no lock: the unpublished migration reads as TypeScript migrates it.
  expect(await readEvents(at.root, source.id)).toEqual(oracleEvents)
  await expectTree(at.root, source.id, [LEASE_FILENAME, seeded.name])
  let expected: string
  if (stop === undefined) {
    await resumeMigration(first, paused, next)
    expected = migrated + probeRow(next)
    expect(await readFile(plainLog(at.root, source.id), 'utf8')).toBe(expected)
    expected += await resumeIn(other(first), at, source.id, next + 1)
  } else {
    // Only the stopped holder's death releases the lock; the other runtime then migrates.
    await paused.crash()
    await expectTree(at.root, source.id, [LEASE_FILENAME, seeded.name])
    expected = migrated + await resumeIn(other(first), at, source.id, next)
    expected += await resumeIn(first, at, source.id, next + 1)
  }
  expect(await readFile(plainLog(at.root, source.id), 'utf8')).toBe(expected)
  expect(await readSeqs(at.root, source.id)).toEqual(seqsTo(next + 1))
  expect(sha256(await readFile(join(sessionDir(at.root, source.id), seeded.name)))).toBe(seeded.hash)
  await expectTree(at.root, source.id, [LEASE_FILENAME, seeded.name, 'session.v3.jsonl'])
}

/**
 * A `killed` writer of the released v2 source dies at `step`: tearing its
 * migration's temporary file after `bytes` bytes, or after linking the
 * migrated log into place (POSIX only). The other runtime is refused while
 * it lives, then migrates or resumes, and `killed`'s runtime resumes after
 * it. The temporary file stays, never a generation.
 */
async function killedMigration(killed: Runtime, step: 'tear' | 'unlink'): Promise<void> {
  const source = releasedSource('v2-clean-turn')
  const { migrated, next } = await oracleMigration(source)
  const at = await area()
  const seeded = await seedSource(at.root, source.id, source.version, source.text)
  const bytes = 50
  let writer: Child
  if (step === 'unlink') {
    writer = await pausedMigration(killed, at, source.id, 'unlink', next)
  } else if (killed === 'Rust') {
    writer = await rustTear(at, ['tear-append', at.root, source.id, String(next), String(bytes)])
  } else {
    writer = new Child('node migrator', process.execPath, [MIGRATOR, at.root, source.id, 'tear', String(bytes)], at, 'pipe')
    expect(await writer.nextJson(), writer.describe()).toEqual({ state: 'torn' })
    expect(writer.rest(), writer.describe()).toEqual({ stdout: '', stderr: '' })
  }
  const published = step === 'unlink' ? ['session.v3.jsonl'] : []
  await expectTree(at.root, source.id, [LEASE_FILENAME, seeded.name, MIGRATION_TEMPORARY, ...published])
  const temporary = (await readdir(sessionDir(at.root, source.id))).find(name => MIGRATION_TEMPORARY.test(name))!
  const temporaryPath = join(sessionDir(at.root, source.id), temporary)
  expect(await readFile(temporaryPath, 'utf8')).toBe(step === 'unlink' ? migrated : migrated.slice(0, bytes))
  if (step === 'unlink') expect(await readFile(plainLog(at.root, source.id), 'utf8')).toBe(migrated)
  await expectOwned(other(killed), at, source.id)
  await writer.crash()
  let expected = migrated + await resumeIn(other(killed), at, source.id, next)
  expected += await resumeIn(killed, at, source.id, next + 1)
  expect(await readFile(plainLog(at.root, source.id), 'utf8')).toBe(expected)
  expect(await readSeqs(at.root, source.id)).toEqual(seqsTo(next + 1))
  // A torn temporary file is never read; a linked one is the log's second name.
  expect(await readFile(temporaryPath, 'utf8')).toBe(step === 'unlink' ? expected : migrated.slice(0, bytes))
  expect(sha256(await readFile(join(sessionDir(at.root, source.id), seeded.name)))).toBe(seeded.hash)
  await expectTree(at.root, source.id, [LEASE_FILENAME, seeded.name, 'session.v3.jsonl', temporary])
}

const suite = PROBE === undefined ? describe.skip : describe
const title = PROBE === undefined
  ? 'cross-runtime resume (opt-in skipped: run `bun run test:rust:lease`, or set DSH_RUST_LEASE_PROBE to the built probe)'
  : 'cross-runtime resume'

suite(title, () => {
  beforeAll(async () => {
    const probe = PROBE!
    if (!isAbsolute(probe)) throw new Error(`the lease probe path must be an absolute path, got ${JSON.stringify(probe)}`)
    const info = await stat(probe).catch((error: unknown) => {
      throw new Error(`the lease probe path names no file: ${probe} (${String(error)}); build it with cargo build -p bake-conformance in rust/`)
    })
    if (!info.isFile()) throw new Error(`the lease probe path is not a file: ${probe}`)
    if (!existsSync(BUILT_LIB)) throw new Error(`the Node writer needs the built package at ${BUILT_LIB}; run bun run build:runtime`)
  })

  it('a TypeScript writer killed mid-append leaves a torn tail that Rust truncates and resumes, to the bytes TypeScript reads back', async () => {
    const at = await area()
    const id = 'xrt-ts-torn'
    // Past `"time":`, where the torn row and the row Rust appends first differ.
    const tear = 40
    const { writer, committed, content } = await tsTear(at, id, 'none', tear)
    expect(committed.toString()).toBe(HEADER.replace('<id>', id) + rows(...FIRST_TURN))
    expect(content.toString()).toBe(rows(turnStart(2, 7, 2), turnEnd(3, 8, 2)))
    expect(content.subarray(0, tear).toString()).not.toBe(probeRow(2).slice(0, tear))
    // The killed writer's lock held until its death.
    await expectRustOwned(at, id)
    await writer.crash()
    expect(await readSeqs(at.root, id)).toEqual([0, 1])
    await rustResume(at, id, 2)
    expect(await readFile(plainLog(at.root, id), 'utf8')).toBe(committed.toString() + probeRow(2))
    await tsResume(at.root, id, [turnEnd(3, 5, 2)])
    expect(await readFile(plainLog(at.root, id), 'utf8')).toBe(committed.toString() + probeRow(2) + rows(turnEnd(3, 5, 2)))
    expect(await readSeqs(at.root, id)).toEqual([0, 1, 2, 3])
    await expectTree(at.root, id, [LEASE_FILENAME, 'session.v3.jsonl'])
  })

  it('a Zstd log a killed TypeScript writer left torn inside a frame is resumed by Rust, which truncates the frame and appends its own, to the bytes TypeScript reads back', async () => {
    const at = await area()
    const id = 'xrt-ts-zstd'
    const tear = 10
    const { writer, committed, content } = await tsTear(at, id, 'zstd', tear)
    expect(committed).toEqual(Buffer.concat([await frame(HEADER.replace('<id>', id)), await frame(rows(...FIRST_TURN))]))
    expect(content).toEqual(await frame(rows(turnStart(2, 7, 2), turnEnd(3, 8, 2))))
    await expectRustZstdOwned(at, id)
    await writer.crash()
    // Ten bytes of a frame recover no row.
    expect(await readSeqs(at.root, id, 'zstd')).toEqual([0, 1])
    await rustZstdResume(at, id, 2)
    expect(await readFile(zstdLog(at.root, id))).toEqual(Buffer.concat([committed, await frame(probeRow(2))]))
    await tsResume(at.root, id, [turnEnd(3, 5, 2)], 'zstd')
    const resumed = Buffer.concat([committed, await frame(probeRow(2)), await frame(rows(turnEnd(3, 5, 2)))])
    expect(await readFile(zstdLog(at.root, id))).toEqual(resumed)
    expect(await readSeqs(at.root, id, 'zstd')).toEqual([0, 1, 2, 3])
    await expectTree(at.root, id, [LEASE_FILENAME, 'session.v3.jsonl.zstd'])
  })

  it('a torn TypeScript Zstd frame whose rows are complete is recovered by Rust, which rewrites them as one frame before its own, to the bytes TypeScript reads back', async () => {
    const at = await area()
    const id = 'xrt-ts-zstd-rows'
    const torn = await frame(rows(turnStart(2, 7, 2), turnEnd(3, 8, 2)))
    // Every byte but the checksum's last two: the frame's rows decode.
    const tear = torn.length - 2
    const { writer, committed, content } = await tsTear(at, id, 'zstd', tear)
    expect(content).toEqual(torn)
    await writer.crash()
    expect(await readSeqs(at.root, id, 'zstd')).toEqual([0, 1, 2, 3])
    await rustZstdResume(at, id, 4)
    const resumed = Buffer.concat([committed, torn, await frame(probeRow(4))])
    expect(await readFile(zstdLog(at.root, id))).toEqual(resumed)
    await tsResume(at.root, id, [turnEnd(5, 5, 3)], 'zstd')
    expect(await readFile(zstdLog(at.root, id))).toEqual(Buffer.concat([resumed, await frame(rows(turnEnd(5, 5, 3)))]))
    expect(await readSeqs(at.root, id, 'zstd')).toEqual([0, 1, 2, 3, 4, 5])
    await expectTree(at.root, id, [LEASE_FILENAME, 'session.v3.jsonl.zstd'])
  })

  it('a Rust Zstd writer killed mid-append leaves a torn frame whose rows TypeScript recovers and resumes, to the bytes Rust resumes after', async () => {
    const at = await area()
    const id = 'xrt-rust-zstd'
    await rustZstdCreate(at, id)
    const committed = await readFile(zstdLog(at.root, id))
    expect(committed).toEqual(Buffer.concat([await frame(HEADER.replace('<id>', id)), await frame(rows(...FIRST_TURN))]))
    const torn = await frame(rows(turnStart(2, 3, 2), turnEnd(3, 4, 2)))
    const tear = torn.length - 2
    const writer = await rustTear(at, ['zstd-tear-append', at.root, id, '2', String(tear)])
    expect(await readFile(zstdLog(at.root, id))).toEqual(Buffer.concat([committed, torn.subarray(0, tear)]))
    const before = await snapshot(at.root)
    expect(await tsOpenError(at.root, id, 'zstd')).toBeInstanceOf(SessionAlreadyOwnedError)
    expect(await snapshot(at.root)).toEqual(before)
    await writer.crash()
    expect(await readSeqs(at.root, id, 'zstd')).toEqual([0, 1, 2, 3])
    await tsResume(at.root, id, [turnStart(4, 5, 3)], 'zstd')
    const resumed = Buffer.concat([committed, torn, await frame(rows(turnStart(4, 5, 3)))])
    expect(await readFile(zstdLog(at.root, id))).toEqual(resumed)
    await rustZstdResume(at, id, 5)
    expect(await readFile(zstdLog(at.root, id))).toEqual(Buffer.concat([resumed, await frame(probeRow(5))]))
    expect(await readSeqs(at.root, id, 'zstd')).toEqual([0, 1, 2, 3, 4, 5])
    await expectTree(at.root, id, [LEASE_FILENAME, 'session.v3.jsonl.zstd'])
  })

  it('a Rust writer killed mid-append leaves a torn tail that TypeScript truncates and resumes, to the bytes Rust resumes after', async () => {
    const at = await area()
    const id = 'xrt-rust-torn'
    await rustCreate(at, id)
    const committed = await readFile(plainLog(at.root, id), 'utf8')
    expect(committed).toBe(HEADER.replace('<id>', id) + rows(...FIRST_TURN))
    const torn = rows(turnStart(2, 3, 2))
    const tear = Buffer.byteLength(torn) + 5
    const writer = await rustTear(at, ['tear-append', at.root, id, '2', String(tear)])
    expect(await readFile(plainLog(at.root, id), 'utf8')).toBe(committed + torn + rows(turnEnd(3, 4, 2)).slice(0, 5))
    await expectTsOwned(at.root, id)
    await writer.crash()
    // The complete row is a committed read; only the fragment is dropped.
    expect(await readSeqs(at.root, id)).toEqual([0, 1, 2])
    await tsResume(at.root, id, [turnEnd(3, 5, 2)])
    expect(await readFile(plainLog(at.root, id), 'utf8')).toBe(committed + torn + rows(turnEnd(3, 5, 2)))
    await rustResume(at, id, 4)
    expect(await readFile(plainLog(at.root, id), 'utf8')).toBe(committed + torn + rows(turnEnd(3, 5, 2)) + probeRow(4))
    expect(await readSeqs(at.root, id)).toEqual([0, 1, 2, 3, 4])
    await expectTree(at.root, id, [LEASE_FILENAME, 'session.v3.jsonl'])
  })

  it('a Rust writer killed while publishing a new Session leaves only its temporary file, beside which TypeScript creates the Session and Rust resumes it', async () => {
    const at = await area()
    const id = 'xrt-rust-publish'
    const writer = await rustTear(at, ['tear-create', at.root, id, '30'])
    const staged = /^session\.v3\.jsonl\.[^.]+\.tmp$/u
    await expectTree(at.root, id, [LEASE_FILENAME, staged])
    const stagedName = (await readdir(sessionDir(at.root, id))).find(name => staged.test(name))!
    const stagedBytes = await readFile(join(sessionDir(at.root, id), stagedName))
    expect(stagedBytes.toString()).toBe(HEADER.replace('<id>', id).slice(0, 30))
    await writer.crash()
    expect(await tsOpenError(at.root, id)).toBeInstanceOf(SessionPersistenceNotFoundError)
    await withBackend(at.root, 'none', async (persistence) => {
      const handle = await persistence.create({ version: SESSION_FORMAT_VERSION, id: SessionId(id), createdAt: 1000, isSeeded: false })
      try {
        await handle.append(FIRST_TURN)
        await handle.flush()
      } finally {
        await handle.close()
      }
    })
    await rustResume(at, id, 2)
    expect(await readFile(plainLog(at.root, id), 'utf8')).toBe(HEADER.replace('<id>', id) + rows(...FIRST_TURN) + probeRow(2))
    expect(await readSeqs(at.root, id)).toEqual([0, 1, 2])
    // The temporary file is the one Rust leaves by design, unchanged and never a generation.
    expect(await readFile(join(sessionDir(at.root, id), stagedName))).toEqual(stagedBytes)
    await expectTree(at.root, id, [LEASE_FILENAME, 'session.v3.jsonl', stagedName])
  })

  it('a v0 log migrated by Rust or by TypeScript resumes in the other runtime to the same bytes, its source unchanged', () => migratedResume('v0-clean-turn'))

  it('a v1 log migrated by Rust or by TypeScript resumes in the other runtime to the same bytes, its source unchanged', () => migratedResume('v1-packed-run-cited'))

  it('a v2 log migrated by Rust or by TypeScript resumes in the other runtime to the same bytes, its source unchanged', () => migratedResume('v2-clean-turn'))
  it('the runtimes migrate one v0 log concurrently: a Rust migration paused before publication holds the lock, TypeScript is refused, and both resume the one target TypeScript\'s migration writes, its source unchanged', () => concurrentMigration('v0-clean-turn', 'Rust'))

  it('the runtimes migrate one v0 log concurrently: a TypeScript migration paused before publication holds the lock, Rust is refused, and both resume the one target TypeScript\'s migration writes, its source unchanged', () => concurrentMigration('v0-clean-turn', 'TypeScript'))

  it('the runtimes migrate one v1 log concurrently: a Rust migration paused before publication holds the lock, TypeScript is refused, and both resume the one target TypeScript\'s migration writes, its source unchanged', () => concurrentMigration('v1-packed-run-cited', 'Rust'))

  it('the runtimes migrate one v1 log concurrently: a TypeScript migration paused before publication holds the lock, Rust is refused, and both resume the one target TypeScript\'s migration writes, its source unchanged', () => concurrentMigration('v1-packed-run-cited', 'TypeScript'))

  it('the runtimes migrate one v2 log concurrently: a Rust migration paused before publication holds the lock, TypeScript is refused, and both resume the one target TypeScript\'s migration writes, its source unchanged', () => concurrentMigration('v2-clean-turn', 'Rust'))

  it('the runtimes migrate one v2 log concurrently: a TypeScript migration paused before publication holds the lock, Rust is refused, and both resume the one target TypeScript\'s migration writes, its source unchanged', () => concurrentMigration('v2-clean-turn', 'TypeScript'))

  it('a Rust writer killed while writing its migration temporary file leaves only that file, beside which TypeScript migrates and Rust resumes', () => killedMigration('Rust', 'tear'))

  it('a TypeScript writer killed while writing its migration temporary file leaves only that file, beside which Rust migrates and TypeScript resumes', () => killedMigration('TypeScript', 'tear'))

  // Windows publishes a migration with a move, which leaves no temporary file to remove.
  it.skipIf(!POSIX)('a Rust writer killed after linking its migrated log leaves the temporary file as its second link, and TypeScript then Rust resume the log (POSIX only)', () => killedMigration('Rust', 'unlink'))

  it.skipIf(!POSIX)('a TypeScript writer killed after linking its migrated log leaves the temporary file as its second link, and Rust then TypeScript resume the log (POSIX only)', () => killedMigration('TypeScript', 'unlink'))

  // Windows has no SIGSTOP.
  it.skipIf(!POSIX)('a stopped Rust migration still refuses TypeScript, and only its death lets TypeScript migrate (POSIX only)', () => concurrentMigration('v2-clean-turn', 'Rust', 'SIGSTOP'))

  it.skipIf(!POSIX)('a stopped TypeScript migration still refuses Rust, and only its death lets Rust migrate (POSIX only)', () => concurrentMigration('v2-clean-turn', 'TypeScript', 'SIGSTOP'))

  // Windows directory permissions are ACLs, which a mode does not set.
  it.skipIf(!POSIX || ROOT)('a read-only Session directory refuses both runtimes\' migrations with a permission error and leaves the tree unchanged (POSIX only)', async () => {
    const source = releasedSource('v2-clean-turn')
    const at = await area()
    const seeded = await seedSource(at.root, source.id, source.version, source.text)
    // The lock file exists, so the lock is taken and the migration's temporary file is refused.
    await writeFile(join(sessionDir(at.root, source.id), LEASE_FILENAME), '')
    const dir = sessionDir(at.root, source.id)
    await chmod(dir, 0o500)
    try {
      const before = await snapshot(at.root)
      const refusal = await tsOpenError(at.root, source.id)
      expect((refusal as NodeJS.ErrnoException).code, String(refusal)).toBe('EACCES')
      expect(await snapshot(at.root)).toEqual(before)
      const { exit, output, child } = await runProbe(at, ['open', at.root, source.id, '9'])
      expect(exit, child.describe()).toEqual({ code: 1, signal: null })
      expect(output).toEqual({ outcome: 'io', kind: 'PermissionDenied' })
      expect(await snapshot(at.root)).toEqual(before)
      await expectTree(at.root, source.id, [LEASE_FILENAME, seeded.name])
    } finally {
      await chmod(dir, 0o700)
    }
    const { migrated, next } = await oracleMigration(source)
    await rustResume(at, source.id, next)
    expect(await readFile(plainLog(at.root, source.id), 'utf8')).toBe(migrated + probeRow(next))
    await expectTree(at.root, source.id, [LEASE_FILENAME, seeded.name, 'session.v3.jsonl'])
  })

  it('a Zstd v0 log migrated by Rust or by TypeScript resumes in the other runtime to the same frames, its source unchanged', () => migratedZstdResume('v0-clean-turn'))

  it('a Zstd v2 log migrated by Rust or by TypeScript resumes in the other runtime to the same frames, its source unchanged', () => migratedZstdResume('v2-clean-turn'))
})
