/**
 * Cross-runtime resume between the TypeScript JSONL backend and the Rust
 * `bake-session` writer, over one shared root and real independent
 * processes (D31): TypeScript writes and Rust resumes, Rust writes and
 * TypeScript resumes, each after the writer was killed mid-append, and an
 * older generation migrated by either runtime and resumed by the other. The
 * Rust side is the development-only `bake-session-lease-probe` binary from
 * `rust/crates/bake-conformance`; the TypeScript side is the production
 * backend in this process and, for a writer killed mid-append, the built
 * package running in `fixtures/resume-cross-runtime-writer.mjs`.
 *
 * Expected bytes are TypeScript's: each event row is `JSON.stringify` of the
 * event and an LF, as the backend writes it, and a migrated log is the one
 * TypeScript's own migration writes from the same source in another root.
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
import { mkdir, mkdtemp, readdir, readFile, rm, stat, writeFile } from 'node:fs/promises'
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

/** The probe path, or `undefined` when the suite is not opted in. */
const PROBE = process.env.BAKE_RUST_LEASE_PROBE ?? process.env.DSH_RUST_LEASE_PROBE
const WRITER = fileURLToPath(new URL('./fixtures/resume-cross-runtime-writer.mjs', import.meta.url))
const BUILT_LIB = fileURLToPath(new URL('../lib/index.js', import.meta.url))
const MIGRATED_CASES = fileURLToPath(new URL('../../../../conformance/session/migrated-restore-cases.json', import.meta.url))
const POSIX = process.platform !== 'win32'

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

  it('a Zstd log a killed TypeScript writer left torn is refused by Rust with TypeScript\'s own message and kept byte for byte, and TypeScript resumes it', async () => {
    const at = await area()
    const id = 'xrt-ts-zstd'
    const tear = 10
    const { writer, committed } = await tsTear(at, id, 'zstd', tear)
    await writer.crash()
    const before = await snapshot(at.root)
    // TypeScript configured as Rust is, for compression none, is the oracle.
    const refusal = await tsOpenError(at.root, id, 'none')
    expect(refusal).toBeInstanceOf(Error)
    expect(await snapshot(at.root)).toEqual(before)
    const { exit, output, child } = await runProbe(at, ['open', at.root, id, '2'])
    expect(exit, child.describe()).toEqual({ code: 1, signal: null })
    expect(output).toEqual({ outcome: 'refused', message: (refusal as Error).message })
    expect(await snapshot(at.root)).toEqual(before)
    expect(await readSeqs(at.root, id, 'zstd')).toEqual([0, 1])
    await tsResume(at.root, id, [turnStart(2, 5, 2), turnEnd(3, 6, 2)], 'zstd')
    const final = await readFile(`${plainLog(at.root, id)}.zstd`)
    expect(final.subarray(0, committed.length)).toEqual(committed)
    expect(await readSeqs(at.root, id, 'zstd')).toEqual([0, 1, 2, 3])
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
})
