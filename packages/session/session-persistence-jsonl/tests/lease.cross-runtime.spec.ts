/**
 * Cross-runtime write-lease exclusion between the TypeScript JSONL backend
 * and the Rust `bake-session` writer, over one shared root and real
 * independent processes. The Rust side is the development-only
 * `bake-session-lease-probe` binary from `rust/crates/bake-conformance`; the
 * Node side is either the production backend inside this process or the
 * built package running in `fixtures/lease-cross-runtime-holder.mjs`.
 *
 * Each case proves a refusal before it proves a takeover, so a takeover cannot
 * pass because nothing was ever held. Every refused write-open leaves the
 * Session directory byte-identical apart from the lock file itself (POSIX
 * opens it for writing, which truncates it), and every final log is checked
 * through a fresh TypeScript backend.
 *
 * Opt-in: run `bun run test:rust:lease`, which passes the built probe. A
 * direct Vitest run sets `DSH_RUST_LEASE_PROBE` to the probe's absolute path:
 * the runtime suite's setup deletes every `BAKE_*` name from each worker, so
 * `BAKE_RUST_LEASE_PROBE` alone never reaches this file, though it is read
 * first when present. With neither name the suite is skipped by name; a name
 * that does not point at a usable probe fails the file. The Node holder needs
 * the built package (`bun run build:runtime`) and fails when it is missing.
 *
 * The two stopped-holder cases need POSIX job-control signals and are skipped on
 * Windows only, where the live idle holder cases carry the evidence.
 */

import { spawn, type ChildProcess } from 'node:child_process'
import { createHash } from 'node:crypto'
import { existsSync } from 'node:fs'
import { mkdir, mkdtemp, readdir, readFile, rm, stat } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { isAbsolute, join, relative } from 'node:path'
import { fileURLToPath } from 'node:url'
import { afterAll, afterEach, beforeAll, describe, expect, it } from 'vitest'
import { Context } from '@deepseek-ai/cordis'
import { SESSION_FORMAT_VERSION, SessionId, SessionSeq } from 'bake-session'
import { SessionAlreadyOwnedError } from 'bake-session-persistence'
import type { SessionPersistence } from 'bake-session-persistence'
import JsonlSessionPersistence from 'bake-session-persistence-jsonl'
import { LEASE_FILENAME } from '../src/lease.ts'

/** The probe path, or `undefined` when the suite is not opted in. */
const PROBE = process.env.BAKE_RUST_LEASE_PROBE ?? process.env.DSH_RUST_LEASE_PROBE
const HOLDER = fileURLToPath(new URL('./fixtures/lease-cross-runtime-holder.mjs', import.meta.url))
const BUILT_LIB = fileURLToPath(new URL('../lib/index.js', import.meta.url))
const POSIX = process.platform !== 'win32'

/** Bound on every single wait for a child's output or exit. */
const WAIT_MS = 20_000
/** Bound on captured stdout and stderr per child; more is a protocol error. */
const MAX_OUTPUT = 64 * 1024

interface Exit {
  readonly code: number | null
  readonly signal: NodeJS.Signals | null
}

/**
 * One owned child process: bounded stdout/stderr capture, line reads with a
 * deadline, and a `closed` promise that settles only after the process has
 * exited and its stdio has closed, including when the spawn itself fails.
 */
class Child {
  readonly proc: ChildProcess
  readonly closed: Promise<Exit>
  exit: Exit | undefined
  spawnError: Error | undefined
  private stdout = ''
  private stderr = ''
  private consumed = 0
  private stdoutOverflow = false
  private stderrOverflow = false
  private readonly watchers = new Set<() => void>()

  constructor(readonly label: string, command: string, args: readonly string[], env: NodeJS.ProcessEnv, cwd: string, stdin: 'pipe' | 'ignore') {
    this.proc = spawn(command, args, { cwd, env, stdio: [stdin, 'pipe', 'pipe'], windowsHide: true })
    // Registered before any await, so a failing case still reaps it.
    children.add(this)
    this.closed = new Promise<Exit>((resolve) => {
      this.proc.once('close', (code, signal) => {
        this.exit = { code, signal }
        this.notify()
        resolve(this.exit)
      })
    })
    this.proc.once('error', (error) => {
      this.spawnError ??= error
      this.notify()
    })
    // A write to a pipe whose reader died must not become an unhandled error.
    this.proc.stdin?.on('error', () => {})
    this.proc.stdout!.setEncoding('utf8')
    this.proc.stderr!.setEncoding('utf8')
    this.proc.stdout!.on('data', (chunk: string) => {
      if (this.stdout.length + chunk.length > MAX_OUTPUT) this.stdoutOverflow = true
      else this.stdout += chunk
      this.notify()
    })
    this.proc.stderr!.on('data', (chunk: string) => {
      if (this.stderr.length + chunk.length > MAX_OUTPUT) this.stderrOverflow = true
      else this.stderr += chunk
      this.notify()
    })
  }

  /** Diagnostics for a failed expectation. */
  describe(): string {
    return `${this.label} (pid ${this.proc.pid}): exit ${JSON.stringify(this.exit)}, spawn error ${String(this.spawnError)}, stdout ${JSON.stringify(this.stdout)}, stderr ${JSON.stringify(this.stderr)}`
  }

  /** Output past {@link MAX_OUTPUT} on either stream is a protocol failure, never silently truncated. */
  private assertBounded(): void {
    if (this.stdoutOverflow) throw new Error(`stdout exceeded ${MAX_OUTPUT} bytes: ${this.describe()}`)
    if (this.stderrOverflow) throw new Error(`stderr exceeded ${MAX_OUTPUT} bytes: ${this.describe()}`)
  }

  private notify(): void {
    for (const watcher of [...this.watchers]) watcher()
  }

  /**
   * Resolve with `check`'s first defined result, re-checked on every output or exit, within {@link WAIT_MS}.
   * Output overflow rejects unless `bounded` is false, which only teardown uses so it still reaches close.
   */
  private waitFor<T>(what: string, check: () => T | undefined, bounded = true): Promise<T> {
    return new Promise<T>((resolve, reject) => {
      const done = (): void => {
        clearTimeout(timer)
        this.watchers.delete(poke)
      }
      const poke = (): void => {
        let value: T | undefined
        try {
          if (bounded) this.assertBounded()
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

  /** The next complete stdout line; rejects on overflow or when the child closes first. */
  nextLine(): Promise<string> {
    return this.waitFor('a stdout line', () => {
      const end = this.stdout.indexOf('\n', this.consumed)
      if (end >= 0) {
        const line = this.stdout.slice(this.consumed, end)
        this.consumed = end + 1
        return line
      }
      if (this.exit !== undefined) throw new Error(`closed before a complete stdout line: ${this.describe()}`)
      return undefined
    })
  }

  /** Stdout not yet returned by {@link nextLine}. */
  unread(): string {
    this.assertBounded()
    return this.stdout.slice(this.consumed)
  }

  /** Stderr captured so far; throws when either stream overflowed. */
  stderrText(): string {
    this.assertBounded()
    return this.stderr
  }

  /** The exit, once the process and its stdio have closed, within {@link WAIT_MS}. */
  waitClosed(): Promise<Exit> {
    return this.waitFor('exit', () => this.exit)
  }

  /** Teardown: SIGKILL (TerminateProcess on Windows) when still running, then await close regardless of output. */
  async kill(): Promise<Exit> {
    if (this.exit === undefined && this.spawnError === undefined) this.proc.kill('SIGKILL')
    return this.waitFor('exit after kill', () => this.exit, false)
  }

  /** Write one protocol line to stdin. */
  send(line: string): void {
    this.proc.stdin!.write(`${line}\n`)
  }
}

const children = new Set<Child>()
const contexts = new Set<Context>()
const dirs: string[] = []

afterEach(async () => {
  // Children first: on Windows their open handles keep the directories busy.
  const live = [...children]
  children.clear()
  const exits = await Promise.allSettled(live.map(child => child.kill()))
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
  const base = await mkdtemp(join(tmpdir(), 'bake-lease-xrt-'))
  dirs.push(base)
  const root = join(base, 'sessions')
  const home = join(base, 'home')
  const temp = join(base, 'tmp')
  await Promise.all([mkdir(root), mkdir(home), mkdir(temp)])
  // A minimal environment: no model credentials, private homes, no inherited BAKE_/DSH_ names.
  return { root, env: childEnv(home, temp), cwd: home }
}

/**
 * A minimal child environment: the search path and the Windows system names a
 * process needs to start, private home and temp directories, and nothing else
 * inherited, so no model credentials or ambient BAKE_/DSH_ settings.
 */
function childEnv(home: string, temp: string): NodeJS.ProcessEnv {
  const env: NodeJS.ProcessEnv = { HOME: home, USERPROFILE: home, BAKE_HOME: home, DSH_HOME: home, TMPDIR: temp, TMP: temp, TEMP: temp }
  for (const name of ['PATH', 'SystemRoot', 'SystemDrive', 'windir', 'PATHEXT', 'ComSpec']) {
    const value = process.env[name]
    if (value !== undefined) env[name] = value
  }
  return env
}

/** Run `use` against a fresh, independent backend, disposed afterwards. */
async function withBackend<T>(root: string, use: (persistence: SessionPersistence) => Promise<T>): Promise<T> {
  const ctx = new Context()
  contexts.add(ctx)
  try {
    await ctx.plugin(JsonlSessionPersistence, { root, compression: 'none' })
    return await use(ctx.sessionPersistence)
  } finally {
    contexts.delete(ctx)
    await ctx.fiber.dispose()
  }
}

/** The production `SessionAlreadyOwnedError` message for `id`. */
function ownedMessage(id: string): string {
  return new SessionAlreadyOwnedError(SessionId(id)).message
}

const FIRST_TURN = [
  { type: 'turn/start', seq: SessionSeq(0), time: 1, data: { turn: 1 } },
  { type: 'turn/end', seq: SessionSeq(1), time: 2, data: { turn: 1, reason: { kind: 'completed' } } },
] as const
const NEXT_TURN = [{ type: 'turn/start', seq: SessionSeq(2), time: 3, data: { turn: 2 } }] as const

/** Every file under `root` but lock files, by relative path, as SHA-256 of its bytes; directories as `dir`. */
async function snapshot(root: string): Promise<Record<string, string>> {
  const entries: Record<string, string> = {}
  const walk = async (dir: string): Promise<void> => {
    for (const entry of await readdir(dir, { withFileTypes: true })) {
      const path = join(dir, entry.name)
      const key = relative(root, path).split('\\').join('/')
      if (entry.isDirectory()) {
        entries[key] = 'dir'
        await walk(path)
      } else if (entry.name !== LEASE_FILENAME) {
        entries[key] = createHash('sha256').update(await readFile(path)).digest('hex')
      }
    }
  }
  await walk(root)
  return entries
}

/** Header id/cwd and event seqs, read through a fresh backend's read handle. */
async function readLog(root: string, id: string): Promise<{ id: string; cwd: string | undefined; seqs: number[] }> {
  return withBackend(root, async (persistence) => {
    const reader = await persistence.open(SessionId(id), 'read')
    try {
      const { events } = await reader.read()
      return { id: reader.header.id, cwd: reader.header.cwd, seqs: events.map(event => event.seq) }
    } finally {
      await reader.close()
    }
  })
}

async function expectLog(root: string, id: string, seqs: number[]): Promise<void> {
  expect(await readLog(root, id)).toEqual({ id, cwd: undefined, seqs })
}

/** Create the Session through the TypeScript backend, with no cwd and the first turn, then close it. */
async function createTsLog(root: string, id: string): Promise<void> {
  await withBackend(root, async (persistence) => {
    const handle = await persistence.create({ version: SESSION_FORMAT_VERSION, id: SessionId(id), createdAt: 1000, isSeeded: false })
    try {
      await handle.append(FIRST_TURN)
      await handle.flush()
    } finally {
      await handle.close()
    }
  })
}

/** A TypeScript write-open must be refused as owned, leaving every non-lock byte unchanged. */
async function expectTsRefused(root: string, id: string): Promise<void> {
  const before = await snapshot(root)
  const outcome = await withBackend(root, async (persistence) => {
    try {
      const handle = await persistence.open(SessionId(id), 'write')
      await handle.close()
      return undefined
    } catch (error) {
      return error
    }
  })
  expect(outcome).toBeInstanceOf(SessionAlreadyOwnedError)
  expect((outcome as Error).message).toBe(ownedMessage(id))
  expect(await snapshot(root)).toEqual(before)
}

/** Take write ownership through a fresh TypeScript backend and append seq 2. */
async function tsTakeOver(root: string, id: string): Promise<void> {
  await withBackend(root, async (persistence) => {
    const handle = await persistence.open(SessionId(id), 'write')
    try {
      await handle.append(NEXT_TURN)
      await handle.flush()
    } finally {
      await handle.close()
    }
  })
  await expectLog(root, id, [0, 1, 2])
}

/** Run the probe to completion and parse its single stdout line. */
async function runProbe(at: Area, args: readonly string[]): Promise<{ exit: Exit; output: unknown; child: Child }> {
  const child = new Child(`probe ${args[0] ?? ''}`, PROBE!, args, at.env, at.cwd, 'ignore')
  const exit = await child.waitClosed()
  const line = await child.nextLine()
  expect(child.unread(), child.describe()).toBe('')
  let output: unknown
  try {
    output = JSON.parse(line)
  } catch {
    throw new Error(`probe stdout is not JSON: ${child.describe()}`)
  }
  return { exit, output, child }
}

/** A Rust write-open must exit 3 with the exact owned message, leaving every non-lock byte unchanged. */
async function expectRustRefused(at: Area, id: string): Promise<void> {
  const before = await snapshot(at.root)
  const { exit, output, child } = await runProbe(at, ['open', at.root, id, '2'])
  expect(exit, child.describe()).toEqual({ code: 3, signal: null })
  expect(output).toEqual({ outcome: 'owned', message: ownedMessage(id) })
  expect(child.stderrText(), child.describe()).toBe('')
  expect(await snapshot(at.root)).toEqual(before)
}

/** Take write ownership through the Rust probe and append seq 2. */
async function rustTakeOver(at: Area, id: string): Promise<void> {
  const { exit, output, child } = await runProbe(at, ['open', at.root, id, '2'])
  expect(exit, child.describe()).toEqual({ code: 0, signal: null })
  expect(output).toEqual({ outcome: 'opened', seqs: [0, 1, 2] })
  expect(child.stderrText(), child.describe()).toBe('')
  await expectLog(at.root, id, [0, 1, 2])
}

type HolderKind = 'rust hold' | 'rust hold-open' | 'node'

/** Start a holder and wait for its exact `holding` line. */
async function startHolder(at: Area, kind: HolderKind, id: string): Promise<Child> {
  const child = kind === 'node'
    ? new Child('node holder', process.execPath, [HOLDER, at.root, id], at.env, at.cwd, 'pipe')
    : new Child(kind, PROBE!, [kind === 'rust hold' ? 'hold' : 'hold-open', at.root, id], at.env, at.cwd, 'pipe')
  const line = await child.nextLine()
  expect(line, child.describe()).toBe('{"state":"holding"}')
  expect(child.stderrText(), child.describe()).toBe('')
  return child
}

/** Release a live holder gracefully and require its exact `released` line and a clean exit. */
async function release(holder: Child): Promise<void> {
  expect(holder.exit, holder.describe()).toBeUndefined()
  holder.send('release')
  expect(await holder.nextLine(), holder.describe()).toBe('{"state":"released"}')
  expect(await holder.waitClosed(), holder.describe()).toEqual({ code: 0, signal: null })
  expect(holder.unread(), holder.describe()).toBe('')
  expect(holder.stderrText(), holder.describe()).toBe('')
}

/**
 * Crash a live holder: no release code runs, and the close is awaited. The
 * kill must target a process that has neither closed nor exited, and the
 * observed exit must be that kill, not the holder dying on its own.
 */
async function crash(holder: Child): Promise<void> {
  expect(holder.unread(), holder.describe()).toBe('')
  expect(holder.stderrText(), holder.describe()).toBe('')
  expect(holder.exit, holder.describe()).toBeUndefined()
  expect(holder.spawnError, holder.describe()).toBeUndefined()
  expect(holder.proc.exitCode, holder.describe()).toBeNull()
  expect(holder.proc.signalCode, holder.describe()).toBeNull()
  expect(holder.proc.kill('SIGKILL'), holder.describe()).toBe(true)
  const exit = await holder.waitClosed()
  if (POSIX) {
    expect(exit, holder.describe()).toEqual({ code: null, signal: 'SIGKILL' })
  } else {
    // Windows has no signals: Node's kill('SIGKILL') is TerminateProcess, and
    // libuv reports the requested signal name on the exit of a process it
    // terminated, so a holder that exited by itself reports no signal.
    expect(exit.signal, holder.describe()).toBe('SIGKILL')
  }
}

/** Wait until `ps` reports `pid` stopped (state `T`), polling until the bound. */
async function observeStopped(pid: number, at: Area): Promise<void> {
  const deadline = Date.now() + WAIT_MS
  let last = ''
  for (;;) {
    const ps = new Child('ps', 'ps', ['-o', 'stat=', '-p', String(pid)], at.env, at.cwd, 'ignore')
    const exit = await ps.waitClosed()
    last = ps.describe()
    children.delete(ps)
    if (exit.code === 0) {
      const state = (await ps.nextLine()).trim()
      if (state.startsWith('T')) return
    }
    if (Date.now() > deadline) throw new Error(`process ${pid} was not observed stopped within ${WAIT_MS} ms; last ${last}`)
    await new Promise(resolve => setTimeout(resolve, 10))
  }
}

const suite = PROBE === undefined ? describe.skip : describe
const title = PROBE === undefined
  ? 'cross-runtime write lease (opt-in skipped: run `bun run test:rust:lease`, or set DSH_RUST_LEASE_PROBE to the built probe)'
  : 'cross-runtime write lease'

suite(title, () => {
  let checkDir: string | undefined

  beforeAll(async () => {
    const probe = PROBE!
    if (!isAbsolute(probe)) throw new Error(`the lease probe path must be an absolute path, got ${JSON.stringify(probe)}`)
    const info = await stat(probe).catch((error: unknown) => {
      throw new Error(`the lease probe path names no file: ${probe} (${String(error)}); build it with cargo build -p bake-conformance in rust/`)
    })
    if (!info.isFile()) throw new Error(`the lease probe path is not a file: ${probe}`)
    if (!existsSync(BUILT_LIB)) throw new Error(`the Node holder needs the built package at ${BUILT_LIB}; run bun run build:runtime`)
    // A usage error must exit 2 with no stdout: proves this is the lease probe and that it runs.
    checkDir = await mkdtemp(join(tmpdir(), 'bake-lease-xrt-check-'))
    const usage = new Child('probe usage check', probe, [], childEnv(checkDir, checkDir), checkDir, 'ignore')
    try {
      const exit = await usage.waitClosed()
      if (usage.spawnError !== undefined || exit.code !== 2 || exit.signal !== null || usage.unread() !== '') {
        throw new Error(`the lease probe path is not a usable lease probe: ${usage.describe()}`)
      }
    } finally {
      await usage.kill()
      children.delete(usage)
    }
  })

  afterAll(async () => {
    if (checkDir !== undefined) await rm(checkDir, { recursive: true, force: true, maxRetries: 5 })
  })

  it('a live Rust holder refuses a TypeScript writer with the exact owned error while reads continue, and its crash permits TypeScript takeover', async () => {
    const at = await area()
    const id = 'xrt-rust-crash'
    const holder = await startHolder(at, 'rust hold', id)
    await expectLog(at.root, id, [0, 1])
    await expectTsRefused(at.root, id)
    await expectLog(at.root, id, [0, 1])
    await crash(holder)
    await tsTakeOver(at.root, id)
  })

  it('a Rust holder\'s graceful release permits a TypeScript writer to append seq 2', async () => {
    const at = await area()
    const id = 'xrt-rust-release'
    const holder = await startHolder(at, 'rust hold', id)
    await expectTsRefused(at.root, id)
    await release(holder)
    await tsTakeOver(at.root, id)
  })

  it('a live Node holder refuses a Rust writer with exit 3 and the exact message, and its crash permits Rust takeover', async () => {
    const at = await area()
    const id = 'xrt-node-crash'
    const holder = await startHolder(at, 'node', id)
    await expectRustRefused(at, id)
    await expectLog(at.root, id, [0, 1])
    await crash(holder)
    await rustTakeOver(at, id)
  })

  it('a Node holder\'s graceful release permits a Rust writer to append seq 2', async () => {
    const at = await area()
    const id = 'xrt-node-release'
    const holder = await startHolder(at, 'node', id)
    await expectRustRefused(at, id)
    await release(holder)
    await rustTakeOver(at, id)
  })

  it('an in-process TypeScript write handle refuses a Rust writer, and its close permits Rust takeover', async () => {
    const at = await area()
    const id = 'xrt-ts-inprocess'
    await withBackend(at.root, async (persistence) => {
      const handle = await persistence.create({ version: SESSION_FORMAT_VERSION, id: SessionId(id), createdAt: 1000, isSeeded: false })
      try {
        await handle.append(FIRST_TURN)
        await handle.flush()
        await expectRustRefused(at, id)
      } finally {
        await handle.close()
      }
    })
    await rustTakeOver(at, id)
  })

  it('a Rust hold-open of a TypeScript-created log refuses TypeScript, and its release permits TypeScript takeover', async () => {
    const at = await area()
    const id = 'xrt-holdopen-release'
    await createTsLog(at.root, id)
    const created = await snapshot(at.root)
    const holder = await startHolder(at, 'rust hold-open', id)
    // hold-open appends nothing.
    expect(await snapshot(at.root)).toEqual(created)
    await expectTsRefused(at.root, id)
    await release(holder)
    await tsTakeOver(at.root, id)
  })

  it('a Rust hold-open of a TypeScript-created log refuses TypeScript, and its crash permits TypeScript takeover', async () => {
    const at = await area()
    const id = 'xrt-holdopen-crash'
    await createTsLog(at.root, id)
    const holder = await startHolder(at, 'rust hold-open', id)
    await expectTsRefused(at.root, id)
    await crash(holder)
    await tsTakeOver(at.root, id)
  })

  // Windows has no SIGSTOP; its live idle holder cases above carry the evidence.
  it.skipIf(!POSIX)('a stopped Rust holder still refuses TypeScript, and only its death permits takeover (POSIX only)', async () => {
    const at = await area()
    const id = 'xrt-rust-stopped'
    const holder = await startHolder(at, 'rust hold', id)
    expect(holder.proc.kill('SIGSTOP')).toBe(true)
    await observeStopped(holder.proc.pid!, at)
    await expectTsRefused(at.root, id)
    await crash(holder)
    await tsTakeOver(at.root, id)
  })

  it.skipIf(!POSIX)('a stopped Node holder still refuses Rust, and only its death permits takeover (POSIX only)', async () => {
    const at = await area()
    const id = 'xrt-node-stopped'
    const holder = await startHolder(at, 'node', id)
    expect(holder.proc.kill('SIGSTOP')).toBe(true)
    await observeStopped(holder.proc.pid!, at)
    await expectRustRefused(at, id)
    await crash(holder)
    await rustTakeOver(at, id)
  })
})
